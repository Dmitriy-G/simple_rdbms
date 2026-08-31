use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use storage::StorageError;
use storage::block_device::{BlockDevice, DurabilityModel, FaultyDevice, FileDevice};
use storage::wal::{FileSegmentStore, SegmentStore};

pub type DeviceTriple = (Box<dyn BlockDevice>, Arc<dyn SegmentStore>, Box<dyn BlockDevice>);

pub fn open_file(path: &Path) -> io::Result<std::fs::File> {
    OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)
}

pub fn faulty_devices(
    dir: &Path,
    counter: &Arc<AtomicU64>,
    fail_at: u64,
    model: DurabilityModel,
) -> Result<DeviceTriple, Box<dyn std::error::Error>> {
    let db_file = open_file(&dir.join("test.db"))?;
    let dwb_file = open_file(&dir.join("test.db.dwb"))?;
    let db_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(db_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    let wal_store: Arc<dyn SegmentStore> =
        Arc::new(FaultySegmentStore::new(dir.join("test.db.wal"), counter.clone(), fail_at, model));
    let dwb_device: Box<dyn BlockDevice> = Box::new(FaultyDevice::with_model(
        Box::new(FileDevice::new(dwb_file)),
        counter.clone(),
        fail_at,
        model,
    ));
    Ok((db_device, wal_store, dwb_device))
}

pub struct FaultySegmentStore {
    inner: FileSegmentStore,
    counter: Arc<AtomicU64>,
    fail_at: u64,
    model: DurabilityModel,
}

impl FaultySegmentStore {
    pub fn new(
        base: impl Into<PathBuf>,
        counter: Arc<AtomicU64>,
        fail_at: u64,
        model: DurabilityModel,
    ) -> Self {
        Self { inner: FileSegmentStore::new(base), counter, fail_at, model }
    }
}

impl SegmentStore for FaultySegmentStore {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError> {
        self.inner.existing_segments()
    }

    fn open(&self, id: u64) -> Result<Box<dyn BlockDevice>, StorageError> {
        Ok(Box::new(FaultyDevice::with_model(
            self.inner.open(id)?,
            self.counter.clone(),
            self.fail_at,
            self.model,
        )))
    }

    fn remove(&self, id: u64) -> Result<(), StorageError> {
        self.inner.remove(id)
    }
}

pub struct CountingDevice {
    inner: Box<dyn BlockDevice>,
    calls: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

impl CountingDevice {
    pub fn new(
        inner: Box<dyn BlockDevice>,
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
    ) -> Self {
        Self { inner, calls, bytes }
    }
}

impl BlockDevice for CountingDevice {
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(buf.len(), Ordering::Relaxed);
        self.inner.read_at(offset, buf)
    }

    fn write_at(&self, offset: u64, buf: &[u8]) -> io::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }

    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }

    fn size(&self) -> io::Result<u64> {
        self.inner.size()
    }
}

pub struct CountingSegmentStore {
    inner: FileSegmentStore,
    calls: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
    opens: Arc<AtomicUsize>,
}

impl CountingSegmentStore {
    pub fn new(
        inner: FileSegmentStore,
        calls: Arc<AtomicUsize>,
        bytes: Arc<AtomicUsize>,
        opens: Arc<AtomicUsize>,
    ) -> Self {
        Self { inner, calls, bytes, opens }
    }
}

impl SegmentStore for CountingSegmentStore {
    fn existing_segments(&self) -> Result<Vec<u64>, StorageError> {
        self.inner.existing_segments()
    }

    fn open(&self, id: u64) -> Result<Box<dyn BlockDevice>, StorageError> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        let device = self.inner.open(id)?;
        Ok(Box::new(CountingDevice::new(device, self.calls.clone(), self.bytes.clone())))
    }

    fn remove(&self, id: u64) -> Result<(), StorageError> {
        self.inner.remove(id)
    }
}
