use std::error::Error;

use catalog::{Catalog, CatalogError, Column, Schema};
use common::{PageId, TxnId};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::dwb::DoubleWriteBuffer;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use storage::wal::LogManager;
use types::DataType;

const TXN: TxnId = TxnId(0);

fn open_pool(path: &std::path::Path) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(path, PAGE_SIZE)?;
    let mut wal_path = path.as_os_str().to_owned();
    wal_path.push(".wal");
    let log = LogManager::open(wal_path)?;
    let mut dwb_path = path.as_os_str().to_owned();
    dwb_path.push(".dwb");
    let dwb = DoubleWriteBuffer::open(dwb_path, DoubleWriteBuffer::DEFAULT_CAPACITY)?;
    Ok(BufferPool::new(disk, dwb, log, 16, Box::new(LruKReplacer::new(16, 2))))
}

fn users_schema() -> Schema {
    Schema::new(vec![
        Column::new("id", DataType::Integer, false),
        Column::new("email", DataType::Varchar(64), true),
    ])
}

#[test]
fn an_index_survives_reopen_with_its_identity_and_root_page_intact() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let table_id = {
        let pool = open_pool(&path)?;
        let mut catalog = Catalog::open(&pool, TXN)?;
        let table = catalog.create_table(&pool, TXN, "users", users_schema())?;
        let table_id = table.table_id;
        let index = catalog.create_index(&pool, TXN, "idx_users_email", table_id, 1)?;
        assert_eq!(index.column_index, 1);
        pool.flush_all()?;
        table_id
    };

    {
        let pool = open_pool(&path)?;
        let catalog = Catalog::open(&pool, TXN)?;

        let index = catalog
            .index_for_column(table_id, 1)
            .expect("the index created before reopen must still be found by column");
        assert_eq!(index.name, "idx_users_email");
        assert_eq!(index.table_id, table_id);

        let root_page = catalog.index_root_page(&pool, index.index_id)?;
        assert_ne!(root_page, PageId(u32::MAX), "root page must be a real, allocated page");

        let indexes: Vec<_> = catalog.indexes_for_table(table_id).collect();
        assert_eq!(indexes.len(), 1);
    }

    Ok(())
}

#[test]
fn duplicate_index_name_is_rejected() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    let pool = open_pool(&path)?;
    let mut catalog = Catalog::open(&pool, TXN)?;

    let table = catalog.create_table(&pool, TXN, "users", users_schema())?;
    let table_id = table.table_id;
    catalog.create_index(&pool, TXN, "idx_users_email", table_id, 1)?;

    match catalog.create_index(&pool, TXN, "idx_users_email", table_id, 0) {
        Err(CatalogError::IndexAlreadyExists(name)) => assert_eq!(name, "idx_users_email"),
        Err(other) => panic!("expected IndexAlreadyExists, got {other}"),
        Ok(_) => panic!("a duplicate index name must be rejected"),
    }

    Ok(())
}

#[test]
fn a_root_page_update_survives_reopen() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    let (table_id, new_root) = {
        let pool = open_pool(&path)?;
        let mut catalog = Catalog::open(&pool, TXN)?;
        let table = catalog.create_table(&pool, TXN, "users", users_schema())?;
        let table_id = table.table_id;
        let index = catalog.create_index(&pool, TXN, "idx_users_id", table_id, 0)?;
        let index_id = index.index_id;

        let original_root = catalog.index_root_page(&pool, index_id)?;
        let new_root = PageId(original_root.0 + 1);
        catalog.update_index_root_page(&pool, TXN, index_id, new_root)?;
        assert_eq!(catalog.index_root_page(&pool, index_id)?, new_root);

        pool.flush_all()?;
        (table_id, new_root)
    };

    {
        let pool = open_pool(&path)?;
        let catalog = Catalog::open(&pool, TXN)?;
        let index = catalog.index_for_column(table_id, 0).expect("index must still be found");
        assert_eq!(catalog.index_root_page(&pool, index.index_id)?, new_root);
    }

    Ok(())
}
