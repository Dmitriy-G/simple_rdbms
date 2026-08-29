# ADR 0011: The write-ahead log is a family of numbered segment files, not one growing file

Date: 2026-08-29

Status: Accepted

## Context

`LogManager` owned a single `<db_path>.wal` file. `open` read the *entire*
file into a `Vec<u8>` (`vec![0u8; device_len]`) to find the append point and
rebuild `last_lsn_by_txn`; `iter_from` did the same for its tail. Startup
memory and startup time were both proportional to the log's total history,
not to the work since the last checkpoint - a database that ran for months
without ever restarting would take longer and longer to reopen even though
nothing about a single reopen should depend on how long ago the database was
created. Worse, nothing ever reclaimed old log bytes: `write_checkpoint`
recorded `last_checkpoint_lsn` in the page-0 header but no code ever used it
to discard anything below it, so the `.wal` file only ever grew. Both of
these are blockers for the project's stated goal of a long-running server in
a container (`docs/ROADMAP.md`), where "restart the container" is routine
and "the log file grows forever" is a guaranteed eventual disk-full.

LSN is a byte offset into the log stream (`wal.MD`), and `prev_lsn`,
`undo_next_lsn`, and `last_checkpoint_lsn` all depend on that meaning holding
across a reopen. Simply truncating the front of one growing file was not an
option: every LSN after the truncated prefix would have to shift, and every
chain pointer already written referring to an LSN by value would silently
point at the wrong byte.

## Decision

**The log becomes a family of files, `<path>.NNNNNN` (a zero-padded, 1-based
counter appended to the original path), each covering a contiguous range of
the same global, ever-increasing LSN space.** A directory-per-database
layout was the other option on the table; the flat family was chosen because
every existing call site already passes a `path` that is a plain file
path (not a directory) and every existing crash-injection device wrapper
already opens *one* file at a caller-chosen path - a family of sibling files
keeps that shape and needs no caller to learn a new directory convention,
at the cost of a directory listing (`std::fs::read_dir`) to discover which
segments already exist on open, which a real directory layout would not
need. Given how rarely `open` runs relative to how often normal operation
does, that one-time cost was the easy trade.

Each segment file starts with its own 16-byte header - the same 8-byte
magic the single file used, plus the global LSN its first record starts at
(`start_lsn`) - so a segment is self-describing: `crate::wal::SegmentStore`
(the trait `FileSegmentStore` implements for real files, and
`FaultySegmentStore` implements for fault-injecting tests) never needs to
consult any other segment to know where its own records fall in LSN space. Mapping an LSN to a physical
location is `segment_header_len + (lsn - segment.start_lsn)` within
whichever segment's `start_lsn` is the largest one `<=` that LSN - a linear
scan of an in-memory `Vec<SegmentMeta>` that never holds more than a few
dozen entries even under sustained load, since segments are deleted once
truncated.

**`open` reads only the active (highest-numbered) segment to find the
append point**, exactly as the old single-file `open` did for the whole
file - now bounded to one segment's worth of bytes (`DEFAULT_SEGMENT_SIZE`,
16 MiB) instead of the database's entire history. Rebuilding
`last_lsn_by_txn` still requires reading every *currently retained*
segment - but "currently retained" is now a bounded set the truncation
policy below actively keeps small, rather than "every segment this database
has ever written." `LogIterator` (`iter_from`) mirrors this: it holds the
list of segment ids still to visit and the store handle to open them, and
loads one segment's bytes only when the previous one is exhausted, instead
of materializing the whole requested tail up front. This is a real
trade-off: since `Iterator::next` cannot return a `Result`, an I/O failure
opening a *later* segment lazily ends iteration silently (as if the log
simply ran out of records) rather than surfacing an error the way a failure
during `iter_from`'s own initial, eager read of the active segment's tail
still does. Given every segment `iter_from` will ever need to open was, by
construction, healthy enough to be read during this same `open` call, this
is an acceptable narrowing of the failure mode, not a silent-corruption
risk.

