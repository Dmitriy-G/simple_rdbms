# btree_crash_injection

The crash-injection sweep for `btree::BTreeIndex::insert`, in the same
shape `crates/engine/tests/crash_injection.MD` documents for SQL
workloads: for a fixed, deterministic sequence of inserts, and for every
possible point a write could fail, opens the tree on a `FaultyDevice`
armed to fail at write `n`, runs the workload until it dies, reopens the
same files on a real device (running `recovery::recover` as part of
`open_recovered_pool`), and requires the result to be consistent with
*exactly* the prefix of inserts that were safely committed before the
fault struck: `check_invariants` passes, every committed key is present,
and no key past the committed prefix is.

This file exists because M9.2's task description explicitly calls for it:
the engine has no `CREATE INDEX` until M9.3, so there is no way to drive
this workload through `engine::Database`/SQL the way
`crates/engine/tests/crash_injection.rs` drives its own workloads - this
file drives `BTreeIndex` directly against a `BufferPool`, the same way
`tests/btree.rs` and `tests/recovery.rs` do, just with fault-injecting
devices underneath. See `crash_injection.MD`'s "Durability models"
section for what each of the four `block_device::DurabilityModel`
compositions this file also sweeps under actually simulates; that
reasoning is identical here; it does not depend on anything SQL-specific.

Unlike the SQL-driven harness, this tree has no catalog to remember where
its root page lives after a restart - that bookkeeping is M9.3's job, not
this milestone's. `BufferPool::set_catalog_first_page`, an existing,
already-crash-safe, WAL-logged page-0 header field, is reused purely as a
durable scratch slot for "the current root `PageId`," written under the
same transaction as the insert that produced it so the two commit or fail
together; nothing about the catalog itself is exercised by this file.

## Key Components

- `DeviceTriple`, `open_file`, `faulty_devices` - the same shape
  `crash_injection.rs` uses: a shared fault counter across the database
  file, WAL, and double-write buffer, so "fail at write N" counts across
  the whole system.
- `open_recovered_pool(db_device, wal_device, dwb_device) -> Result<BufferPool, StorageError>` -
  the storage-level equivalent of `Database::open_with_devices`: builds a
  `DiskManager`/`LogManager`/`DoubleWriteBuffer` directly on the given
  devices, runs `recovery::recover_double_write` then
  `recovery::recover`, and returns the recovered `BufferPool` - the exact
  three-step sequence `engine::Database::open_with_managers` documents,
  minus the catalog load step this crate has no reason to perform.
- `workload_keys() -> Vec<Vec<u8>>` - twelve distinct ~600-byte keys.
  Deliberately large relative to the 4KB page size: a leaf holds only
  six or seven such entries, so this short workload still forces a real
  split (confirmed directly by `workload_keys_actually_force_a_root_split`)
  without needing hundreds of inserts, keeping the sweep - which reopens
  the database once per write point - fast.
- `commit(pool, txn_id) -> Result<(), StorageError>` - `append_log(Commit)`,
  `flush_log` up to it, `append_log(End)`: the same sequence
  `txn::TransactionManager::commit` uses, reproduced here directly since
  `storage` cannot depend on `txn` (see `CLAUDE.md`'s dependency-edge
  rules - `txn` depends on `storage`, not the other way around).
- `run_until_crash(db_device, wal_device, dwb_device, keys) -> usize` -
  opens the tree (creating it if this is the first generation), then
  inserts `keys` one at a time, each under its own transaction, committing
  after each. Returns how many keys were fully committed - including
  their root-pointer update - before either an insert or its commit
  failed, or the workload ran out of keys. An error opening the pool at
  all (the fault can strike during `DiskManager`/`LogManager`/
  `DoubleWriteBuffer`'s own setup writes, before a single insert runs)
  counts as zero keys committed, not a panic.
- `assert_workload_is_crash_safe(model) -> Result<(), Box<dyn Error>>` -
  the sweep itself: measures the unfaulted workload's total write count,
  then for every `fail_at` from `1` to that count, runs the workload
  against fault-injecting devices, attempts (and discards the result of)
  two more fault-injected reopens to prove recovery itself is resumable,
  then reopens cleanly and checks the recovered tree. A durable root
  pointer is only *required* to exist when at least one key was reported
  committed - the tree's own creation can succeed and become durable on
  its own, with every subsequent insert then failing before it commits,
  which is why the assertion is one-directional rather than an
  equivalence (`workload_keys_actually_force_a_root_split`'s own
  existence as a separate check exists to keep this file honest about the
  "root split" part of that promise instead of leaving it implicit in the
  sweep's plumbing).
- `workload_keys_actually_force_a_root_split` - a non-fault-injected
  sanity check, run once: confirms `BTreeIndex::root_page_id()` actually
  changes after inserting `workload_keys()`, and that the result passes
  `check_invariants`. Exists so a future change to `workload_keys` that
  accidentally stops forcing a split fails loudly here, rather than the
  crash-sweep tests below silently degrading into testing single-leaf
  durability instead of the split path they are named for.
- The four `enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write*`
  tests each call `assert_workload_is_crash_safe` under one
  `DurabilityModel`; names encode which.

## Usage Example

```rust
#[test]
fn enough_inserts_to_force_a_root_split_survive_a_crash_at_every_write_with_torn_writes()
-> Result<(), Box<dyn Error>> {
    assert_workload_is_crash_safe(DurabilityModel::torn_write())
}
```

Run as part of `cargo test --workspace`, or just this file with
`cargo test -p storage --test btree_crash_injection`. Like
`crash_injection.rs`, each sweep reopens a temp-dir-backed database once
per write point per `DurabilityModel`, so this is one of the slower files
in the suite but is what actually proves a split's several page writes
recover as a consistent tree no matter where a crash lands among them,
rather than only in the no-crash case `tests/btree.rs` covers.
