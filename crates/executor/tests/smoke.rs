use common::TableId;
use executor::{Executor, SeqScanExecutor};

#[test]
fn seq_scan_executor_constructs_as_trait_object() {
    let scan = SeqScanExecutor::new(TableId(1));
    let boxed: Box<dyn Executor> = Box::new(scan);
    drop(boxed);
}
