# ADR 0020: Closing a connection is not a flush point

Date: 2026-09-11

Status: Accepted

## Context

**This section describes the tree as M13.2 left it**; its references are
historical, and the Decision below is what the code should do.

`engine::Database` is two things wearing one type. `Database::open`
returns the handle that owns the engine — the `Arc<runtime::EngineHandle>`,
the engine dispatch thread, the worker pool, the buffer pool, the log.
`Database::connect` returns a handle to one *session* on that same
engine, sharing every one of those. Nothing in the type tells the two
apart, and `Drop` cannot tell them apart either: it runs the same code
for both.

That `Drop` called `SessionHandle::best_effort_flush`, which sends
`EngineMessage::BestEffortFlush` and blocks the dropping thread on the
reply while a worker thread runs `EngineShared::best_effort_flush` —
`BufferPool::flush_log_all` followed by `BufferPool::flush_all`, which is
every dirty frame in the pool pushed through the double-write buffer with
its three `fsync`s.

That was written when a process held exactly one `Database` and dropped
it once, at exit, where flushing the whole pool is exactly the right
thing to do. M13.1 and M13.2 made one `Database` per TCP connection —
`server::wire` calls `db.connect()` for every accepted socket — and a
whole-engine action stayed attached to what had become a per-connection
handle. A client that connects, runs a statement or two and disconnects
(`psql` from a shell, an unpooled JDBC/ODBC client, a health probe, a
script in a loop) pays for a full pool flush on the way out, regardless of
which pages it touched or whether it wrote anything at all. By ADR 0019
`BufferPool::flush_pages` deliberately holds `BufferPool.flush_sequence`
across its syncs, so while that flush runs no other session can evict a
dirty frame: N disconnecting connections serialize the whole engine
behind N full flushes.

Nothing about it was *incorrect*, and that is the point worth being
precise about. Durability here belongs to the write-ahead log: a
committed transaction's records are already flushed under
`EngineShared::commit_txn`, and an uncommitted one is undone by recovery.
The drop-time flush was never a durability guarantee — it does not even
`sync()` the data file, which only `Database::close`'s
`checkpoint_and_flush` does — so what it buys is a shorter redo at the
next startup, and what it cost was a whole-database stall on the most
ordinary path a server has. This is a liveness and throughput decision,
not a durability one.

The question it forces is the one M13.1 was meant to answer and only
half did: now that a handle no longer means a process, where does
engine-wide work belong?

## Decision

**A connection closing is not a flush point. The engine going away is.**

- **`Drop for Database` flushes nothing.** The implementation is removed;
  what remains is the ordinary field drop of its `SessionHandle`, which
  sends `EngineMessage::Disconnect` as it always did. A per-connection
  drop therefore costs one message send and no wait.
- **`Disconnect` is the whole of per-connection cleanup.** `disconnect`
  removes the session from the engine's map and submits
  `disconnect_session` to the worker pool, which aborts the session's open
  transaction and reloads the catalog if there was one. That work is
  bounded by what the one session actually did, and it is asynchronous —
  the dropping thread does not wait for it, which is what makes a
  disconnect cheap for the socket task that triggers it.
- **The engine-wide best-effort flush moves to `Drop for EngineHandle`.**
  That destructor already runs exactly once, when the last
  `Arc<EngineHandle>` in the process goes, and it already owns the
  shutdown sequence. It sends `BestEffortFlush` and waits for the reply
  *before* taking the sender and joining the engine thread, so the flush
  still completes on a live engine. `EngineMessage::BestEffortFlush`,
  `SessionHandle::best_effort_flush`, `dispatch_best_effort_flush` and
  `EngineShared::best_effort_flush` keep their present behaviour,
  poisoning check included; only the caller moves.
- **`Database::close` is unchanged.** An explicit close still checkpoints,
  flushes and syncs, and still returns its error, because a caller that
  asked has somewhere to report a failure to and a destructor does not.
