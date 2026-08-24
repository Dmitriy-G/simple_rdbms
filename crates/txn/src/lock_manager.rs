use std::collections::HashMap;

use common::{Rid, TxnId};

use crate::error::TxnError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Default)]
pub struct LockManager {
    #[allow(dead_code)]
    holders: HashMap<Rid, Vec<(TxnId, LockMode)>>,
}

impl LockManager {
    pub fn new() -> Self {
        Self { holders: HashMap::new() }
    }

    pub fn lock(&mut self, txn_id: TxnId, rid: Rid, mode: LockMode) -> Result<(), TxnError> {
        let _ = (txn_id, rid, mode);
        todo!("check for conflicting holders, block or record the grant, upgrade S->X in place")
    }

    pub fn unlock(&mut self, txn_id: TxnId, rid: Rid) -> Result<(), TxnError> {
        let _ = (txn_id, rid);
        todo!("remove txn_id's entry from holders[rid], waking any blocked waiters")
    }
}
