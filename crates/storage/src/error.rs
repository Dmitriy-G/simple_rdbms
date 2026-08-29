use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("disk io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("buffer pool exhausted: no evictable frame available")]
    BufferPoolExhausted,

    #[error("timed out after waiting {waited_ms}ms for a free buffer pool frame")]
    BufferPoolWaitTimedOut { waited_ms: u64 },

    #[error("corrupt page {page_id}: {reason}")]
    CorruptPage { page_id: u32, reason: String },

    #[error("page {0} not found")]
    PageNotFound(u32),

    #[error(
        "database file length {actual} bytes is not a multiple of the {page_size}-byte page \
         size (expected {expected} bytes for a whole number of pages)"
    )]
    TruncatedFile { actual: u64, expected: u64, page_size: usize },

    #[error("tuple of {size} bytes exceeds the {max}-byte maximum for a single page")]
    TupleTooLarge { size: usize, max: usize },

    #[error("index key of {size} bytes exceeds the {max}-byte maximum a single node can hold")]
    KeyTooLarge { size: usize, max: usize },

    #[error("corrupt write-ahead log header: {reason}")]
    CorruptLogHeader { reason: String },

    #[error("checksum mismatch on page {page_id}: expected {expected:#010x}, got {actual:#010x}")]
    ChecksumMismatch { page_id: u32, expected: u32, actual: u32 },

    #[error(
        "double-write restore for page {page_id} did not survive its own write; the batch is \
         left in place for a retry"
    )]
    DoubleWriteRestoreFailed { page_id: u32 },

    #[error("double-write buffer capacity must be at least 1, got 0")]
    InvalidDwbCapacity,

    #[error("index key cannot be encoded: {0}")]
    UnorderableKey(#[from] types::ValueError),

    #[error(
        "in-place update of slot {slot} on page {page_id} would write {patch_len} bytes at \
         offset {offset}, past its {tuple_len}-byte tuple"
    )]
    InPlaceUpdateOutOfBounds {
        page_id: u32,
        slot: u16,
        tuple_len: usize,
        offset: usize,
        patch_len: usize,
    },

    #[error(
        "index node {page_id} would need {required} bytes to hold its rebuilt entries but only \
         has {capacity}; rebuild refused rather than overflow the page"
    )]
    NodeOverflow { page_id: u32, required: usize, capacity: usize },
}

impl From<StorageError> for common::Error {
    fn from(err: StorageError) -> Self {
        let message = err.to_string();
        match err {
            StorageError::Io(err) => common::Error::Io(err),
            StorageError::BufferPoolExhausted => common::Error::BufferPoolExhausted,
            StorageError::BufferPoolWaitTimedOut { waited_ms } => {
                common::Error::BufferPoolWaitTimedOut { waited_ms }
            }
            StorageError::CorruptPage { page_id, reason } => {
                common::Error::CorruptPage { page_id, reason }
            }
            StorageError::PageNotFound(page_id) => common::Error::PageNotFound { page_id },
            StorageError::TruncatedFile { actual, expected, page_size } => {
                common::Error::TruncatedFile { actual, expected, page_size }
            }
            StorageError::TupleTooLarge { size, max } => common::Error::TupleTooLarge { size, max },
            StorageError::KeyTooLarge { size, max } => common::Error::KeyTooLarge { size, max },
            StorageError::CorruptLogHeader { reason } => common::Error::CorruptLog { reason },
            StorageError::ChecksumMismatch { page_id, expected, actual } => {
                common::Error::ChecksumMismatch { page_id, expected, actual }
            }
            StorageError::DoubleWriteRestoreFailed { page_id } => {
                common::Error::DoubleWriteRestoreFailed { page_id }
            }
            StorageError::InvalidDwbCapacity => {
                common::Error::InvalidConfiguration { detail: message }
            }
            StorageError::UnorderableKey(_) => common::Error::DatatypeMismatch { detail: message },
            StorageError::InPlaceUpdateOutOfBounds { .. } => {
                common::Error::Internal { detail: message }
            }
            StorageError::NodeOverflow { .. } => common::Error::Internal { detail: message },
        }
    }
}
