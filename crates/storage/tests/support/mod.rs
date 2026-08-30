use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use storage::StorageError;
use storage::block_device::BlockDevice;
use storage::wal::{FileSegmentStore, SegmentStore};

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
