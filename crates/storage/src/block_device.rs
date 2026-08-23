//! The lowest layer both `DiskManager` and `LogManager` write through: a
//! small trait abstracting "a file-like byte range on stable storage,"
//! implemented for real by `FileDevice` and, for crash-injection testing,
//! by `FaultyDevice` (see `crates/engine/tests/crash_injection.rs`).
//!
//! Routing every read/write through this trait rather than calling
//! `std::fs::File` directly is what lets a test wrap either file (the main
//! database file or the WAL) in a device that fails on cue, so the
//! crash-injection harness can simulate a process dying at an arbitrary
//! point in the middle of a real workload without actually killing a
//! process.

use std::cell::Cell;
use std::fs::File;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// A file-like byte-addressable device. `DiskManager` and `LogManager` are
/// written entirely in terms of this trait, never `std::fs::File` directly.
pub trait BlockDevice {
    /// Reads `buf.len()` bytes starting at `offset`, failing if fewer are
    /// available.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    /// Writes `buf` starting at `offset`.
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;

    /// Truncates or extends the device to exactly `len` bytes.
    fn set_len(&mut self, len: u64) -> io::Result<()>;

    /// Forces every write made so far to durable storage.
    fn sync_all(&mut self) -> io::Result<()>;

    /// The device's current length, in bytes. Named `size` rather than
    /// `len` since a device isn't a collection (no `is_empty` makes sense
    /// alongside it).
    fn size(&mut self) -> io::Result<u64>;
}

/// The real `BlockDevice`: an ordinary OS file.
pub struct FileDevice(File);

impl FileDevice {
    /// Wraps an already-open file.
    pub fn new(file: File) -> Self {
        Self(file)
    }
}

impl BlockDevice for FileDevice {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.read_exact(buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.0.seek(SeekFrom::Start(offset))?;
        self.0.write_all(buf)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.0.sync_all()
    }

    fn size(&mut self) -> io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }
}

/// How `FaultyDevice` treats a completed `write_at`/`set_len` call: does it
/// land on the wrapped device immediately, or does it only become durable
/// once a later `sync_all` succeeds - and, independently, is the specific
/// call the fault trips itself torn (a real subset of its bytes lands) or
/// lost outright?
///
/// These two questions are orthogonal, which is why this is a struct of two
/// `bool`s rather than an enum of named combinations: `tear_on_fault` is
/// about what a crash does to the *one* call it interrupts, while
/// `durable_only_on_sync` is about what a crash does to every *other* call
/// that already reported success but was never followed by a `sync_all`.
/// Composing both is what a real power failure looks like - the write in
/// flight when the power dies lands torn, and anything sitting in the page
/// cache before it, unsynced, is gone - which is exactly the case the
/// double-write buffer exists to survive; testing `tear_on_fault` only
/// against a device that is otherwise perfectly durable (as the old
/// three-variant enum did, by grouping tearing with `WriteIsDurable`'s
/// per-call durability) never exercises that combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurabilityModel {
    /// Whether the call the fault trips lands torn - a real, seeded-random
    /// subset of its 512-byte sectors reaching the inner device - rather
    /// than being lost outright. See `FaultyDevice::write_at` for how the
    /// subset is chosen.
    pub tear_on_fault: bool,
    /// Whether a completed `write_at`/`set_len` that does *not* trip the
    /// fault still needs a later `sync_all` to reach the inner device.
    /// `false` means every such call is immediately durable, as if
    /// `sync_all` ran automatically after each one.
    pub durable_only_on_sync: bool,
}

impl DurabilityModel {
    /// Every completed `write_at`/`set_len` is immediately durable, and the
    /// tripped call is lost outright rather than torn. This is the
    /// harness's original, stronger-than-real-disks model: it only ever
    /// loses the one write the fault interrupted, never anything that
    /// "succeeded" but was still sitting in a cache, and never leaves a
    /// partially-written page behind. It covers a real crash that lands
    /// between two syscalls, but not one that lands between a `write()`
    /// returning and the next `fsync()` (`requires_sync`'s gap), nor one
    /// that lands mid-syscall (`torn_write`'s gap).
    pub fn write_is_durable() -> Self {
        Self { tear_on_fault: false, durable_only_on_sync: false }
    }

