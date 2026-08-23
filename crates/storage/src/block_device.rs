use std::cell::Cell;
use std::fs::File;
use std::io;
use std::io::{Read, Seek, SeekFrom, Write};
use std::rc::Rc;

use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

pub trait BlockDevice {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;

    fn set_len(&mut self, len: u64) -> io::Result<()>;

    fn sync_all(&mut self) -> io::Result<()>;

    fn size(&mut self) -> io::Result<u64>;
}

pub struct FileDevice(File);

impl FileDevice {
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
    counter: Rc<Cell<u64>>,
    fail_at: u64,
    model: DurabilityModel,
    pending: Vec<PendingOp>,
    tear_sectors: Option<Vec<usize>>,
}

impl FaultyDevice {
    pub fn new(inner: Box<dyn BlockDevice>, counter: Rc<Cell<u64>>, fail_at: u64) -> Self {
        Self::with_model(inner, counter, fail_at, DurabilityModel::write_is_durable())
    }

    pub fn with_model(
        inner: Box<dyn BlockDevice>,
        counter: Rc<Cell<u64>>,
        fail_at: u64,
        model: DurabilityModel,
    ) -> Self {
        Self { inner, counter, fail_at, model, pending: Vec::new(), tear_sectors: None }
    }

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

#[cfg(test)]
pub struct CountingDevice {
    inner: Box<dyn BlockDevice>,
    calls: Rc<Cell<usize>>,
    bytes: Rc<Cell<usize>>,
}

#[cfg(test)]
impl CountingDevice {
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
