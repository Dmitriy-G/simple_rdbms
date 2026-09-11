# dispatch_never_blocks

Regression coverage for P-14 (found reviewing M10.2 subtask 1): the
dispatch thread in `crates/engine/src/runtime.rs` must
never block on a lock a worker thread might be holding for a statement's
whole duration, since that turns one slow session's `SELECT` into a
stall for every other connected session. Unlike `tests/sessions.rs`,
which drives its concurrency through real wall-clock sleeps, this file
uses a device-level pause so the "long-running `SELECT`" in its test is
genuinely stuck - not just slow - for as long as the test wants, which is
what lets it assert responsiveness without a timing-dependent race: if
the dispatch thread were still blocking on the held lock, the probe
request below would hang until the pause is released rather than merely
arrive a bit late.

It now also covers P-53/P-55, which is the same property one layer down:
the catalog itself must never hold a lock that a statement's length can
stretch. **The requirement pulls against `catalog_reload_race.MD`'s**, and
the two files have to be read together - that suite needs a reload to
exclude every concurrent catalog mutation, and the first attempt at giving
it that excluded readers too, which is precisely the stall this file
asserts against.
`docs/adr/0014-catalog-is-a-live-shared-object.md` is the design that
satisfies both: a mutation `Mutex` for `catalog_reload_race`, and a state
`RwLock` held only for one lookup for this file. Break either apart and
one of the two suites fails; which one it is tells you which half was
broken.

It now also covers P-54: `EngineMessage::Checkpoint`/`BestEffortFlush`
used to run `EngineShared::checkpoint_and_flush`/`best_effort_flush`
inline on the dispatch thread despite both doing real I/O (a page flush),
so a stalled flush froze every other session's `Connect`/`Execute` the
same way a stalled lock did for P-14. `dispatch_checkpoint`/
`dispatch_best_effort_flush` (`runtime.MD`) now submit that work to the
worker pool instead, and
`a_stalled_checkpoint_does_not_block_an_unrelated_session`
below is the regression test, pausing a *write* rather than a read since
that is where a checkpoint's flush actually blocks.

**That test covers two fixes, and it is worth knowing why, because the
first one alone did not make it pass.** Moving the arm to a worker frees
the dispatch thread, so `Connect` and `table_names()` are answered while
the checkpoint is stuck. A `SELECT` was not: a checkpoint also held
`EngineShared.txn_manager` across `write_checkpoint`'s page-0 flush, and
`EngineShared::txn_for_statement` needs that same mutex to begin any
statement's transaction, so the probe parked one layer below the layer
that had just been fixed. `docs/adr/0016-a-checkpoint-is-not-a-quiesce-point.md`
is the second fix - the checkpoint's page flush moved out from under the
lock - and the probe below asserts all three requests together for that
reason. If it ever fails again, which request timed out says which of the
two halves regressed: `Connect` means the dispatch thread, the `SELECT`
alone means the transaction manager. Widening its timeout is never the
answer; the wait it would be tolerating is unbounded, not slow.

It now also covers P-63, which is ADR 0016's defect on the abort path:
`EngineShared::abort_txn` held `EngineShared.txn_manager` across
`TransactionManager::abort`, and the undo inside it fetches every page the
transaction wrote, so a fetch that found no free frame evicted a victim and
wrote and synced the database file with that mutex still held. Every
session's `BEGIN`, `COMMIT` and first statement stopped for the length of
one `ROLLBACK`.
`docs/adr/0018-an-abort-is-not-a-quiesce-point.md` splits the abort the way
ADR 0016 split the checkpoint, and
`a_stalled_rollback_does_not_block_an_unrelated_session` below is the
regression test. The two stalled-write tests are deliberately the same
shape, and the difference between them is where the write comes from: the
checkpoint's is one the test forces directly, the rollback's is an eviction
the test has to engineer with a small pool and a transaction too large to
fit in it.

## Key Components

