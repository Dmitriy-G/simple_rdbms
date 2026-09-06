use std::collections::HashMap;
use std::sync::Mutex;

use common::sync::recover_lock;
use common::{Rid, TxnId};

use crate::mvcc::{VersionChain, VersionEntry};

#[derive(Debug, Default)]
struct StoreState {
    chains: HashMap<Rid, VersionChain>,
    write_sets: HashMap<TxnId, Vec<Rid>>,
}

#[derive(Debug, Default)]
pub struct VersionStore {
    state: Mutex<StoreState>,
}

impl VersionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_insert(&self, txn_id: TxnId, rid: Rid) {
        let mut state = recover_lock(self.state.lock(), "VersionStore.state");
        state.chains.entry(rid).or_default().push(VersionEntry {
            creator_txn_id: txn_id,
            begin_ts: None,
            end_ts: None,
        });
        state.write_sets.entry(txn_id).or_default().push(rid);
    }

    pub fn commit_versions(&self, txn_id: TxnId, commit_ts: u64) {
        let mut state = recover_lock(self.state.lock(), "VersionStore.state");
        let Some(rids) = state.write_sets.remove(&txn_id) else {
            return;
        };
        for rid in rids {
            if let Some(chain) = state.chains.get_mut(&rid) {
                chain.mark_committed(txn_id, commit_ts);
            }
        }
    }

    pub fn abort_versions(&self, txn_id: TxnId) {
        let mut state = recover_lock(self.state.lock(), "VersionStore.state");
        let Some(rids) = state.write_sets.remove(&txn_id) else {
            return;
        };
        for rid in rids {
            if let Some(chain) = state.chains.get_mut(&rid) {
                chain.remove_creator(txn_id);
            }
        }
    }

    pub fn is_visible(&self, rid: Rid, read_ts: u64) -> bool {
        let state = recover_lock(self.state.lock(), "VersionStore.state");
        match state.chains.get(&rid) {
            Some(chain) => chain.visible_version(read_ts).is_some(),
            None => true,
        }
    }

    pub fn is_visible_to(&self, rid: Rid, reader_txn_id: TxnId, read_ts: u64) -> bool {
        let state = recover_lock(self.state.lock(), "VersionStore.state");
        match state.chains.get(&rid) {
            Some(chain) => chain.visible_version_for(reader_txn_id, read_ts).is_some(),
            None => true,
        }
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn chain_count(&self) -> usize {
        let state = recover_lock(self.state.lock(), "VersionStore.state");
        state.chains.len()
    }
}
