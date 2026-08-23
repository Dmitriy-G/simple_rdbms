use std::path::PathBuf;

/// Top-level configuration for opening a database: where its files live on
/// disk and the sizing knobs for the storage engine. Constructed by the
/// `engine` facade (or the `cli`) and threaded down into the disk manager
/// and buffer pool.
#[derive(Debug, Clone)]
pub struct DbConfig {
    /// Path to the main database file on disk.
    pub db_path: PathBuf,
    /// Size in bytes of a single page. Fixed for the lifetime of a database
    /// file; all pages, including the WAL's, use this size.
    pub page_size: usize,
    /// Number of frames the buffer pool holds in memory.
    pub buffer_pool_size: usize,
    /// A checkpoint is triggered once this many bytes have been appended to
    /// the write-ahead log since the last one, bounding how much of the log
    /// a future recovery's Analysis pass has to scan.
    pub checkpoint_byte_threshold: u64,
    /// The number of page-image slots the double-write buffer's
    /// `<db_path>.dwb` file holds, capping how many dirty pages
    /// `BufferPool::flush_all` can cover in a single batch before it must
    /// split into more than one.
    pub dwb_capacity: usize,
}

impl DbConfig {
    /// The default on-disk page size: 4 KiB, matching common OS page sizes.
    pub const DEFAULT_PAGE_SIZE: usize = 4096;

    /// The default number of buffer pool frames.
    pub const DEFAULT_BUFFER_POOL_SIZE: usize = 64;

    /// The default checkpoint trigger: 4 MiB of log growth.
    pub const DEFAULT_CHECKPOINT_BYTE_THRESHOLD: u64 = 4 * 1024 * 1024;

    /// The default double-write buffer capacity: 64 page-image slots.
    pub const DEFAULT_DWB_CAPACITY: usize = 64;

    /// Builds a config pointing at `db_path` with default sizing.
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            page_size: Self::DEFAULT_PAGE_SIZE,
            buffer_pool_size: Self::DEFAULT_BUFFER_POOL_SIZE,
            checkpoint_byte_threshold: Self::DEFAULT_CHECKPOINT_BYTE_THRESHOLD,
            dwb_capacity: Self::DEFAULT_DWB_CAPACITY,
        }
    }
}
