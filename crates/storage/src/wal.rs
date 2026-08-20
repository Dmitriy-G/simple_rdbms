use std::path::PathBuf;

use common::{Lsn, PageId, TxnId};

use crate::error::StorageError;

/// A single write-ahead log record. Every mutation to a page is logged here
/// before the page itself is flushed (the "write-ahead" rule), which is
/// what makes ARIES-style crash recovery possible: replaying the log
/// reconstructs any committed change that never made it to the data file,
/// and undoes any change from a transaction that never committed.
#[derive(Debug, Clone)]
pub enum LogRecord {
    /// Marks the start of transaction `txn_id`.
    Begin {
        /// The transaction beginning.
        txn_id: TxnId,
    },
    /// Records that `txn_id` modified `page_id`, with enough before/after
    /// image bytes to redo or undo the change.
    Update {
        /// The transaction performing the write.
        txn_id: TxnId,
        /// The page being modified.
        page_id: PageId,
        /// The page's bytes before the change, for undo.
        before: Vec<u8>,
        /// The page's bytes after the change, for redo.
        after: Vec<u8>,
    },
    /// Marks `txn_id` as committed; its updates must survive a crash from
    /// this point on.
    Commit {
        /// The transaction committing.
        txn_id: TxnId,
    },
    /// Marks `txn_id` as aborted; its updates must be undone.
    Abort {
        /// The transaction aborting.
        txn_id: TxnId,
    },
    /// Records that every page dirtied before this point has been flushed,
    /// bounding how far back recovery needs to scan.
    Checkpoint {
        /// Transactions that were still active when the checkpoint was
        /// taken, and so still need redo/undo consideration.
        active_txns: Vec<TxnId>,
    },
}

/// Appends `LogRecord`s to a durable, append-only log file and assigns each
/// one a monotonically increasing `Lsn`. The buffer pool consults the WAL's
/// current LSN before flushing a page (the write-ahead rule: the log record
/// covering a change must be durable before the changed page is).
pub struct WriteAheadLog {
    #[allow(dead_code)]
    path: PathBuf,
    #[allow(dead_code)]
    next_lsn: u64,
}

impl WriteAheadLog {
    /// Opens (creating if necessary) the log file at `path`.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let _ = path;
        todo!("open or create the log file, determine next_lsn from its current contents")
    }

    /// Appends `record` to the log, returning its assigned `Lsn`. Does not
    /// itself guarantee durability; call `flush` to force it to disk.
    pub fn append(&mut self, record: LogRecord) -> Result<Lsn, StorageError> {
        let _ = record;
        todo!("serialize the record, write it at next_lsn, advance next_lsn")
    }

    /// Forces every appended record up to and including `up_to` to disk.
    pub fn flush(&mut self, up_to: Lsn) -> Result<(), StorageError> {
        let _ = up_to;
        todo!("fsync the log file, tracking the durable LSN watermark")
    }

    /// Replays the log for crash recovery, in three ARIES passes: analysis
    /// (determine which transactions and pages were in flight), redo
    /// (reapply every logged update, committed or not), and undo (roll back
    /// updates from transactions that never committed).
    pub fn recover(&mut self) -> Result<(), StorageError> {
        todo!("run the ARIES analysis/redo/undo passes over the log")
    }
}
