use std::fs::File;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use common::sync::recover_lock;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub trait BlockDevice: Send + Sync {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()>;

    fn set_len(&self, len: u64) -> io::Result<()>;

    fn sync_all(&self) -> io::Result<()>;

    fn size(&self) -> io::Result<u64>;
}

#[cfg(unix)]
fn file_read_at(file: &File, offset: u64, buf: &mut [u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.read_exact_at(buf, offset)
}

#[cfg(unix)]
fn file_write_at(file: &File, offset: u64, buf: &[u8]) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    file.write_all_at(buf, offset)
}

#[cfg(windows)]
fn file_read_at(file: &File, offset: u64, mut buf: &mut [u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut offset = offset;
    while !buf.is_empty() {
        match file.seek_read(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to fill whole buffer",
                ));
            }
            Ok(n) => {
                buf = &mut buf[n..];
                offset += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn file_write_at(file: &File, offset: u64, mut buf: &[u8]) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut offset = offset;
    while !buf.is_empty() {
        match file.seek_write(buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write whole buffer",
                ));
            }
            Ok(n) => {
                buf = &buf[n..];
                offset += n as u64;
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn lock_exclusive(file: &File) -> io::Result<()> {
    use std::os::unix::io::AsRawFd;

    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }

    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;

    // SAFETY: `file` owns a valid, open file descriptor for the duration of
    // this call; `flock` only consults and mutates the kernel's per-open-file
    // lock table for that descriptor and cannot invalidate Rust's view of
    // `file`.
    let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn lock_exclusive(file: &File) -> io::Result<()> {
    use std::ffi::c_void;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        h_event: *mut c_void,
    }

    unsafe extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;

    let mut overlapped = Overlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        h_event: std::ptr::null_mut(),
    };

    // SAFETY: `file`'s raw handle is valid for the duration of this call;
    // `overlapped` is a validly initialized structure owned exclusively by
    // this call and not read after it returns; locking the whole byte range
    // (`u32::MAX` in both halves) touches only the OS lock table for this
    // handle and cannot invalidate Rust's view of `file`.
    let result = unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub struct FileDevice(File);

impl FileDevice {
    pub fn new(file: File) -> Self {
        Self(file)
    }
}

impl BlockDevice for FileDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        file_read_at(&self.0, offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        file_write_at(&self.0, offset, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.0.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.0.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        Ok(self.0.metadata()?.len())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurabilityModel {
    pub tear_on_fault: bool,
    pub durable_only_on_sync: bool,
}

impl DurabilityModel {
    pub fn write_is_durable() -> Self {
        Self { tear_on_fault: false, durable_only_on_sync: false }
    }

    pub fn requires_sync() -> Self {
        Self { tear_on_fault: false, durable_only_on_sync: true }
    }

    pub fn torn_write() -> Self {
        Self { tear_on_fault: true, durable_only_on_sync: false }
    }

    pub fn torn_write_requires_sync() -> Self {
        Self { tear_on_fault: true, durable_only_on_sync: true }
    }
}

enum PendingOp {
    Write { offset: u64, bytes: Vec<u8> },
    SetLen(u64),
}

const SECTOR_SIZE: usize = 512;

pub struct FaultyDevice {
    inner: Box<dyn BlockDevice>,
    counter: Arc<AtomicU64>,
    fail_at: u64,
    model: DurabilityModel,
    pending: Mutex<Vec<PendingOp>>,
    tear_sectors: Option<Vec<usize>>,
}

impl FaultyDevice {
    pub fn new(inner: Box<dyn BlockDevice>, counter: Arc<AtomicU64>, fail_at: u64) -> Self {
        Self::with_model(inner, counter, fail_at, DurabilityModel::write_is_durable())
    }

    pub fn with_model(
        inner: Box<dyn BlockDevice>,
        counter: Arc<AtomicU64>,
        fail_at: u64,
        model: DurabilityModel,
    ) -> Self {
        Self { inner, counter, fail_at, model, pending: Mutex::new(Vec::new()), tear_sectors: None }
    }

    pub fn with_torn_sectors(
        inner: Box<dyn BlockDevice>,
        counter: Arc<AtomicU64>,
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

    fn tick(&self) -> io::Result<()> {
        let count = self.counter.fetch_add(1, Ordering::AcqRel) + 1;
        if count > self.fail_at {
            return Err(io::Error::other(format!(
                "injected fault: write {count} exceeds the armed limit of {}",
                self.fail_at
            )));
        }
        Ok(())
    }

    fn materialized(&self) -> io::Result<Vec<u8>> {
        let len = self.inner.size()?;
        let mut buf = vec![0u8; len as usize];
        self.inner.read_at(0, &mut buf)?;
        let pending = recover_lock(self.pending.lock(), "FaultyDevice.pending");
        for op in pending.iter() {
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

    fn tear(&self, offset: u64, buf: &[u8]) {
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
            let _ = self.inner.write_at(offset + start as u64, &buf[start..end]);
        }
    }
}

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
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        if recover_lock(self.pending.lock(), "FaultyDevice.pending").is_empty() {
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

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if let Err(err) = self.tick() {
            if self.model.tear_on_fault {
                self.tear(offset, buf);
            }
            return Err(err);
        }
        if self.model.durable_only_on_sync {
            recover_lock(self.pending.lock(), "FaultyDevice.pending")
                .push(PendingOp::Write { offset, bytes: buf.to_vec() });
            Ok(())
        } else {
            self.inner.write_at(offset, buf)
        }
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.tick()?;
        if self.model.durable_only_on_sync {
            recover_lock(self.pending.lock(), "FaultyDevice.pending").push(PendingOp::SetLen(len));
            Ok(())
        } else {
            self.inner.set_len(len)
        }
    }

    fn sync_all(&self) -> io::Result<()> {
        let pending: Vec<PendingOp> =
            recover_lock(self.pending.lock(), "FaultyDevice.pending").drain(..).collect();
        for op in pending {
            match op {
                PendingOp::Write { offset, bytes } => self.inner.write_at(offset, &bytes)?,
                PendingOp::SetLen(len) => self.inner.set_len(len)?,
            }
        }
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        if recover_lock(self.pending.lock(), "FaultyDevice.pending").is_empty() {
            return self.inner.size();
        }
        Ok(self.materialized()?.len() as u64)
    }
}
