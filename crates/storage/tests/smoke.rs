//! Asserts the crate compiles and its main types can be constructed.

use common::PageId;
use storage::StorageError;
use storage::page::Page;
use storage::replacer::LruKReplacer;
use storage::wal::{LogRecord, LogRecordKind};

#[test]
fn page_constructs_zeroed() {
    let page = Page::new(PageId(0));
    assert_eq!(page.id(), PageId(0));
    assert!(page.data().iter().all(|&byte| byte == 0));
}

#[test]
fn lru_k_replacer_constructs() {
    let _replacer = LruKReplacer::new(64, 2);
}

#[test]
fn log_record_constructs() {
    let record = LogRecord { txn_id: common::TxnId(1), kind: LogRecordKind::Begin };
    assert!(matches!(record.kind, LogRecordKind::Begin));
}

#[test]
fn storage_error_displays() {
    let err = StorageError::PageNotFound(7);
    assert_eq!(err.to_string(), "page 7 not found");
}