    /// A completed `write_at`/`set_len` only reaches the wrapped device once
    /// a later `sync_all` call succeeds; anything still pending when the
    /// fault fires is discarded instead of applied, and the tripped call
    /// itself is lost outright - modeling "the write reached the page
    /// cache, but `fsync` had not happened yet," a real and common way for
    /// an OS-level crash or power loss to lose data that a completed
    /// `write()` call already promised. Reads still see pending writes
    /// (matching real page-cache behavior: a live process reads back what
    /// it just wrote), so only what a *crash* would lose differs from
    /// `write_is_durable`.
    pub fn requires_sync() -> Self {
        Self { tear_on_fault: false, durable_only_on_sync: true }
    }

    /// Like `write_is_durable` for every call that doesn't trip the fault -
    /// immediately durable in full - but the tripped `write_at` call itself
    /// is torn rather than lost: a seeded-random subset of its 512-byte
    /// sectors still lands on the inner device before the call reports its
    /// error. Models a single `write_at` being interrupted mid-sector by a
    /// crash or power loss, which neither `write_is_durable` (loses the
    /// whole call) nor `requires_sync` (loses it unless synced) can
    /// produce; this is the failure mode M12's checksum and double-write
    /// buffer exist to detect and repair.
    pub fn torn_write() -> Self {
        Self { tear_on_fault: true, durable_only_on_sync: false }
    }

    /// Both of the above at once: the tripped call tears, *and* every other
    /// call still needs a `sync_all` to be durable, so whatever was pending
    /// when the fault fired is lost outright rather than merely absent from
    /// this one call. This is what an actual power failure does - the write
    /// in flight lands torn, and the page cache behind it evaporates - and
    /// is the combination the double-write buffer must survive even though
    /// neither `torn_write` nor `requires_sync` alone exercises it.
    pub fn torn_write_requires_sync() -> Self {
        Self { tear_on_fault: true, durable_only_on_sync: true }
    }
}

/// A `write_at`/`set_len` call not yet covered by a `sync_all`, under a
/// model with `durable_only_on_sync` set.
enum PendingOp {
    Write { offset: u64, bytes: Vec<u8> },
    SetLen(u64),
}

/// The sector size a real disk actually lands a write as: a 4KB page write
/// reaches hardware as 8 of these, and a crash mid-write can land any
/// subset of them rather than a clean prefix.
const SECTOR_SIZE: usize = 512;

/// A `BlockDevice` that wraps a real one and fails the first mutating call
/// (`write_at` or `set_len`) once a shared counter passes `fail_at`,
/// simulating a crash mid-workload. Reads never fail. Whether a write that
/// completed *before* the fault fired actually survives the "crash" depends
/// on `model` - see `DurabilityModel`.
///
/// The counter is an `Rc<Cell<u64>>` so that two `FaultyDevice`s - one
/// wrapping the database file, one wrapping the WAL - can share a single
/// counter, making "fail at write N" count across the whole system rather
/// than per file, which is what actually happens when a process dies.
pub struct FaultyDevice {
    inner: Box<dyn BlockDevice>,
    counter: Rc<Cell<u64>>,
    fail_at: u64,
    model: DurabilityModel,
    /// Writes and length changes made so far but not yet covered by a
    /// `sync_all`, applied in order. Always empty unless
    /// `model.durable_only_on_sync`.
    pending: Vec<PendingOp>,
    /// Explicit 512-byte sector indices to land when the tripped write
    /// tears, overriding the seeded-random subset `write_at` would
    /// otherwise derive from `fail_at`. `None` (the default) is what every
    /// ordinary crash-injection sweep uses; `Some` exists only for tests
    /// that need to pin down one of the boundary shapes (e.g. "only the
    /// last sector lands") a random seed cannot be relied on to hit.
    tear_sectors: Option<Vec<usize>>,
}

impl FaultyDevice {
    /// Wraps `inner` under `DurabilityModel::write_is_durable`, sharing
    /// `counter` with (presumably) another `FaultyDevice`, and failing the
    /// write that would make the counter exceed `fail_at`.
    pub fn new(inner: Box<dyn BlockDevice>, counter: Rc<Cell<u64>>, fail_at: u64) -> Self {
        Self::with_model(inner, counter, fail_at, DurabilityModel::write_is_durable())
    }