**`LogManager::truncate_below(bound)` deletes every whole sealed segment
strictly below `bound`, never the active segment.** `write_checkpoint`
calls it with `min(the lowest recLSN in the Dirty Page Table, the lowest
`begin_lsn` of any currently active transaction, the checkpoint's own
`CheckpointBegin` LSN)`. The middle term is deliberately each active
transaction's *first* LSN, not its most recent one (`TransactionManager::
earliest_active_begin_lsn`, backed by a new `Transaction::begin_lsn` field) -
this is a correctness choice, not just literal-mindedness about the
Active Transaction Table. This engine's buffer pool steals dirty pages
under memory pressure (`buffer.MD`), so an active, uncommitted
transaction's early write can already be flushed to disk - and thus absent
from the Dirty Page Table entirely - well before that transaction commits
or aborts. If undo later needs that transaction's *first* update (it walks
backward from its last LSN through every record's `prev_lsn`/
`undo_next_lsn` chain until it reaches `Begin`), the segment holding that
early record must still exist on disk. Bounding by the transaction's most
recent LSN instead would have let a long-lived, partially-flushed
transaction's own early records be deleted out from under its future undo -
exactly the kind of bug a durability fix must not introduce while
"fixing" something else.

**`disk::HEADER_VERSION` moved from 9 to 10.** The on-disk shape of a
database as a whole changed - a pre-existing single `<db_path>.wal` file is
not a segment and `open` on the new code would misinterpret its first
sixteen bytes as a segment header (magic-plus-start-lsn) rather than the
old bare eight-byte magic, silently reading garbage instead of failing
loudly. Bumping the page-0 header's format version turns that into the
existing, well-understood "unsupported on-disk format version" error every
other format change already produces, rather than a new, WAL-specific
failure mode a caller would have to learn to recognize.

**There is exactly one on-disk log layout: the numbered segment family.**
An earlier revision of this ADR kept `LogManager::open_with_device`, a
single-device legacy path every test and crash-injection harness used to
hand a `LogManager` an already-constructed `BlockDevice` without rewriting
every fault-injecting device wrapper into a family. That turned out to be
the wrong trade: a harness that writes through the single-device path and
then reopens for recovery through the production, segment-family path (as
`crates/engine/tests/crash_injection.rs` does) is reading and writing two
different on-disk shapes - the write half creates `<path>`, the read half
looks for `<path>.000000`, finds nothing, and "recovers" an empty log. The
harness measured nothing; every one of its fault points passed by
accident, because recovery never touched the real data regardless of what
the crash preserved. `open_with_device` and its backing `NoSegmentStore`
are deleted rather than fixed in place, because keeping two log shapes
alive is exactly the divergence that caused this. `FaultySegmentStore`
(`wal.MD`) is the replacement: it wraps a real `FileSegmentStore` and
hands back every segment device wrapped in a `block_device::FaultyDevice`
sharing one fault model, so a crash-injection harness now writes and
recovers through the identical segment family production uses.
`LogManager::open` additionally refuses to open a path that exists as a
plain file rather than a segment family (`StorageError::CorruptLogHeader`),
so a database created before this change fails loudly on the first open
instead of silently starting from an empty log.

## Consequences

Reopening a database whose log history is many times the segment size now
reads a bounded number of bytes and touches a bounded number of segments,
not a number proportional to total history - the concrete problem this ADR
exists to fix. A database that checkpoints regularly now keeps a bounded
number of `.wal.NNNNNN` files on disk instead of one ever-growing file.

An old, pre-segmentation database file now fails to open with a version
error instead of being silently misread; there is no migration path from
the old single-file format, since none existed for this project's alpha
data yet and none was requested.

The crash-injection harnesses (`crates/engine/tests/crash_injection.rs`,
`crates/storage/tests/btree_crash_injection.rs`) now sweep real segment
rollover and truncation under fault injection instead of a single
never-rolling device, since `FaultySegmentStore` is the only way any of
them can open a log at all.

`max_txn_id` remains correct under truncation for a reason worth stating
plainly since it is not obvious from the code alone: transaction ids are
assigned monotonically and never reused within a database's lifetime, so
the highest id ever used can only appear in the most recently written
segments - exactly the ones truncation never removes. Nothing needed to
change here beyond noting why.
