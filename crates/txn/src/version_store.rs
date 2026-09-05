use std::collections::HashMap;
use std::sync::Mutex;

use common::sync::recover_lock;
use common::{Rid, TxnId};

use crate::mvcc::{VersionChain, VersionEntry};

#[derive(Debug, Default)]
pub struct VersionStore {
    chains: Mutex<HashMap<Rid, VersionChain>>,
}

impl VersionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_insert(&self, txn_id: TxnId, rid: Rid, tuple_bytes: Vec<u8>) {
        let mut chains = recover_lock(self.chains.lock(), "VersionStore.chains");
        chains.entry(rid).or_default().push(VersionEntry {
            creator_txn_id: txn_id,
            begin_ts: None,
            end_ts: None,
            tuple_bytes,
        });
    }

    pub fn commit_versions(&self, txn_id: TxnId, commit_ts: u64) {
        let mut chains = recover_lock(self.chains.lock(), "VersionStore.chains");
        for chain in chains.values_mut() {
            chain.mark_committed(txn_id, commit_ts);
        }
    }

    pub fn abort_versions(&self, txn_id: TxnId) {
        let mut chains = recover_lock(self.chains.lock(), "VersionStore.chains");
        for chain in chains.values_mut() {
            chain.remove_creator(txn_id);
        }
    }

    pub fn is_visible(&self, rid: Rid, read_ts: u64) -> bool {
        let chains = recover_lock(self.chains.lock(), "VersionStore.chains");
        match chains.get(&rid) {
            Some(chain) => chain.visible_version(read_ts).is_some(),
            None => true,
        }
    }

    pub fn is_visible_to(&self, rid: Rid, reader_txn_id: TxnId, read_ts: u64) -> bool {
        let chains = recover_lock(self.chains.lock(), "VersionStore.chains");
        match chains.get(&rid) {
            Some(chain) => chain.visible_version_for(reader_txn_id, read_ts).is_some(),
            None => true,
        }
    }
}
