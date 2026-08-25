use std::cell::Cell;
use std::io;
use std::rc::Rc;

use storage::block_device::BlockDevice;

pub struct CountingDevice {
    inner: Box<dyn BlockDevice>,
    calls: Rc<Cell<usize>>,
    bytes: Rc<Cell<usize>>,
}

impl CountingDevice {
    pub fn new(
        inner: Box<dyn BlockDevice>,
        calls: Rc<Cell<usize>>,
        bytes: Rc<Cell<usize>>,
    ) -> Self {
        Self { inner, calls, bytes }
    }
}

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
