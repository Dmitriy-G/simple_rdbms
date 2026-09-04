use std::collections::{HashMap, HashSet};
use std::sync::{Condvar, Mutex};

use common::sync::recover_lock;
use common::{Rid, TableId, TxnId};

use crate::error::TxnError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

impl LockMode {
    fn conflicts_with(self, other: LockMode) -> bool {
        !matches!((self, other), (LockMode::Shared, LockMode::Shared))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Resource {
    Row(Rid),
    Table(TableId),
}

#[derive(Debug, Default)]
struct State {
    holders: HashMap<Resource, Vec<(TxnId, LockMode)>>,
    held_by: HashMap<TxnId, HashSet<Resource>>,
    waiting_for: HashMap<TxnId, (Resource, LockMode)>,
    finished: HashSet<TxnId>,
}

fn conflicting_holders(
    state: &State,
    resource: Resource,
    mode: LockMode,
    exclude: TxnId,
) -> Vec<TxnId> {
    state
        .holders
        .get(&resource)
        .into_iter()
        .flatten()
        .filter(|(id, held_mode)| *id != exclude && held_mode.conflicts_with(mode))
        .map(|(id, _)| *id)
        .collect()
}

fn would_deadlock(state: &State, start: TxnId, blockers: &[TxnId]) -> bool {
    let mut stack = blockers.to_vec();
    let mut visited = HashSet::new();
    while let Some(txn_id) = stack.pop() {
        if txn_id == start {
            return true;
        }
        if !visited.insert(txn_id) {
            continue;
        }
        if let Some(&(resource, mode)) = state.waiting_for.get(&txn_id) {
            stack.extend(conflicting_holders(state, resource, mode, txn_id));
        }
    }
    false
}

#[derive(Debug, Default)]
pub struct LockManager {
    state: Mutex<State>,
    released: Condvar,
}

impl LockManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock(&self, txn_id: TxnId, rid: Rid, mode: LockMode) -> Result<(), TxnError> {
        self.acquire(txn_id, Resource::Row(rid), mode)
    }

    pub fn lock_table(
        &self,
        txn_id: TxnId,
        table_id: TableId,
        mode: LockMode,
    ) -> Result<(), TxnError> {
        self.acquire(txn_id, Resource::Table(table_id), mode)
    }

    pub fn release_all(&self, txn_id: TxnId) {
        let mut state = recover_lock(self.state.lock(), "LockManager.state");
        if let Some(resources) = state.held_by.remove(&txn_id) {
            for resource in resources {
                if let Some(holders) = state.holders.get_mut(&resource) {
                    holders.retain(|(id, _)| *id != txn_id);
                    if holders.is_empty() {
                        state.holders.remove(&resource);
                    }
                }
            }
        }
        state.waiting_for.remove(&txn_id);
        state.finished.insert(txn_id);
        self.released.notify_all();
    }

    #[cfg(any(test, feature = "test-util"))]
    pub fn held_lock_count(&self, txn_id: TxnId) -> usize {
        let state = recover_lock(self.state.lock(), "LockManager.state");
        state.held_by.get(&txn_id).map_or(0, HashSet::len)
    }

    fn acquire(&self, txn_id: TxnId, resource: Resource, mode: LockMode) -> Result<(), TxnError> {
        let mut state = recover_lock(self.state.lock(), "LockManager.state");
        if state.finished.contains(&txn_id) {
            return Err(TxnError::LockAfterUnlock(txn_id.0));
        }
        loop {
            state.waiting_for.remove(&txn_id);

            let held_mode = state
                .holders
                .get(&resource)
                .into_iter()
                .flatten()
                .find(|(id, _)| *id == txn_id)
                .map(|(_, held_mode)| *held_mode);
            if held_mode == Some(LockMode::Exclusive) || held_mode == Some(mode) {
                return Ok(());
            }

            let blockers = conflicting_holders(&state, resource, mode, txn_id);
            if blockers.is_empty() {
                let holders = state.holders.entry(resource).or_default();
                holders.retain(|(id, _)| *id != txn_id);
                holders.push((txn_id, mode));
                state.held_by.entry(txn_id).or_default().insert(resource);
                return Ok(());
            }

            if would_deadlock(&state, txn_id, &blockers) {
                return Err(TxnError::DeadlockVictim(txn_id.0));
            }

            state.waiting_for.insert(txn_id, (resource, mode));
            state = recover_lock(self.released.wait(state), "LockManager.state");
        }
    }
}
