# ADR 0005: State the durability boundary after the double-write buffer

Date: 2026-08-23

Status: Accepted

## Context

M12 closes the last gap ADR 0004 didn't cover: durability held for
anything the WAL could describe as a byte-range delta to an otherwise-intact
page, but a single `write_at` is not atomic on real hardware — a 4KB page
write can be interrupted mid-sector, leaving a page with some old bytes and
some new ones. Part 1 added a per-page CRC-32 so that tearing is at least
detected instead of silently trusted by redo. Detection alone isn't a
durability improvement, though: a `ChecksumMismatch` there was a hard,
unrecoverable error, so a torn page was just as lost as before, only now
loudly instead of quietly. Part 2 (this change) adds the mechanism that
turns detection into repair: every dirty page's image is written to a
separate `<db_path>.dwb` file and synced *before* the real write, so a torn
real write has an intact copy `recovery::recover_double_write` can restore
from on the next open.

Making a durability claim precise matters here for the same reason ADR 0004
gave for ACID: a claim nobody checks against the actual mechanism is not
worth more than not making it. "Torn writes are handled" is true only
within specific limits, and those limits need to be written down rather
than assumed.

## Decision

State precisely what the double-write buffer does and does not cover:

**Covered.** Committed data survives a process kill, an OS crash, or a
power loss that lands mid-write — including one that tears a single
`write_at` call for a whole 4KB page, not just one that lands cleanly
between two separate calls (which M6–M8's write-ahead rule already
covered). This holds for the real data file, the write-ahead log (via its
own per-record CRC-32 and clean truncation at the first torn record — it
never needed the double-write buffer, see the WAL's own module docs), and
the double-write buffer's own file (a torn slot or header write there is
self-detecting the same way a torn real-file write is, and is treated as
"the crash landed before this copy was trusted," never restored from).

The claim above is checked, not assumed: `crates/engine/tests/crash_injection.rs`
sweeps every fixed workload, at every possible fail point, under all four
compositions of `storage::block_device::DurabilityModel` — `write_is_durable`
(a crash lands cleanly between two syscalls), `requires_sync` (a crash loses
whatever was written but never `fsync`'d), `torn_write` (a crash tears the
one call it interrupts, landing a seeded-random subset of that write's
512-byte sectors), and `torn_write_requires_sync`, the composition of the
last two — a torn write *and* a lost unsynced write in the same crash, which
is what an actual power failure does, and the specific case the double-write
buffer exists to survive. The first three each leave one dimension of a real
crash out; only the fourth exercises both at once, which is why it is
swept alongside the other three rather than treated as redundant with
`torn_write` alone.

**Not covered.** A disk that lies about `fsync` having completed (some
consumer SSDs and virtualized/cloud block devices under certain
configurations) - the write-ahead rule and the double-write protocol both
assume a completed `sync_all` really is durable. Media failure large
enough to corrupt both a page and its double-write copy in a way that
still passes CRC-32 in both places - astronomically unlikely for random
bit rot, but not impossible, and not something a 32-bit checksum can be
asked to rule out. Corruption introduced by anything other than a torn
write - a bug elsewhere in this engine that writes the wrong bytes
correctly and durably is not something a checksum can catch, since the
bytes it wrote are exactly what it checksums.

## Consequences

Every dirty page is now written twice - once to the double-write buffer,
once to its real location - and every batch flush (`BufferPool::flush_all`,
and per-page eviction, which flushes a batch of one) costs three `sync_all`
calls instead of zero: one after the double-write batch, one after the
real writes, one after retiring the batch. This is the double-write
buffer's known, standard cost, not a bug to optimize away; the standard
mitigation is that the double-write batch's own write is sequential (one
contiguous region of one file) while the real writes it precedes are
scattered across the data file, so the two are cheaper together on rotating
media than the raw byte count suggests. `BufferPool::flush_all` batches
many dirty pages through one double-write round trip rather than one per
page specifically to amortize this cost, which is also why the
double-write buffer's slot capacity (`DbConfig::dwb_capacity`, default 64)
is a real tuning knob, not an arbitrary constant.

A `ChecksumMismatch` surviving to `recovery::recover`'s own Redo pass is a
hard error again, as it always was before part 1's temporary `TODO(M12.2)`
- by the time Redo runs, `recover_double_write` has already repaired every
torn page it could, so anything still failing its checksum is real, media
corruption beyond what this design can fix, and is worth reporting loudly
rather than silently working around.
