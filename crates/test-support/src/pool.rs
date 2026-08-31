use std::path::Path;

use storage::block_device::BlockDevice;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

pub struct PoolOptions {
    pool_size: usize,
    replacer_k: usize,
    segment_size: Option<u64>,
    data_device: Option<Box<dyn BlockDevice>>,
}

impl PoolOptions {
    pub fn new(pool_size: usize) -> Self {
        Self { pool_size, replacer_k: 2, segment_size: None, data_device: None }
    }

    pub fn replacer_k(mut self, k: usize) -> Self {
        self.replacer_k = k;
        self
    }

    pub fn segment_size(mut self, size: u64) -> Self {
        self.segment_size = Some(size);
        self
    }

    pub fn data_device(mut self, device: Box<dyn BlockDevice>) -> Self {
        self.data_device = Some(device);
        self
    }
}

fn build_pool(
    db_path: std::path::PathBuf,
    wal_path: std::path::PathBuf,
    dwb_path: std::path::PathBuf,
    options: PoolOptions,
) -> Result<BufferPool, Box<dyn std::error::Error>> {
    let disk = match options.data_device {
        Some(device) => DiskManager::open_with_device(device, PAGE_SIZE, None)?,
        None => DiskManager::open(db_path, PAGE_SIZE)?,
    };
    let dwb = DoubleWriteBuffer::open(dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    let log = match options.segment_size {
        Some(size) => LogManager::open_with_segment_size(wal_path, size)?,
        None => LogManager::open(wal_path)?,
    };
    let replacer = Box::new(LruKReplacer::new(options.pool_size, options.replacer_k));
    Ok(BufferPool::new(disk, dwb, log, options.pool_size, replacer))
}

pub fn open_pool(
    dir: &Path,
    options: PoolOptions,
) -> Result<BufferPool, Box<dyn std::error::Error>> {
    build_pool(dir.join("test.db"), dir.join("test.db.wal"), dir.join("test.db.dwb"), options)
}

pub fn open_pool_at_path(
    db_path: &Path,
    options: PoolOptions,
) -> Result<BufferPool, Box<dyn std::error::Error>> {
    let mut wal_path = db_path.as_os_str().to_owned();
    wal_path.push(".wal");
    let mut dwb_path = db_path.as_os_str().to_owned();
    dwb_path.push(".dwb");
    build_pool(db_path.to_path_buf(), wal_path.into(), dwb_path.into(), options)
}
