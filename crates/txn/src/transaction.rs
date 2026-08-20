use common::{Lsn, TxnId};

use crate::isolation::IsolationLevel;

/// A transaction's position in the two-phase locking protocol: it may only
/// acquire new locks while `Growing`, and once it releases any lock (moving
/// to `Shrinking`) it may never acquire another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    /// Acquiring locks; may not yet have released any.
    Growing,
    /// Has released at least one lock; may not acquire any more.
    Shrinking,
    /// Committed; all of its writes are durable.
    Committed,
    /// Aborted; all of its writes have been (or are being) undone.
    Aborted,
}

/// A single unit of work: the state the `TransactionManager` and
/// `LockManager` track for one in-flight (or finished) transaction.
#[derive(Debug, Clone)]
pub struct Transaction {
    /// The transaction's unique id, assigned at `begin`.
    pub txn_id: TxnId,
    /// The isolation level this transaction runs under.
    pub isolation_level: IsolationLevel,
    /// The transaction's current position in the 2PL protocol.
    pub state: TransactionState,
    /// The LSN of this transaction's most recent log record, used to walk
    /// its undo chain on abort.
    pub last_lsn: Option<Lsn>,
}

impl Transaction {
    /// Begins a new transaction with the given id and isolation level, in
    /// the `Growing` state.
    pub fn new(txn_id: TxnId, isolation_level: IsolationLevel) -> Self {
        Self { txn_id, isolation_level, state: TransactionState::Growing, last_lsn: None }
    }
}
