use std::fmt::Debug;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::{BufMut, BytesMut};
use common::{Error, Severity, SqlState};
use engine::{Database, ResultSet, StatementDescription, Tuple, Value};
use futures::{Sink, stream};
use pgwire::api::Type;

use crate::pg_catalog::{self, Introspection, field_info, pg_type_of};
use crate::settings;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::auth::{DefaultServerParameterProvider, ServerParameterProvider, StartupHandler};
use pgwire::api::portal::{Format, Portal};
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{DataRowEncoder, FieldFormat, FieldInfo, QueryResponse, Response, Tag};
use pgwire::api::stmt::QueryParser;
use pgwire::api::store::PortalStore;
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::PgWireBackendMessage;
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
    session: Arc<Mutex<Database>>,
}

impl ConnectionState {
    fn new(session: Database) -> Self {
        Self { session: Arc::new(Mutex::new(session)) }
    }
}

#[derive(Debug, Clone)]
enum PreparedStatement {
    Ordinary { sql: String, description: StatementDescription },
    Introspection { sql: String, columns: Vec<(String, Type)> },
}

struct StatementParser {
    session: Arc<Mutex<Database>>,
}

fn on_session<T>(session: &Mutex<Database>, f: impl FnOnce(&mut Database) -> T) -> T {
    let mut session = session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    tokio::task::block_in_place(|| f(&mut session))
}

#[async_trait]
impl QueryParser for StatementParser {
    type Statement = PreparedStatement;

    async fn parse_sql<C>(
        &self,
        _client: &C,
        sql: &str,
        _types: &[Option<Type>],
    ) -> PgWireResult<Option<Self::Statement>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let outcome = on_session(&self.session, |session| {
            if let Some(introspection) = pg_catalog::answer(session, sql) {
                return Ok(PreparedStatement::Introspection {
                    sql: sql.to_string(),
                    columns: introspection.columns,
                });
            }
            session.describe(sql).map(|description| PreparedStatement::Ordinary {
                sql: sql.to_string(),
                description,
            })
        });
        match outcome {
            Ok(stmt) => Ok(Some(stmt)),
            Err(err) => Err(to_pg_error(&err)),
        }
    }

    fn get_parameter_types(&self, stmt: &Self::Statement) -> PgWireResult<Vec<Type>> {
        match stmt {
            PreparedStatement::Ordinary { description, .. } => {
                Ok(description.param_types.iter().map(|t| pg_type_of(t.as_ref())).collect())
            }
            PreparedStatement::Introspection { .. } => Ok(Vec::new()),
        }
    }

    fn get_result_schema(
        &self,
        stmt: &Self::Statement,
        column_format: Option<&Format>,
    ) -> PgWireResult<Vec<FieldInfo>> {
        match stmt {
            PreparedStatement::Ordinary { description, .. } => Ok(description
                .columns
                .iter()
                .enumerate()
                .map(|(idx, (name, data_type))| {
                    let format = column_format
                        .map(|format| format.format_for(idx))
                        .unwrap_or(FieldFormat::Text);
                    field_info(name, data_type.as_ref(), format)
                })
                .collect()),
            PreparedStatement::Introspection { columns, .. } => Ok(columns
                .iter()
                .enumerate()
                .map(|(idx, (name, data_type))| {
                    let format = column_format
                        .map(|format| format.format_for(idx))
                        .unwrap_or(FieldFormat::Text);
                    FieldInfo::new(name.clone(), None, None, data_type.clone(), format)
                })
                .collect()),
        }
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
        let introspection = on_session(&self.session, |session| pg_catalog::answer(session, query));
        if let Some(introspection) = introspection {
            log_answered_without_the_engine(
                &*client,
                introspection.shape,
                introspection.rows.len(),
                query,
            );
            return Ok(vec![introspection_response(introspection)]);
        }
        if let Some(response) = intercept_set_show_reset(&*client, query) {
            return Ok(vec![response]);
        }
        let result = on_session(&self.session, |session| session.execute(query));
        match result {
            Ok(result_set) => Ok(vec![to_response(query, result_set, &Format::UnifiedText)]),
            Err(err) => Err(to_pg_error(&err)),
        }
    }
}

#[async_trait]
impl ExtendedQueryHandler for ConnectionState {
    type Statement = PreparedStatement;
    type QueryParser = StatementParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        Arc::new(StatementParser { session: Arc::clone(&self.session) })
    }

    async fn do_query<C>(
        &self,
        client: &mut C,
        portal: &Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + ClientPortalStore + Sink<PgWireBackendMessage> + Unpin + Send + Sync,
        C::PortalStore: PortalStore<Statement = Self::Statement>,
        C::Error: Debug,
        PgWireError: From<<C as Sink<PgWireBackendMessage>>::Error>,
    {
        match &portal.statement.statement {
            PreparedStatement::Introspection { sql, .. } => {
                match on_session(&self.session, |session| pg_catalog::answer(session, sql)) {
                    Some(introspection) => {
                        log_answered_without_the_engine(
                            &*client,
                            introspection.shape,
                            introspection.rows.len(),
                            sql,
                        );
                        Ok(introspection_response(introspection))
                    }
                    None => Err(cached_plan_changed()),
                }
            }
            PreparedStatement::Ordinary { sql, description } => {
                let sql = sql.clone();
                let param_types: Vec<Type> = description
                    .param_types
                    .iter()
                    .map(|data_type| pg_type_of(data_type.as_ref()))
                    .collect();

                let mut params = Vec::with_capacity(portal.parameter_len());
                for idx in 0..portal.parameter_len() {
                    let pg_type = param_types.get(idx).cloned().unwrap_or(Type::VARCHAR);
                    params.push(decode_param(portal, idx, &pg_type)?);
                }

                let result =
                    on_session(&self.session, |session| session.execute_with_params(&sql, &params));
                match result {
                    Ok(result_set) => {
                        Ok(to_response(&sql, result_set, &portal.result_column_format))
                    }
                    Err(err) => Err(to_pg_error(&err)),
                }
            }
        }
    }
}

