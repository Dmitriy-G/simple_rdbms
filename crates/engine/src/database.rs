use catalog::Schema;
#[cfg(feature = "test-util")]
use common::TxnId;
use common::{DbConfig, Result};
use types::Value;

use crate::result_set::ResultSet;
#[cfg(feature = "test-util")]
use crate::runtime::EngineStats;
use crate::runtime::{EngineHandle, SessionHandle};
use crate::statement_description::StatementDescription;

const DEFAULT_DATABASE_NAME: &str = "postgres";

fn database_name_from(config: &DbConfig) -> String {
    config
        .db_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| DEFAULT_DATABASE_NAME.to_string())
}

pub struct Database {
    session: SessionHandle,
    database_name: String,
}

impl Database {
    pub fn open(config: DbConfig) -> Result<Self> {
        Self::open_impl(config).inspect_err(|err| {
            tracing::error!(sql_state = %err.sql_state(), %err, "failed to open database");
        })
    }

    fn open_impl(config: DbConfig) -> Result<Self> {
        let database_name = database_name_from(&config);
        let engine = EngineHandle::open(&config)?;
        let session = engine.connect()?;
        Ok(Self { session, database_name })
    }

    #[cfg(feature = "test-util")]
    pub fn open_with_devices(
        config: DbConfig,
        db_device: Box<dyn storage::block_device::BlockDevice>,
        wal_store: std::sync::Arc<dyn storage::wal::SegmentStore>,
        wal_segment_size: u64,
        dwb_device: Box<dyn storage::block_device::BlockDevice>,
    ) -> Result<Self> {
        let database_name = database_name_from(&config);
        let engine = EngineHandle::open_with_devices(
            &config,
            db_device,
            wal_store,
            wal_segment_size,
            dwb_device,
        )?;
        let session = engine.connect()?;
        Ok(Self { session, database_name })
    }

    pub fn connect(&self) -> Result<Self> {
        Ok(Self { session: self.session.connect()?, database_name: self.database_name.clone() })
    }

    pub fn database_name(&self) -> &str {
        &self.database_name
    }

    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        self.session.execute(sql)
    }

    pub fn execute_with_params(&mut self, sql: &str, params: &[Value]) -> Result<ResultSet> {
        self.session.execute_with_params(sql, params)
    }

    pub fn describe(&self, sql: &str) -> Result<StatementDescription> {
        self.session.describe(sql)
    }

    pub fn close(self) -> Result<()> {
        self.session.checkpoint_and_flush()
    }

    pub fn table_names(&self) -> Vec<String> {
        self.session.table_names().unwrap_or_default()
    }

    pub fn table_schema(&self, name: &str) -> Result<Schema> {
        self.session.table_schema(name)
    }

    #[cfg(feature = "test-util")]
    pub fn kill_engine_for_test(&self) {
        self.session.kill_engine_for_test();
    }

    #[cfg(feature = "test-util")]
    pub fn stats(&self) -> Result<EngineStats> {
        self.session.stats()
    }

    #[cfg(feature = "test-util")]
    pub fn current_txn_id(&self) -> Result<Option<TxnId>> {
        self.session.current_txn_id()
    }

    #[cfg(feature = "test-util")]
    pub fn lock_count_for_txn(&self, txn_id: TxnId) -> Result<usize> {
        self.session.lock_count_for_txn(txn_id)
    }
}
