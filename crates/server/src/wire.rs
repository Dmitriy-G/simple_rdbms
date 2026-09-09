use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use engine::{DataType, Database, ResultSet, Tuple, Value};
use futures::stream;
use pgwire::api::Type;
use pgwire::api::auth::StartupHandler;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::query::SimpleQueryHandler;
use pgwire::api::results::{FieldFormat, FieldInfo, QueryResponse, Response, Tag};
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
    async fn do_query<C>(&self, _client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
    {
        let result = {
            let mut session =
                self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            tokio::task::block_in_place(|| session.execute(query))
        };
        match result {
            Ok(result_set) => Ok(vec![to_response(query, result_set)]),
            Err(err) => Err(PgWireError::UserError(Box::new(ErrorInfo::new(
                "ERROR".to_owned(),
                "XX000".to_owned(),
                err.to_string(),
            )))),
        }
    }
}

fn to_response(query: &str, result_set: ResultSet) -> Response {
    match result_set {
        ResultSet::RolledBack => Response::Execution(Tag::new("ROLLBACK")),
        ResultSet::RowsAffected(count) => {
            let keyword = statement_keyword(query);
            let tag = if keyword == "INSERT" {
                Tag::new("INSERT 0").with_rows(count)
            } else {
                Tag::new(&keyword)
            };
            Response::Execution(tag)
        }
        ResultSet::Rows { columns, column_types, rows } => {
            let keyword = statement_keyword(query);
            let command_tag = if keyword == "EXPLAIN" { "EXPLAIN" } else { "SELECT" };
            let fields: Vec<FieldInfo> = columns
                .iter()
                .zip(column_types.iter())
                .map(|(name, data_type)| field_info(name, data_type.as_ref()))
                .collect();
            let schema = Arc::new(fields);
            let data_rows = stream::iter(rows.into_iter().map(|tuple| Ok(encode_data_row(&tuple))));
            let mut query_response = QueryResponse::new(schema, data_rows);
            query_response.set_command_tag(command_tag);
            Response::Query(query_response)
        }
    }
}

fn statement_keyword(sql: &str) -> String {
    let mut words = sql.split_whitespace();
    let first = words.next().unwrap_or("").to_uppercase();
    if first == "CREATE" {
        if let Some(second) = words.next() {
            return format!("CREATE {}", second.to_uppercase());
        }
    }
    first
}

fn field_info(name: &str, data_type: Option<&DataType>) -> FieldInfo {
    let pg_type = match data_type {
        Some(DataType::Boolean) => Type::BOOL,
        Some(DataType::Integer) => Type::INT4,
        Some(DataType::BigInt) => Type::INT8,
        Some(DataType::Double) => Type::FLOAT8,
        Some(DataType::Varchar(_)) | None => Type::VARCHAR,
    };
    FieldInfo::new(name.to_string(), None, None, pg_type, FieldFormat::Text)
}

fn encode_data_row(tuple: &Tuple) -> DataRow {
    let values = tuple.values();
    let mut buf = BytesMut::new();
    for value in values {
        match encode_value_text(value) {
            Some(text) => {
                buf.put_i32(text.len() as i32);
                buf.put_slice(text.as_bytes());
            }
            None => buf.put_i32(-1),
        }
    }
    DataRow::new(buf, values.len() as i16)
}

fn encode_value_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Boolean(b) => Some(if *b { "t".to_string() } else { "f".to_string() }),
        Value::Integer(v) => Some(v.to_string()),
        Value::BigInt(v) => Some(v.to_string()),
        Value::Double(v) => Some(v.to_string()),
        Value::Varchar(s) => Some(s.clone()),
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