- **Steady-state flushing stays where it already is.**
  `EngineShared::maybe_checkpoint`, on the statement path, is what bounds
  how much log a long-running server leaves to redo. A process that never
  exits is covered by the checkpoint threshold, never by its clients
  hanging up.

**Why this loses nothing.** Three things could go wrong and none of them
can:

- **No committed transaction depends on it.** Every `COMMIT` flushes the
  log before it returns; redo replays from the log whatever the buffer
  pool had not written home. A flush at disconnect only ever moved a page
  to its home location sooner.
- **No uncommitted transaction depends on it either.** A session dropped
  mid-transaction is aborted by `disconnect_session`, whose undo is logged
  as CLRs like any other, and a crash before that abort finishes is
  recovered as a loser. This is strictly better ordered than what it
  replaces: the old `Drop` sent `BestEffortFlush` *before* the field drop
  sent `Disconnect`, so the flush it performed could not have contained
  that session's undo anyway.
- **Process exit behaves exactly as before.** Today the last `Database` to
  drop flushes and then drops the `EngineHandle`; after this change the
  last `Database` to drop drops the `EngineHandle`, which flushes. The
  same flush happens at the same point in the same thread. What
  disappears is the other N−1 flushes.

**Alternatives considered.**

- **Drop the flush entirely and rely on `close()`.** The narrower change,
  and tempting because both binaries call `close()` on their normal path.
  Rejected: the paths that do *not* call it are real — `server`'s
  `Arc::try_unwrap` fallback when a connection outlives the shutdown
  signal, a test that lets a `Database` fall out of scope, an embedder
  that panics — and for those the last-handle flush is what keeps the next
  startup's redo short. It costs one flush per process, at the one place
  the flush was always meant for.
- **Flush only the pages the closing connection dirtied.** Rejected: the
  buffer pool tracks no per-session dirty set, and building one would buy
  a cheaper flush on a path that should be doing none.
- **Keep the flush in `Drop for Database` but skip it unless
  `Arc::strong_count` is 1.** Rejected: it reaches the same behaviour
  through a racy test, in the wrong destructor. `Drop for EngineHandle`
  *is* the "last one out" hook, and using it needs no count at all.

## Consequences

`Drop for Database` disappears, so `Database`'s destructor stops blocking:
dropping a session is a send, not a round trip through the worker pool.
`crates/engine/src/database.MD` and `crates/engine/src/runtime.MD` both
document the old arrangement in detail and are rewritten with the code, as
are the two paragraphs in `crates/storage/src/buffer.MD` that cite
"`Drop for Database`'s best-effort flush" as a caller of `flush_pages`.

`crates/engine/tests/dispatch_never_blocks.rs` has a comment-level
dependency on the old behaviour — its probe thread hands its sessions back
to the test rather than dropping them, because dropping one would have
queued behind the very flush the test is holding. That workaround becomes
unnecessary; it stays correct either way, and the `.MD` paragraph
explaining it is what must change.

`server`'s shutdown warning, which says it is "relying on the best-effort
flush on drop", stays true and becomes more precise: the flush it means is
now the engine's, at the last connection's exit.

The regression this needs is a counting test, not a timing one:
`EngineStats` gains a count of engine-wide flushes, and a test in
`crates/engine/tests/` connects and drops a number of sessions and asserts
that count does not grow with them. It is the same shape, and exists for
the same reason, as
`crates/engine/tests/sessions.rs`'s
`several_sessions_checkpoint_once_per_threshold_not_once_each`: an
engine-wide cost that quietly multiplied by the number of sessions is a
mistake this codebase has now made twice, and asserting on a counter is
the only way to notice the third time.

The general rule, which the next engine-wide action inherits without a
further ADR: **work whose scope is the engine belongs to the handle whose
lifetime is the engine.** ADR 0014 and ADR 0016 kept engine-wide I/O out
of locks a statement needs; this one keeps it out of a path a client
triggers.