    /// Wraps `inner` under an explicit `model`, otherwise identical to
    /// `new`. When `model.tear_on_fault` is set, the sectors that land are
    /// a seeded-random subset derived from `fail_at`, so a sweep failure at
    /// a given fail point is reproducible from the failure message alone;
    /// use `with_torn_sectors` instead to pin down an exact subset.
    pub fn with_model(
        inner: Box<dyn BlockDevice>,
        counter: Rc<Cell<u64>>,
        fail_at: u64,
        model: DurabilityModel,
    ) -> Self {
        Self { inner, counter, fail_at, model, pending: Vec::new(), tear_sectors: None }
    }

    /// Like `with_model`, but pins the exact 512-byte sector indices
    /// (0-based) that land when the tripped write tears, instead of
    /// deriving them from a seeded RNG. `model.tear_on_fault` must be set,
    /// or `sectors` is never consulted. Test-only: lets a test target a
    /// specific torn shape (e.g. `vec![0]` for "only the first sector",
    /// or the last valid index for "only the last sector") rather than
    /// hoping a seed happens to produce it.
    pub fn with_torn_sectors(
        inner: Box<dyn BlockDevice>,
        counter: Rc<Cell<u64>>,
        fail_at: u64,
        model: DurabilityModel,
        sectors: Vec<usize>,
    ) -> Self {
        debug_assert!(
            model.tear_on_fault,
            "with_torn_sectors is meaningless under a model that doesn't tear on fault"
        );
        let mut device = Self::with_model(inner, counter, fail_at, model);
        device.tear_sectors = Some(sectors);
        device
    }

    /// Ticks the shared write counter, returning an injected error once it
    /// has exceeded `fail_at`.
    fn tick(&self) -> io::Result<()> {
        let count = self.counter.get() + 1;
        self.counter.set(count);
        if count > self.fail_at {
            return Err(io::Error::other(format!(
                "injected fault: write {count} exceeds the armed limit of {}",
                self.fail_at
            )));
        }
        Ok(())
    }

    /// The device's contents as a live process would currently see them:
    /// whatever's really durable on `inner`, with every still-pending
    /// write/length-change replayed on top, in order. Only ever called with
    /// a non-empty `pending` (which requires `model.durable_only_on_sync`) -
    /// `read_at`/`size` skip straight to `inner` otherwise, since there is
    /// nothing to overlay.
    fn materialized(&mut self) -> io::Result<Vec<u8>> {
        let len = self.inner.size()?;
        let mut buf = vec![0u8; len as usize];
        self.inner.read_at(0, &mut buf)?;
        for op in &self.pending {
            match op {
                PendingOp::Write { offset, bytes } => {
                    let end = *offset as usize + bytes.len();
                    if buf.len() < end {
                        buf.resize(end, 0);
                    }
                    buf[*offset as usize..end].copy_from_slice(bytes);
                }
                PendingOp::SetLen(new_len) => {
                    buf.resize(*new_len as usize, 0);
                }
            }
        }
        Ok(buf)
    }

    /// Lands the sectors of `buf` (as if written at `offset`) that a torn
    /// version of this call would actually reach the inner device, writing
    /// each landed sector directly - never through `pending` regardless of
    /// `model.durable_only_on_sync`, since a sector a crash physically wrote
    /// to the platter is durable by definition, independent of whether a
    /// later `fsync` was ever going to be needed for anything else.
    ///
    /// Never lands a sector that would extend `inner` past its current,
    /// already-durable length. Growing a file is `set_len`'s job, not an
    /// incidental side effect of `write_at` seeking past the current end -
    /// under `model.durable_only_on_sync`, an unsynced `set_len` correctly
    /// has not applied yet, and letting a torn write's direct-to-`inner`
    /// sector land past that not-yet-durable boundary would extend the file
    /// anyway, leaving a length no `set_len` call ever actually produced
    /// (surfacing as `StorageError::TruncatedFile` on the next open - a
    /// different failure class than the one this model is for).
    fn tear(&mut self, offset: u64, buf: &[u8]) {
        let Ok(durable_len) = self.inner.size() else {
            return;
        };
        let sector_count = buf.len().div_ceil(SECTOR_SIZE).max(1);
        let landed: Vec<usize> = match &self.tear_sectors {
            Some(explicit) => explicit.iter().copied().filter(|&i| i < sector_count).collect(),
            None => random_sector_subset(sector_count, self.fail_at),
        };
        for index in landed {
            let start = index * SECTOR_SIZE;
            if start >= buf.len() {
                continue;
            }
            let end = (start + SECTOR_SIZE).min(buf.len());
            if offset + end as u64 > durable_len {
                continue;
            }
            // Best-effort: the call is already failing, so a failure to
            // even write a landed sector is not itself reported - it just
            // means an even shorter tear than intended, still a torn write
            // either way.
            let _ = self.inner.write_at(offset + start as u64, &buf[start..end]);
        }
    }
}

