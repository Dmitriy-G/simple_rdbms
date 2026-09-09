use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use engine::Database;
use pgwire::api::auth::StartupHandler;
use pgwire::api::auth::noop::NoopStartupHandler;
use pgwire::api::query::SimpleQueryHandler;
use pgwire::api::results::Response;
use pgwire::api::store::PortalStore;
use pgwire::api::{ClientInfo, ClientPortalStore, PgWireServerHandlers};
use pgwire::error::PgWireResult;
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
    async fn do_query<C>(&self, _client: &mut C, _query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + ClientPortalStore + Unpin + Send + Sync,
        C::PortalStore: PortalStore,
    {
        let _guard = self.session.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(vec![Response::EmptyQuery])
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
