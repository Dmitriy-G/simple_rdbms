use std::collections::HashMap;

use common::{Rid, TxnId};

use crate::error::TxnError;

/// The mode a lock is held in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// Multiple transactions may hold a shared lock on the same resource
    /// simultaneously; used for reads.
    Shared,
    /// Only one transaction may hold an exclusive lock on a resource at a
    /// time, and it excludes shared locks too; used for writes.
    Exclusive,
}

/// Grants and releases row-level locks under strict two-phase locking,
/// blocking (conceptually; the actual wait/wound policy is future work)
/// conflicting requests and detecting deadlocks among waiters.
#[derive(Debug, Default)]
pub struct LockManager {
    #[allow(dead_code)]
    holders: HashMap<Rid, Vec<(TxnId, LockMode)>>,
}

impl LockManager {
    /// Creates a lock manager with no locks held.
    pub fn new() -> Self {
        Self { holders: HashMap::new() }
    }

    /// Acquires a lock on `rid` in `mode` on behalf of `txn_id`, blocking
    /// (or erroring, once deadlock detection exists) if it conflicts with
    /// an incompatible lock already held by another transaction.
    pub fn lock(&mut self, txn_id: TxnId, rid: Rid, mode: LockMode) -> Result<(), TxnError> {
        let _ = (txn_id, rid, mode);
        todo!("check for conflicting holders, block or record the grant, upgrade S->X in place")
    }

    /// Releases `txn_id`'s lock on `rid`. Under strict 2PL this only
    /// happens at commit/abort, never mid-transaction.
    pub fn unlock(&mut self, txn_id: TxnId, rid: Rid) -> Result<(), TxnError> {
        let _ = (txn_id, rid);
        todo!("remove txn_id's entry from holders[rid], waking any blocked waiters")
    }
}
