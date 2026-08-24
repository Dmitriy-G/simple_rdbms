use thiserror::Error;

#[derive(Debug, Error)]
pub enum TxnError {
    #[error("deadlock detected, transaction {0} aborted")]
    DeadlockVictim(u64),

    #[error("transaction {0} may not acquire new locks after releasing one")]
    LockAfterUnlock(u64),

    #[error("unknown transaction {0}")]
    UnknownTransaction(u64),

    #[error(transparent)]
    Storage(#[from] storage::StorageError),
}

impl From<TxnError> for common::Error {
    fn from(err: TxnError) -> Self {
        common::Error::Transaction(err.to_string())
    }
}
