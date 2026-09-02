use thiserror::Error;

#[derive(Debug, Error)]
pub enum TxnError {
    #[error("deadlock detected, transaction {0} aborted")]
    DeadlockVictim(u64),

    #[error(
        "transaction {0} may not acquire new locks after release_all was already called for it"
    )]
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
            TxnError::DeadlockVictim(_) => common::Error::DeadlockDetected { detail },
            TxnError::LockAfterUnlock(_) | TxnError::UnknownTransaction(_) => {
                common::Error::Internal { detail }
            }
            TxnError::Storage(err) => err.into(),
        }
    }
}
