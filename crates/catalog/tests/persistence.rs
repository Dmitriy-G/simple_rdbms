//! Catalog persistence: tables created against one `BufferPool` must
//! reappear, with identical schemas, when the database file is reopened;
//! duplicate table names are rejected.

use std::error::Error;

use catalog::{Catalog, CatalogError, Column, Schema};
use storage::buffer::BufferPool;
use storage::disk::DiskManager;
use storage::page::PAGE_SIZE;
use storage::replacer::LruKReplacer;
use types::DataType;

fn open_pool(path: &std::path::Path) -> Result<BufferPool, Box<dyn Error>> {
    let disk = DiskManager::open(path, PAGE_SIZE)?;
    Ok(BufferPool::new(disk, 16, Box::new(LruKReplacer::new(16, 2))))
}

fn users_schema() -> Schema {
    Schema::new(vec![
        Column::new("id", DataType::Integer, false),
        Column::new("name", DataType::Varchar(64), true),
    ])
}

#[test]
fn reopening_the_database_reloads_identical_schemas() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");

    {
        let pool = open_pool(&path)?;
        let mut catalog = Catalog::open(&pool)?;
        catalog.create_table(&pool, "users", users_schema())?;
        catalog.create_table(
            &pool,
            "orders",
            Schema::new(vec![
                Column::new("order_id", DataType::BigInt, false),
                Column::new("total", DataType::Double, false),
            ]),
        )?;
        pool.flush_all()?;
    }

    {
        let pool = open_pool(&path)?;
        let catalog = Catalog::open(&pool)?;

        assert_eq!(catalog.table_names(), vec!["orders", "users"]);

        let users = catalog.get_table("users")?;
        assert_eq!(users.schema, users_schema());

        let orders = catalog.get_table("orders")?;
        assert_eq!(orders.schema.columns().len(), 2);
        assert_eq!(orders.schema.columns()[0].name, "order_id");
        assert_eq!(orders.schema.columns()[0].data_type, DataType::BigInt);

        let by_id = catalog.get_table_by_id(users.table_id)?;
        assert_eq!(by_id.name, "users");
    }

    Ok(())
}

#[test]
fn duplicate_table_name_is_rejected() -> Result<(), Box<dyn Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("test.db");
    let pool = open_pool(&path)?;
    let mut catalog = Catalog::open(&pool)?;

    catalog.create_table(&pool, "users", users_schema())?;

    match catalog.create_table(&pool, "users", users_schema()) {
        Err(CatalogError::TableAlreadyExists(name)) => assert_eq!(name, "users"),
        Err(other) => panic!("expected TableAlreadyExists, got {other}"),
        Ok(_) => panic!("a duplicate table name must be rejected"),
    }

    Ok(())
}
