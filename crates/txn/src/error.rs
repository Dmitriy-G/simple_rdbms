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
        let detail = err.to_string();
        match err {
            TxnError::DeadlockVictim(_) => common::Error::SerializationFailure { detail },
            TxnError::LockAfterUnlock(_) | TxnError::UnknownTransaction(_) => {
                common::Error::Internal { detail }
            }
            TxnError::Storage(err) => err.into(),
        }
    }
}
