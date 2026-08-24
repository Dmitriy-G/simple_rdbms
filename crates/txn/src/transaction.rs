use common::TxnId;

use crate::isolation::IsolationLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Growing,
    Shrinking,
    Committed,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub txn_id: TxnId,
    pub isolation_level: IsolationLevel,
    pub state: TransactionState,
}

impl Transaction {
    pub fn new(txn_id: TxnId, isolation_level: IsolationLevel) -> Self {
        Self { txn_id, isolation_level, state: TransactionState::Growing }
    }
}
