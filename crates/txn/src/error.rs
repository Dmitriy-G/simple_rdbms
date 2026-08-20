use thiserror::Error;

/// Errors raised by the transaction subsystem.
#[derive(Debug, Error)]
pub enum TxnError {
    /// Two transactions requested conflicting locks on the same resource in
    /// a way that cannot be resolved without one of them waiting forever;
    /// the detector broke the cycle by choosing this transaction as the
    /// victim.
    #[error("deadlock detected, transaction {0} aborted")]
    DeadlockVictim(u64),

    /// A transaction tried to acquire a lock after entering its shrinking
    /// phase, violating two-phase locking.
    #[error("transaction {0} may not acquire new locks after releasing one")]
    LockAfterUnlock(u64),

    /// An operation referenced a transaction id the manager has no record
    /// of (never began, or already finished and was reaped).
    #[error("unknown transaction {0}")]
    UnknownTransaction(u64),
}

impl From<TxnError> for common::Error {
    fn from(err: TxnError) -> Self {
        common::Error::Transaction(err.to_string())
    }
}