- `REACHED_TIMEOUT`/`RESPONSIVENESS_BOUND`/`PAUSE_SAFETY_NET` - the three
  bounds the test's blocking waits are held to, per `CLAUDE.md`'s
  concurrency-test discipline: `REACHED_TIMEOUT` bounds waiting for the
  paused `SELECT` to actually reach its blocked read (generous - this is
  just proving setup worked, not the property under test),
  `RESPONSIVENESS_BOUND` is the actual assertion (tight - a regression
  that reintroduces dispatch-thread blocking must fail this, not merely
  slow it down), and `PAUSE_SAFETY_NET` bounds the device's own internal
  wait so a broken test can't hang the suite forever even if the test
  function itself never reaches its release send. Both tests share all
  three: each probes with in-memory dispatch-thread work only, so
  `RESPONSIVENESS_BOUND` is the right order of magnitude for any of them,
  and
  a bound that has to be widened to pass is the signal that the probe is
  waiting on a lock rather than on a scheduler.
- `ProbeStep` - what
  `a_stalled_rollback_does_not_block_an_unrelated_session`'s probe thread
  reports after each of its three requests, so the test waits on each one
  under its own `RESPONSIVENESS_BOUND` and a timeout says which request
  blocked. The other two tests send one message carrying every result at
  once, which is enough for them because each has a single suspect; this
  one has three and the distinction between them is the whole diagnosis.
  It is not a refinement worth backporting until one of those tests
  actually fails.
- `PauseSlot` - one arming's worth of synchronization: the `reached`
  `SyncSender` the device signals when it parks, and the `release`
  `Receiver` it then blocks on. The device holds a `VecDeque` of these and
  consumes one per arming, which is what lets a single test pause two
  different threads in sequence at two different reads.
- `PausingDevice` - wraps a real `FileDevice` and independently arms a
  read pause and a write pause, each its own `Arc<AtomicBool>` plus
  `Mutex`-wrapped `VecDeque<PauseSlot>` (`read_armed`/`read_slots`,
  `write_armed`/`write_slots`) so a test can pick which operation it needs
  to intercept without one pause's arming affecting the other.
  `PausingDevice::new(inner, read_armed, read_slots)` builds one with only
  read pausing live (write pausing disarmed and empty), for
  `a_paused_select_does_not_block_an_unrelated_sessions_request`'s device
  reads; `PausingDevice::with_write_pause(inner, write_armed, write_slots)`
  is the mirror image, used by both
  `a_stalled_checkpoint_does_not_block_an_unrelated_session` and
  `a_stalled_rollback_does_not_block_an_unrelated_session`, which need to
  intercept a page write - a checkpoint's flush in the first, an undo's
  eviction in the second - rather than a read. `pause_if_armed(armed, slots)` is the shared body both `read_at`
  and `write_at` call: if armed (checked via `swap(false, ...)`, so only
  the *next* call after each arming pauses), it pops the front
  `PauseSlot`, signals that slot's `reached`, and blocks on its `release`
  (bounded by `PAUSE_SAFETY_NET`) before the caller delegates to the
  wrapped device. Every other `BlockDevice` method delegates straight
  through unpaused.
- `recv_within`/`row_count` - the same private helpers as
  `tests/sessions.rs` (`sessions.MD`), duplicated rather than shared
  since `tests/support/` is for helpers more than one file actually needs
  and this is currently the only other file using them.