fn cached_plan_changed() -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        SqlState::FEATURE_NOT_SUPPORTED.as_str().to_owned(),
        "cached plan must not change result type: this statement was prepared as a pg_catalog \
         introspection query and the relation it names now exists as a real table"
            .to_owned(),
    )))
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

fn log_answered_without_the_engine<C>(client: &C, shape: &'static str, rows: usize, sql: &str)
where
    C: ClientInfo,
{
    let peer = client.socket_addr();
    tracing::info!(%peer, shape, rows, "pgwire: answered without the engine");
    tracing::debug!(%peer, shape, sql, "pgwire: answered without the engine, full text");
}

fn intercept_set_show_reset<C>(client: &C, query: &str) -> Option<Response>
where
    C: ClientInfo,
{
    let trimmed = query.trim().trim_end_matches(';').trim();
    let mut words = trimmed.split_whitespace();
    let leading = words.next()?;
    let (shape, rows, response) = match leading.to_uppercase().as_str() {
        "SET" => ("SET", 0, Response::Execution(Tag::new("SET"))),
        "RESET" => ("RESET", 0, Response::Execution(Tag::new("RESET"))),
        "SHOW" => {
            let rest: Vec<&str> = words.collect();
            ("SHOW", 1, show_response(client, &settings::parameter_name(&rest)))
        }
        _ => return None,
    };
    log_answered_without_the_engine(client, shape, rows, query);
    Some(response)
}

fn show_response<C>(client: &C, name: &str) -> Response
where
    C: ClientInfo,
{
    let value = settings::lookup(name).map(str::to_string).unwrap_or_else(|| {
        DefaultServerParameterProvider::default()
            .server_parameters(client)
            .and_then(|params| params.get(name).cloned())
            .unwrap_or_default()
    });
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

fn to_response(query: &str, result_set: ResultSet, result_format: &Format) -> Response {
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
                .enumerate()
                .map(|(idx, (name, data_type))| {
                    field_info(name, data_type.as_ref(), result_format.format_for(idx))
                })
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

fn introspection_response(introspection: Introspection) -> Response {
    let fields: Vec<FieldInfo> = introspection
        .columns
        .iter()
        .map(|(name, data_type)| {
            FieldInfo::new(name.clone(), None, None, data_type.clone(), FieldFormat::Text)
        })
        .collect();
    let schema = Arc::new(fields);
    let mut encoder = DataRowEncoder::new(Arc::clone(&schema));
    let data_rows: Vec<PgWireResult<DataRow>> =
        introspection.rows.iter().map(|row| encode_introspection_row(&mut encoder, row)).collect();
    let data_rows = stream::iter(data_rows);
    let mut query_response = QueryResponse::new(schema, data_rows);
    query_response.set_command_tag("SELECT");
    Response::Query(query_response)
}

fn encode_introspection_row(
    encoder: &mut DataRowEncoder,
    row: &[Option<String>],
) -> PgWireResult<DataRow> {
    for cell in row {
        match cell {
            Some(value) => encoder.encode_field(value)?,
            None => encoder.encode_field(&None::<&str>)?,
        }
    }
    Ok(encoder.take_row())
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

fn decode_param(
    portal: &Portal<PreparedStatement>,
    idx: usize,
    pg_type: &Type,
) -> PgWireResult<Value> {
    decode_param_bytes(portal, idx, pg_type).map_err(|err| parameter_decode_error(idx, err))
}

fn decode_param_bytes(
    portal: &Portal<PreparedStatement>,
    idx: usize,
    pg_type: &Type,
) -> PgWireResult<Value> {
    match *pg_type {
        Type::BOOL => {
            Ok(portal.parameter::<bool>(idx, pg_type)?.map_or(Value::Null, Value::Boolean))
        }
        Type::INT4 => {
            Ok(portal.parameter::<i32>(idx, pg_type)?.map_or(Value::Null, Value::Integer))
        }
        Type::INT8 => Ok(portal.parameter::<i64>(idx, pg_type)?.map_or(Value::Null, Value::BigInt)),
        Type::FLOAT8 => {
            Ok(portal.parameter::<f64>(idx, pg_type)?.map_or(Value::Null, Value::Double))
        }
        _ => {
            Ok(portal.parameter::<String>(idx, &Type::VARCHAR)?.map_or(Value::Null, Value::Varchar))
        }
    }
}

fn parameter_decode_error(idx: usize, err: PgWireError) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_owned(),
        SqlState::PROTOCOL_VIOLATION.as_str().to_owned(),
        format!("parameter ${} could not be decoded: {err}", idx + 1),
    )))
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

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.inner.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        self.inner.clone()
    }
}
