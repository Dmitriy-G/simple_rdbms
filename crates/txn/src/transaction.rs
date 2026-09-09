use common::{Lsn, TxnId};

use crate::isolation::IsolationLevel;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionState {
    Growing,
    Aborted,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    pub txn_id: TxnId,
    pub isolation_level: IsolationLevel,
    pub state: TransactionState,
    pub begin_lsn: Lsn,
    pub read_ts: u64,
}

impl Transaction {
    pub fn new(
        txn_id: TxnId,
        isolation_level: IsolationLevel,
        begin_lsn: Lsn,
        read_ts: u64,
    ) -> Self {
        Self { txn_id, isolation_level, state: TransactionState::Growing, begin_lsn, read_ts }
    }
}
