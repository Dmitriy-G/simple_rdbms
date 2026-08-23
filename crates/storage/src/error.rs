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

    /// The database file's length is not a whole multiple of its page size,
    /// so the last page on disk is partially written rather than a complete
    /// page. `set_len` (the sole durable act of allocation) is a single
    /// syscall, so this should only arise from damage outside the engine's
    /// own writes - but it must still be reported clearly rather than
    /// silently truncating or rounding.
    #[error(
        "database file length {actual} bytes is not a multiple of the {page_size}-byte page \
         size (expected {expected} bytes for a whole number of pages)"
    )]
    TruncatedFile {
        /// The file's actual length, in bytes.
        actual: u64,
        /// The largest whole-page-multiple length not exceeding `actual`.
        expected: u64,
        /// The configured page size, in bytes.
        page_size: usize,
    },

    /// A tuple's encoded bytes are larger than a single page can ever hold,
    /// even empty, so no amount of retrying onto a fresh page would help.
    #[error("tuple of {size} bytes exceeds the {max}-byte maximum for a single page")]
    TupleTooLarge {
        /// The tuple's encoded size, in bytes.
        size: usize,
        /// The largest tuple a single empty page can hold.
        max: usize,
    },

    /// The write-ahead log's own header (the fixed magic bytes every log
    /// file starts with) failed a structural sanity check: bad magic, or a
    /// file shorter than the header itself.
    #[error("corrupt write-ahead log header: {reason}")]
    CorruptLogHeader {
        /// A human-readable description of what failed validation.
        reason: String,
    },

    /// A page's stored CRC-32 checksum (`page::CHECKSUM_RANGE`) does not
    /// match the checksum recomputed over its bytes at read time - the
    /// page's on-disk bytes were altered by something other than
    /// `DiskManager::write_page` (media corruption, a torn write, disk
    /// bit rot). Detection only: recovering from this by reconstructing the
    /// page from a double-write buffer is a following task, not this one.
    #[error("checksum mismatch on page {page_id}: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch {
        /// The offending page, identified by its raw page number.
        page_id: u32,
        /// The checksum stored in the page's `CHECKSUM_RANGE`.
        expected: u32,
        /// The checksum recomputed over the page's `4..PAGE_SIZE` bytes.
        actual: u32,
    },
}

impl From<StorageError> for common::Error {
    fn from(err: StorageError) -> Self {
        common::Error::Storage(err.to_string())
    }
}
