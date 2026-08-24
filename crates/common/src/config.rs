use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DbConfig {
    pub db_path: PathBuf,
    pub page_size: usize,
    pub buffer_pool_size: usize,
    pub checkpoint_byte_threshold: u64,
    pub dwb_capacity: usize,
    pub slow_query_warn_threshold_ms: u64,
}

impl DbConfig {
    pub const DEFAULT_PAGE_SIZE: usize = 4096;

    pub const DEFAULT_BUFFER_POOL_SIZE: usize = 64;

    pub const DEFAULT_CHECKPOINT_BYTE_THRESHOLD: u64 = 4 * 1024 * 1024;

    pub const DEFAULT_DWB_CAPACITY: usize = 64;

    pub const DEFAULT_SLOW_QUERY_WARN_THRESHOLD_MS: u64 = 100;

    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            page_size: Self::DEFAULT_PAGE_SIZE,
            buffer_pool_size: Self::DEFAULT_BUFFER_POOL_SIZE,
            checkpoint_byte_threshold: Self::DEFAULT_CHECKPOINT_BYTE_THRESHOLD,
            dwb_capacity: Self::DEFAULT_DWB_CAPACITY,
            slow_query_warn_threshold_ms: Self::DEFAULT_SLOW_QUERY_WARN_THRESHOLD_MS,
        }
    }
}
