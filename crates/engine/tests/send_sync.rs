use catalog::Catalog;
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::wal::LogManager;
use txn::TransactionManager;

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn storage_types_are_send_and_sync() {
    assert_send_sync::<BufferPool>();
    assert_send_sync::<DiskManager>();
    assert_send_sync::<LogManager>();
    assert_send_sync::<DoubleWriteBuffer>();
    assert_send_sync::<Catalog>();
    assert_send_sync::<TransactionManager>();
}
