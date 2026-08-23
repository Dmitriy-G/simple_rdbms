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
/// once a later `sync_all` succeeds?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityModel {
    /// Every completed `write_at`/`set_len` is immediately durable, as if
    /// `sync_all` ran automatically after each one. This is the harness's
    /// original, stronger-than-real-disks model: it only ever loses the one
    /// write the fault interrupted, never anything that "succeeded" but was
    /// still sitting in a cache. It covers a real crash that lands between
    /// two syscalls, but not one that lands between a `write()` returning
    /// and the next `fsync()` - which is exactly the gap `RequiresSync`
    /// exercises.
    WriteIsDurable,
    /// A completed `write_at`/`set_len` only reaches the wrapped device once
    /// a later `sync_all` call succeeds; anything still pending when the
    /// fault fires is discarded instead of applied - modeling "the write
    /// reached the page cache, but `fsync` had not happened yet," a real and
    /// common way for an OS-level crash or power loss to lose data that a
    /// completed `write()` call already promised. Reads still see pending
    /// writes (matching real page-cache behavior: a live process reads back
    /// what it just wrote), so only what a *crash* would lose differs from
    /// `WriteIsDurable`.
    RequiresSync,
}

/// A `write_at`/`set_len` call not yet covered by a `sync_all`, under
/// `DurabilityModel::RequiresSync`.
enum PendingOp {
    Write { offset: u64, bytes: Vec<u8> },
    SetLen(u64),
}

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
    /// `sync_all`, applied in order. Always empty under `WriteIsDurable`.
    pending: Vec<PendingOp>,
}

impl FaultyDevice {
    /// Wraps `inner` under `DurabilityModel::WriteIsDurable`, sharing
    /// `counter` with (presumably) another `FaultyDevice`, and failing the
    /// write that would make the counter exceed `fail_at`.
    pub fn new(inner: Box<dyn BlockDevice>, counter: Rc<Cell<u64>>, fail_at: u64) -> Self {
        Self::with_model(inner, counter, fail_at, DurabilityModel::WriteIsDurable)
    }

    /// Wraps `inner` under an explicit `model`, otherwise identical to
    /// `new`.
    pub fn with_model(
        inner: Box<dyn BlockDevice>,
        counter: Rc<Cell<u64>>,
        fail_at: u64,
        model: DurabilityModel,
    ) -> Self {
        Self { inner, counter, fail_at, model, pending: Vec::new() }
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
    /// write/length-change replayed on top, in order. Only ever called
    /// under `RequiresSync` with a non-empty `pending` - `read_at`/`size`
    /// skip straight to `inner` otherwise, since there is nothing to
    /// overlay.
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
        self.tick()?;
        match self.model {
            DurabilityModel::WriteIsDurable => self.inner.write_at(offset, buf),
            DurabilityModel::RequiresSync => {
                self.pending.push(PendingOp::Write { offset, bytes: buf.to_vec() });
                Ok(())
            }
        }
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.tick()?;
        match self.model {
            DurabilityModel::WriteIsDurable => self.inner.set_len(len),
            DurabilityModel::RequiresSync => {
                self.pending.push(PendingOp::SetLen(len));
                Ok(())
            }
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
