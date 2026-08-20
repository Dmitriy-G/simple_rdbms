//! Asserts the crate compiles and its main types can be constructed.

#[test]
fn db_config_constructs() {
    let config = common::DbConfig::new("test.db");
    assert_eq!(config.page_size, common::DbConfig::DEFAULT_PAGE_SIZE);
    assert_eq!(config.buffer_pool_size, common::DbConfig::DEFAULT_BUFFER_POOL_SIZE);
}

#[test]
fn ids_construct_and_compare() {
    let page_a = common::PageId(1);
    let page_b = common::PageId(2);
    assert_ne!(page_a, page_b);

    let rid = common::Rid::new(page_a, 0);
    assert_eq!(rid.page_id, page_a);
    assert_eq!(rid.slot, 0);
}

#[test]
fn error_displays() {
    let err = common::Error::NotSupported("example".to_string());
    assert_eq!(err.to_string(), "not supported: example");
}
