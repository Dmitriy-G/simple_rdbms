use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use common::{Error, Severity};
use engine::{DataType, Database, ResultSet, Tuple, Value};
use futures::stream;
use pgwire::api::Type;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::auth::{DefaultServerParameterProvider, ServerParameterProvider, StartupHandler};
use pgwire::api::query::SimpleQueryHandler;
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::store::PortalStore;
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::data::DataRow;
use pgwire::tokio::process_socket;
use tokio::net::TcpListener;

pub async fn serve(listener: TcpListener, db: Arc<Database>) {
    loop {
        let (socket, peer_addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(%err, "pgwire: failed to accept a connection");
                continue;
            }
        };
        let db = Arc::clone(&db);
        tokio::spawn(async move {
            let session = match tokio::task::block_in_place(|| db.connect()) {
                Ok(session) => session,
                Err(err) => {
                    tracing::warn!(
                        %peer_addr,
                        %err,
                        "pgwire: failed to open a session for a new connection"
                    );
                    return;
                }
            };
            let handlers = ConnectionHandlers { inner: Arc::new(ConnectionState::new(session)) };
            if let Err(err) = process_socket(socket, None, handlers).await {
                tracing::debug!(%peer_addr, %err, "pgwire: connection ended with an I/O error");
            }
        });
    }
}

struct ConnectionState {
    session: Mutex<Database>,
}

impl ConnectionState {
    fn new(session: Database) -> Self {
        Self { session: Mutex::new(session) }
    }
}

impl NoopStartupHandler for ConnectionState {}

#[async_trait]
impl SimpleQueryHandler for ConnectionState {
    async fn do_query<C>(&self, client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
    {
        if let Some(response) = intercept_set_show_reset(&*client, query) {
            return Ok(vec![response]);
        }
        let result = {
            let mut session =
                self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            tokio::task::block_in_place(|| session.execute(query))
        };
        match result {
            Ok(result_set) => Ok(vec![to_response(query, result_set)]),
            Err(err) => Err(to_pg_error(&err)),
        }
    }
}

fn to_pg_error(err: &Error) -> PgWireError {
    let severity = match err.severity() {
        Severity::Error => "ERROR",
        Severity::Fatal => "FATAL",
        Severity::Panic => "PANIC",
    };
    PgWireError::UserError(Box::new(ErrorInfo::new(
        severity.to_owned(),
        err.sql_state().as_str().to_owned(),
        err.to_string(),
    )))
}

fn intercept_set_show_reset<C>(client: &C, query: &str) -> Option<Response>
where
    C: ClientInfo,
{
    let trimmed = query.trim().trim_end_matches(';').trim();
    let mut words = trimmed.split_whitespace();
    let leading = words.next()?;
    match leading.to_uppercase().as_str() {
        "SET" => Some(Response::Execution(Tag::new("SET"))),
        "RESET" => Some(Response::Execution(Tag::new("RESET"))),
        "SHOW" => Some(show_response(client, words.next().unwrap_or(""))),
        _ => None,
    }
}

fn show_response<C>(client: &C, name: &str) -> Response
where
    C: ClientInfo,
{
    let value = DefaultServerParameterProvider::default()
        .server_parameters(client)
        .and_then(|params| params.get(name).cloned())
        .unwrap_or_default();
    let fields =
        vec![FieldInfo::new(name.to_owned(), None, None, Type::VARCHAR, FieldFormat::Text)];
    let schema = Arc::new(fields);
    let mut buf = BytesMut::new();
    buf.put_i32(value.len() as i32);
    buf.put_slice(value.as_bytes());
    let data_row = DataRow::new(buf, 1);
    let data_rows = stream::iter(std::iter::once(Ok(data_row)));
    let mut query_response = QueryResponse::new(schema, data_rows);
    query_response.set_command_tag("SHOW");
    Response::Query(query_response)
}

fn to_response(query: &str, result_set: ResultSet) -> Response {
    match result_set {
        ResultSet::RolledBack => Response::TransactionEnd(Tag::new("ROLLBACK")),
        ResultSet::RowsAffected(count) => {
            let keyword = statement_keyword(query);
            match keyword.as_str() {
                "INSERT" => Response::Execution(Tag::new("INSERT 0").with_rows(count)),
                "BEGIN" => Response::TransactionStart(Tag::new("BEGIN")),
                "COMMIT" => Response::TransactionEnd(Tag::new("COMMIT")),
                _ => Response::Execution(Tag::new(&keyword)),
            }
        }
        ResultSet::Rows { columns, column_types, rows } => {
            let keyword = statement_keyword(query);
            let command_tag = if keyword == "EXPLAIN" { "EXPLAIN" } else { "SELECT" };
            let fields: Vec<FieldInfo> = columns
                .iter()
                .zip(column_types.iter())
                .map(|(name, data_type)| field_info(name, data_type.as_ref(), FieldFormat::Text))
                .collect();
            let schema = Arc::new(fields);
            let mut encoder = DataRowEncoder::new(Arc::clone(&schema));
            let data_rows: Vec<PgWireResult<DataRow>> =
                rows.iter().map(|tuple| encode_data_row(&mut encoder, tuple)).collect();
            let data_rows = stream::iter(data_rows);
            let mut query_response = QueryResponse::new(schema, data_rows);
            query_response.set_command_tag(command_tag);
            Response::Query(query_response)
        }
    }
}

fn statement_keyword(sql: &str) -> String {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let mut words = trimmed.split_whitespace();
    let first = words.next().unwrap_or("").to_uppercase();
    if first == "CREATE" {
        if let Some(second) = words.next() {
            return format!("CREATE {}", second.to_uppercase());
        }
    }
    first
}

fn field_info(name: &str, data_type: Option<&DataType>, format: FieldFormat) -> FieldInfo {
    let pg_type = match data_type {
        Some(DataType::Boolean) => Type::BOOL,
        Some(DataType::Integer) => Type::INT4,
        Some(DataType::BigInt) => Type::INT8,
        Some(DataType::Double) => Type::FLOAT8,
        Some(DataType::Varchar(_)) | None => Type::VARCHAR,
    };
    FieldInfo::new(name.to_string(), None, None, pg_type, format)
}

fn encode_data_row(encoder: &mut DataRowEncoder, tuple: &Tuple) -> PgWireResult<DataRow> {
    for value in tuple.values() {
        encode_field(encoder, value)?;
    }
    Ok(encoder.take_row())
}

fn encode_field(encoder: &mut DataRowEncoder, value: &Value) -> PgWireResult<()> {
    match value {
        Value::Null => encoder.encode_field(&None::<i8>),
        Value::Boolean(v) => encoder.encode_field(v),
        Value::Integer(v) => encoder.encode_field(v),
        Value::BigInt(v) => encoder.encode_field(v),
        Value::Double(v) => encoder.encode_field(v),
        Value::Varchar(v) => encoder.encode_field(v),
    }
}

struct ConnectionHandlers {
    inner: Arc<ConnectionState>,
}

impl PgWireServerHandlers for ConnectionHandlers {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.inner.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        self.inner.clone()
    }
}
