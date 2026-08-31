use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DbConfig {
    pub db_path: PathBuf,
    pub page_size: usize,
    pub buffer_pool_size: usize,
    pub checkpoint_byte_threshold: u64,
    pub dwb_capacity: usize,
    pub slow_query_warn_threshold_ms: u64,
    pub lock_wait_timeout_ms: u64,
    pub idle_in_transaction_timeout_ms: u64,
}

impl DbConfig {
    pub const DEFAULT_PAGE_SIZE: usize = 4096;

    pub const DEFAULT_BUFFER_POOL_SIZE: usize = 64;

    pub const DEFAULT_CHECKPOINT_BYTE_THRESHOLD: u64 = 4 * 1024 * 1024;

    pub const DEFAULT_DWB_CAPACITY: usize = 64;

    pub const DEFAULT_SLOW_QUERY_WARN_THRESHOLD_MS: u64 = 100;

    pub const DEFAULT_LOCK_WAIT_TIMEOUT_MS: u64 = 5_000;

    pub const DEFAULT_IDLE_IN_TRANSACTION_TIMEOUT_MS: u64 = 60_000;

    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            page_size: Self::DEFAULT_PAGE_SIZE,
            buffer_pool_size: Self::DEFAULT_BUFFER_POOL_SIZE,
            checkpoint_byte_threshold: Self::DEFAULT_CHECKPOINT_BYTE_THRESHOLD,
            dwb_capacity: Self::DEFAULT_DWB_CAPACITY,
            slow_query_warn_threshold_ms: Self::DEFAULT_SLOW_QUERY_WARN_THRESHOLD_MS,
            lock_wait_timeout_ms: Self::DEFAULT_LOCK_WAIT_TIMEOUT_MS,
            idle_in_transaction_timeout_ms: Self::DEFAULT_IDLE_IN_TRANSACTION_TIMEOUT_MS,
        }
    }
}