- `a_paused_select_does_not_block_an_unrelated_sessions_request` - opens a
  database with a tiny `buffer_pool_size` (2 frames, so a page written
  during setup is evicted by the time the paused scan runs and really
  does need a disk read to intercept) through `PausingDevice`, creates a
  table spanning several pages, and commits unarmed so setup itself never
  pauses. Then: a second session (`db2`) opens an explicit transaction and
  is left idle; the device is armed; a third session (`db_scan`) starts
  `SELECT * FROM t` on its own thread, which blocks inside the paused read
  mid-statement; the test blocks on `scan_reached_rx` until it knows the
  scan is genuinely stuck there, not just about to run. The device is then
  **armed a second time** and `db2` is dropped, exercising the `Disconnect`
  path P-14 named: its `reload_catalog` runs on a worker, takes the
  catalog's mutation lock and parks in the second `PauseSlot`, and the test
  waits on `reload_reached_rx` before probing. That second wait is the
  point of the two-slot device - it makes the probe demonstrably run while
  a reload is *known* to be holding the catalog's mutation lock, where
  waiting only for the disconnect to be "in flight" left it a race whether
  the reload had reached the catalog at all. (Before ADR 0014 the suite
  passed for that reason and not on merit: with a two-frame pool the reload
  happened to block inside `Catalog::open` before ever reaching the write
  lock, so a faster open would have stalled the same probe.) A fourth
  session's `table_names()` is then sent on its own thread and must come
  back within `RESPONSIVENESS_BOUND` - proven by disabling the fix locally
  while
  writing this test: doing so made `disconnect` call
  `disconnect_session` inline instead of through `WorkerPool::submit`,
  and the probe request then hung past every bound in this file (in fact
  past the whole test binary's own default per-test timeout), because the
  dispatch thread was blocked behind the still-paused scan and never got to
  processing the probe's `Execute` message at all.
  Only after the probe request is confirmed to have returned does the test
  release both paused reads - the reload's first, then the scan's - and
  join the scan thread, asserting it still completed with the correct row
  count - the
  fix must not change what the paused statement itself sees, only stop it
  from starving every other session while it runs.
- `a_stalled_checkpoint_does_not_block_an_unrelated_session` -
  opens a database through a `PausingDevice::with_write_pause` at the
  default `buffer_pool_size` (the pause is on a write this test forces,
  not on an eviction it has to engineer), creates a table and commits one
  row unarmed, opens a second session (`db_probe_base`) that stays alive
  across the whole test, then arms the write pause and calls
  `Database::close` (which sends `Checkpoint`) on its own thread. The
  first page write that checkpoint issues is page 0's header flush inside
  `write_checkpoint`; it parks there, and the test waits on
  `flush_reached_rx` until it knows the flush is genuinely stuck. Only
  then does it open a *third* session from `db_probe_base` and, on its own
  thread, ask for `table_names()` and run `SELECT * FROM t`, asserting all
  three - the `Connect`, the lookup and the statement - come back within
  `RESPONSIVENESS_BOUND`. It checks the answers, not just the timing: the
  name list must really contain `t`, since `Database::table_names` returns
  an empty `Vec` on error, and the `SELECT` must return its one row, since
  a statement that was merely *dispatched* would satisfy a timing
  assertion on its own. Before the P-54 fix, `dispatch_checkpoint` did not
  exist and `dispatch_message` ran `checkpoint_and_flush` to completion
  inline, so the dispatch thread was parked in the write and never reached
  the probe's `Connect` at all; before ADR 0016 the `Connect` and the
  lookup passed and the `SELECT` hung on `EngineShared.txn_manager`. Both
  hangs were unbounded, not slow. The probe thread
  hands both its sessions back to the test rather than dropping them. That
  was once load-bearing - `Drop for Database` called `best_effort_flush`,
  which would have queued behind the very flush the test is holding - and
  since `docs/adr/0020-closing-a-connection-is-not-a-flush-point.md` it is
  merely tidy: dropping a session now sends `Disconnect` and waits for
  nothing. It is kept because handing the sessions back also keeps them
  alive past the assertions, which is what stops the engine shutting down
  underneath a probe that has not finished.
  Finally the pause is released and the checkpoint
  thread is joined on its `Result`, confirming the checkpoint completes
  rather than being abandoned mid-write.
- `a_stalled_rollback_does_not_block_an_unrelated_session` - the same
  shape as the checkpoint test above, with the stall moved onto the abort
  path. It opens through a `PausingDevice::with_write_pause` at
  `buffer_pool_size` 8, creates `big` and a one-row `small`, then opens an
  explicit transaction that inserts 80 rows of ~1800 bytes each into `big`
  - roughly forty pages, five times the pool, so the undo cannot walk its
  chain backwards without evicting. All of that happens unarmed, so setup's
  own evictions never pause. The write pause is then armed and `ROLLBACK`
  runs on its own thread: the undo's first eviction parks in `flush_pages`,
  and the test waits on `evict_reached_rx` until it knows the rollback is
  genuinely stuck inside a device write. Only then does a second session's
  thread `Connect`, `BEGIN` and `COMMIT`, reporting each step on its own
  `ProbeStep` message so each is waited for under its own
  `RESPONSIVENESS_BOUND`. `BEGIN` and `COMMIT` are the assertion:
  `handle_begin` and `commit_txn` both take `EngineShared.txn_manager`,
  which is the exact mutex P-63 was about, and before the fix `BEGIN` hung
  on it - unboundedly, not slowly, so widening the bound is never the
  answer. After the release the rollback thread is joined on its `Result`,
  the probe's session runs a `SELECT` to show it is still usable, and
  `big` is re-read: it must be empty, since a fix that let the probe
  through by weakening the undo would otherwise pass.

  **The probe deliberately runs no data statement inside the timed
  window, and that is a real limit on what this test can assert.** A
  `SELECT` was tried there first and timed out at step 2 with `Connect`
  and `BEGIN` both passing comfortably - which is what identified the
  cause rather than a flaw in the fix. `BufferPool::flush_pages` holds
  `BufferPool.flush_sequence` for the whole of its double-write batch,
  page writes and `sync`, so while a device write is stalled no session
  anywhere can evict a *dirty* frame. This test's writer fills all 8
  frames with dirty `big` pages before the device is armed, which leaves
  the pool with nothing to reclaim for free: a probe reading a table whose
  page is not already resident genuinely needs a page written before it can
  read one, no matter what the transaction manager is doing. Pinning the
  page in the pool first does not survive contact with the replacer
  either: `LruKReplacer` evicts every frame with fewer than `k` recorded
  accesses before any frame with `k`, so a page read once to warm it is
  the *first* victim the undo takes, not the last. The checkpoint test
  above gets away with a `SELECT` only because its default-size pool never
  needs to evict at all.

  That serialization is its own property, one layer below this file's, and
  `docs/adr/0019-one-double-write-batch-at-a-time.md` is where it is now
  settled: one batch at a time is the design, and what the pool owes a
  caller instead is that an eviction needing no flush is never queued
  behind one that does, and that an eviction that does need one gives up
  at `frame_wait_timeout`. Neither changes this test. The properties
  themselves are held by
  `crates/storage/tests/buffer_pool_flush_races.rs`'s
  `an_eviction_prefers_a_clean_victim_while_a_flush_is_in_flight` and
  `a_fetch_needing_a_dirty_victim_gives_up_at_the_frame_wait_timeout`,
  where the pool's whole dirty set is controllable and the assertion can
  be a counter rather than a clock - which is why the strong-form probe
  was not added here.
- `wal_base_path(dir) -> PathBuf` - private helper, identical in shape to
  `tests/sessions.rs`'s helper of the same name, building the WAL's base
  path for `Database::open_with_devices`.

## Usage Example

```rust
let armed = Arc::new(AtomicBool::new(false));
let (reached_tx, reached_rx) = mpsc::sync_channel(0);
let (release_tx, release_rx) = mpsc::sync_channel(0);
let slots = VecDeque::from(vec![PauseSlot { reached: reached_tx, release: release_rx }]);
let device = PausingDevice::new(FileDevice::new(file), Arc::clone(&armed), slots);
armed.store(true, Ordering::SeqCst);
// ... spawn a statement that reads through `device` ...
reached_rx.recv().unwrap();
// ... assert some other session's request is still fast here ...
release_tx.send(()).unwrap();
```

`cargo test -p engine --test dispatch_never_blocks` runs this file alone;
it needs the `test-util` feature (enabled automatically for `cargo test`
via this crate's `[dev-dependencies]`) for `Database::open_with_devices`.
Run under `taskset -c 0,1` per `CLAUDE.md`'s concurrency-test discipline,
the same as `tests/sessions.rs`'s concurrent tests.
