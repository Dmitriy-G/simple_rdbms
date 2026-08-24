use std::collections::HashMap;

use common::{PageId, TableId, TxnId};
use storage::buffer::BufferPool;
use storage::heap::TableHeap;

use crate::error::CatalogError;
use crate::persist::{decode_table_info, encode_table_info};
use crate::schema::Schema;
use crate::table_info::TableInfo;

#[derive(Debug, Default)]
pub struct Catalog {
    tables_by_name: HashMap<String, TableInfo>,
    tables_by_id: HashMap<TableId, String>,
    next_table_id: u32,
    catalog_first_page: Option<PageId>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_tables(tables: Vec<TableInfo>) -> Self {
        let mut catalog = Self::new();
        for info in tables {
            catalog.next_table_id = catalog.next_table_id.max(info.table_id.0 + 1);
            catalog.tables_by_id.insert(info.table_id, info.name.clone());
            catalog.tables_by_name.insert(info.name.clone(), info);
        }
        catalog
    }

    pub fn open(buffer_pool: &BufferPool, txn_id: TxnId) -> Result<Self, CatalogError> {
        let mut catalog = Self::new();
        let catalog_first_page = catalog.ensure_catalog_heap(buffer_pool, txn_id)?;

        let heap = TableHeap::open(buffer_pool, catalog_first_page);
        for entry in heap.iter() {
            let (_, bytes) = entry?;
            let info = decode_table_info(&bytes)?;
            catalog.next_table_id = catalog.next_table_id.max(info.table_id.0 + 1);
            catalog.tables_by_id.insert(info.table_id, info.name.clone());
            catalog.tables_by_name.insert(info.name.clone(), info);
        }
        Ok(catalog)
    }

    pub fn create_table(
        &mut self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        name: &str,
        schema: Schema,
    ) -> Result<&TableInfo, CatalogError> {
        if self.tables_by_name.contains_key(name) {
            return Err(CatalogError::TableAlreadyExists(name.to_string()));
        }

        let table_id = TableId(self.next_table_id);
        let table_heap = TableHeap::create(buffer_pool, txn_id)?;
        let info = TableInfo::new(table_id, name, schema, table_heap.first_page_id());

        let catalog_first_page = self.ensure_catalog_heap(buffer_pool, txn_id)?;
        let mut catalog_heap = TableHeap::open(buffer_pool, catalog_first_page);
        catalog_heap.insert_tuple(txn_id, &encode_table_info(&info))?;

        self.next_table_id += 1;
        self.tables_by_id.insert(table_id, name.to_string());
        self.tables_by_name.insert(name.to_string(), info);
        Ok(&self.tables_by_name[name])
    }

    pub fn get_table(&self, name: &str) -> Result<&TableInfo, CatalogError> {
        self.tables_by_name.get(name).ok_or_else(|| CatalogError::TableNotFound(name.to_string()))
    }

    pub fn get_table_by_id(&self, table_id: TableId) -> Result<&TableInfo, CatalogError> {
        let name = self
            .tables_by_id
            .get(&table_id)
            .ok_or_else(|| CatalogError::TableNotFound(format!("table id {}", table_id.0)))?;
        self.tables_by_name.get(name).ok_or_else(|| CatalogError::TableNotFound(name.clone()))
    }

    pub fn table_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.tables_by_name.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    pub fn drop_table(&mut self, name: &str) -> Result<(), CatalogError> {
        let _ = name;
        todo!("remove the entry from tables_by_name, erroring if absent")
    }

    fn ensure_catalog_heap(
        &mut self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
    ) -> Result<PageId, CatalogError> {
        if let Some(page_id) = self.catalog_first_page {
            return Ok(page_id);
        }
        if let Some(page_id) = buffer_pool.catalog_first_page()? {
            self.catalog_first_page = Some(page_id);
            return Ok(page_id);
        }

        let heap = TableHeap::create(buffer_pool, txn_id)?;
        let page_id = heap.first_page_id();
        buffer_pool.set_catalog_first_page(txn_id, page_id)?;
        self.catalog_first_page = Some(page_id);
        Ok(page_id)
    }
}
