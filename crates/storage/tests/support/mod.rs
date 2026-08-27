use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use storage::block_device::BlockDevice;

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
