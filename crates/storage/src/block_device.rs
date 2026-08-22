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

/// A `BlockDevice` that wraps a real one and fails the first mutating call
/// (`write_at` or `set_len`) once a shared counter passes `fail_at`,
/// simulating a crash mid-workload. Reads and `sync_all` never fail: only
/// `write_at`/`set_len` count as "writes" for the purpose of "fail after
/// exactly N successful writes," and every write before the failing one
/// completes for real against the wrapped device, so the underlying file
/// ends up in exactly the state a real crash at that point would leave.
///
/// The counter is an `Rc<Cell<u64>>` so that two `FaultyDevice`s - one
/// wrapping the database file, one wrapping the WAL - can share a single
/// counter, making "fail at write N" count across the whole system rather
/// than per file, which is what actually happens when a process dies.
pub struct FaultyDevice {
    inner: Box<dyn BlockDevice>,
    counter: Rc<Cell<u64>>,
    fail_at: u64,
}

impl FaultyDevice {
    /// Wraps `inner`, sharing `counter` with (presumably) another
    /// `FaultyDevice`, and failing the write that would make the counter
    /// exceed `fail_at`.
    pub fn new(inner: Box<dyn BlockDevice>, counter: Rc<Cell<u64>>, fail_at: u64) -> Self {
        Self { inner, counter, fail_at }
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
}

impl BlockDevice for FaultyDevice {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.tick()?;
        self.inner.write_at(offset, buf)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.tick()?;
        self.inner.set_len(len)
    }

    fn sync_all(&mut self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&mut self) -> io::Result<u64> {
        self.inner.size()
    }
}