/// A seeded-random, *proper* subset of `0..sector_count` - never empty and
/// never the full set - deterministic from `seed` alone so a crash-injection
/// sweep's failure at a given fail point is reproducible from the failure
/// message (which already names the fail point) without recording anything
/// else. `seed` is always the fail point itself - see `FaultyDevice::tear`.
///
/// Excluding the full set matters beyond realism: `write_at` returning `Err`
/// is this harness's *only* signal that a call didn't reliably land, and
/// every caller in the codebase (and every crash-injection assertion) relies
/// on that - a write that lands in full despite reporting failure would be
/// durable and undetectable as torn (no checksum or CRC would ever catch
/// it), silently breaking that contract. A single-sector write (`sector_count
/// == 1`, e.g. a small WAL record) has no subset between "nothing landed"
/// and "it all landed" - there is no such thing as a partial write of less
/// than one physical sector - so for it, tearing can only ever mean total
/// loss, same as it always could.
fn random_sector_subset(sector_count: usize, seed: u64) -> Vec<usize> {
    if sector_count <= 1 {
        return Vec::new();
    }
    let mut rng = StdRng::seed_from_u64(seed);
    loop {
        let subset: Vec<usize> = (0..sector_count).filter(|_| rng.random_bool(0.5)).collect();
        if !subset.is_empty() && subset.len() < sector_count {
            return subset;
        }
    }
}

impl BlockDevice for FaultyDevice {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if self.pending.is_empty() {
            return self.inner.read_at(offset, buf);
        }
        let materialized = self.materialized()?;
        let start = offset as usize;
        let end = start + buf.len();
        if end > materialized.len() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "read past the device's current (materialized) end",
            ));
        }
        buf.copy_from_slice(&materialized[start..end]);
        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if let Err(err) = self.tick() {
            if self.model.tear_on_fault {
                self.tear(offset, buf);
            }
            return Err(err);
        }
        if self.model.durable_only_on_sync {
            self.pending.push(PendingOp::Write { offset, bytes: buf.to_vec() });
            Ok(())
        } else {
            self.inner.write_at(offset, buf)
        }
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.tick()?;
        if self.model.durable_only_on_sync {
            self.pending.push(PendingOp::SetLen(len));
            Ok(())
        } else {
            self.inner.set_len(len)
        }
    }

    fn sync_all(&mut self) -> io::Result<()> {
        for op in self.pending.drain(..) {
            match op {
                PendingOp::Write { offset, bytes } => self.inner.write_at(offset, &bytes)?,
                PendingOp::SetLen(len) => self.inner.set_len(len)?,
            }
        }
        self.inner.sync_all()
    }

    fn size(&mut self) -> io::Result<u64> {
        if self.pending.is_empty() {
            return self.inner.size();
        }
        Ok(self.materialized()?.len() as u64)
    }
}

/// A `BlockDevice` that wraps a real one and counts `read_at` calls and the
/// cumulative bytes requested across them, via counters shared with the
/// test that constructed it (the same shared-`Rc<Cell<_>>` shape
/// `FaultyDevice` uses `counter` for). Test-only: lets a test assert that
/// `LogManager::read_at` costs a small, bounded amount of I/O per lookup
/// rather than re-reading the whole log, without resorting to a flaky
/// wall-clock timing assertion.
#[cfg(test)]
pub struct CountingDevice {
    inner: Box<dyn BlockDevice>,
    calls: Rc<Cell<usize>>,
    bytes: Rc<Cell<usize>>,
}

#[cfg(test)]
impl CountingDevice {
    /// Wraps `inner`, recording every `read_at` call's count and byte size
    /// into `calls`/`bytes`.
    pub fn new(
        inner: Box<dyn BlockDevice>,
        calls: Rc<Cell<usize>>,
        bytes: Rc<Cell<usize>>,
    ) -> Self {
        Self { inner, calls, bytes }
    }
}

#[cfg(test)]
impl BlockDevice for CountingDevice {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        self.bytes.set(self.bytes.get() + buf.len());
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&mut self) -> io::Result<u64> {
        self.inner.size()
    }
}
