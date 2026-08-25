use storage::block_device::FileDevice;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;

#[test]
#[ignore]
fn fetch_page_hot_loop_is_unaffected_by_disabled_trace_calls() {
    const ITERATIONS: usize = 200_000;
    const POOL_FRAMES: usize = 64;

    let dir = tempfile::tempdir().expect("create temp dir");

    let db_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("bench.db"))
        .expect("open db file");
    let disk = DiskManager::open_with_device(Box::new(FileDevice::new(db_file)), PAGE_SIZE, None)
        .expect("open disk manager");

    let wal_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("bench.db.wal"))
        .expect("open wal file");
    let log = LogManager::open_with_device(Box::new(FileDevice::new(wal_file)))
        .expect("open log manager");

    let dwb_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.path().join("bench.db.dwb"))
        .expect("open dwb file");
    let dwb = DoubleWriteBuffer::open_with_device(
        Box::new(FileDevice::new(dwb_file)),
        DoubleWriteBuffer::DEFAULT_CAPACITY,
    )
    .expect("open dwb");

    let pool =
        BufferPool::new(disk, dwb, log, POOL_FRAMES, Box::new(LruKReplacer::new(POOL_FRAMES, 2)));

    let page_id = pool.new_page(common::TxnId(1)).expect("allocate a page").0;

    let start = std::time::Instant::now();
    for _ in 0..ITERATIONS {
        let guard = pool.fetch_page(page_id).expect("fetch_page");
        std::hint::black_box(guard.page());
    }
    let elapsed = start.elapsed();

    println!(
        "fetch_page: {ITERATIONS} calls in {elapsed:?} ({:.1} ns/call)",
        elapsed.as_nanos() as f64 / ITERATIONS as f64
    );
}
