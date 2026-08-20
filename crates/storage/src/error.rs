use thiserror::Error;

/// Errors raised by the storage engine: disk I/O, buffer pool exhaustion,
/// corrupt page layout, or WAL failures.
#[derive(Debug, Error)]
pub enum StorageError {
    /// The underlying file could not be read from or written to.
    #[error("disk io error: {0}")]
    Io(#[from] std::io::Error),

    /// The buffer pool has no free frame and every resident page is pinned,
    /// so a new page cannot be brought into memory.
    #[error("buffer pool exhausted: no evictable frame available")]
    BufferPoolExhausted,

    /// A page's on-disk bytes failed a structural sanity check (bad magic,
    /// truncated slot array, checksum mismatch, etc.).
    #[error("corrupt page {page_id}: {reason}")]
    CorruptPage {
        /// The offending page, identified by its raw page number.
        page_id: u32,
        /// A human-readable description of what failed validation.
        reason: String,
    },

    /// The requested page does not exist in the database file.
    #[error("page {0} not found")]
    PageNotFound(u32),
}

impl From<StorageError> for common::Error {
    fn from(err: StorageError) -> Self {
        common::Error::Storage(err.to_string())
    }
}
