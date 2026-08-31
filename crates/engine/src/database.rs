use catalog::Schema;
use common::{DbConfig, Result};

use crate::result_set::ResultSet;
#[cfg(feature = "test-util")]
use crate::runtime::EngineStats;
use crate::runtime::{EngineHandle, SessionHandle};

pub struct Database {
    session: SessionHandle,
}

impl Database {
    pub fn open(config: DbConfig) -> Result<Self> {
        Self::open_impl(config).inspect_err(|err| {
            tracing::error!(sql_state = %err.sql_state(), %err, "failed to open database");
        })
    }

    fn open_impl(config: DbConfig) -> Result<Self> {
        let engine = EngineHandle::open(&config)?;
        let session = engine.connect()?;
        Ok(Self { session })
    }

    #[cfg(feature = "test-util")]
    pub fn open_with_devices(
        config: DbConfig,
        db_device: Box<dyn storage::block_device::BlockDevice>,
        wal_store: std::sync::Arc<dyn storage::wal::SegmentStore>,
        wal_segment_size: u64,
        dwb_device: Box<dyn storage::block_device::BlockDevice>,
    ) -> Result<Self> {
        let engine = EngineHandle::open_with_devices(
            &config,
            db_device,
            wal_store,
            wal_segment_size,
            dwb_device,
        )?;
        let session = engine.connect()?;
        Ok(Self { session })
    }

    pub fn connect(&self) -> Result<Self> {
        Ok(Self { session: self.session.connect()? })
    }

    pub fn execute(&mut self, sql: &str) -> Result<ResultSet> {
        self.session.execute(sql)
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
}

impl Drop for Database {
    fn drop(&mut self) {
        self.session.best_effort_flush();
    }
}
