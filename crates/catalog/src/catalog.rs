use std::collections::HashMap;

use common::{IndexId, PageId, TableId, TxnId};
use storage::btree::BTreeIndex;
use storage::buffer::BufferPool;
use storage::heap::TableHeap;

use crate::error::CatalogError;
use crate::index_info::IndexInfo;
use crate::persist::{decode_index_row, decode_table_info, encode_index_row, encode_table_info};
use crate::schema::Schema;
use crate::table_info::TableInfo;

#[derive(Debug, Default)]
pub struct Catalog {
    tables_by_name: HashMap<String, TableInfo>,
    tables_by_id: HashMap<TableId, String>,
    next_table_id: u32,
    catalog_first_page: Option<PageId>,
    indexes_by_name: HashMap<String, IndexInfo>,
    indexes_by_id: HashMap<IndexId, String>,
    indexes_by_table: HashMap<TableId, Vec<IndexId>>,
    next_index_id: u32,
    index_catalog_first_page: Option<PageId>,
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

    pub fn from_tables_and_indexes(tables: Vec<TableInfo>, indexes: Vec<IndexInfo>) -> Self {
        let mut catalog = Self::from_tables(tables);
        for info in indexes {
            catalog.next_index_id = catalog.next_index_id.max(info.index_id.0 + 1);
            catalog.indexes_by_id.insert(info.index_id, info.name.clone());
            catalog.indexes_by_table.entry(info.table_id).or_default().push(info.index_id);
            catalog.indexes_by_name.insert(info.name.clone(), info);
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

        if let Some(index_catalog_first_page) = buffer_pool.index_catalog_first_page()? {
            catalog.index_catalog_first_page = Some(index_catalog_first_page);
            let index_heap = TableHeap::open(buffer_pool, index_catalog_first_page);
            for entry in index_heap.iter() {
                let (_, bytes) = entry?;
                let (info, _root_page_id) = decode_index_row(&bytes)?;
                catalog.next_index_id = catalog.next_index_id.max(info.index_id.0 + 1);
                catalog.indexes_by_id.insert(info.index_id, info.name.clone());
                catalog.indexes_by_table.entry(info.table_id).or_default().push(info.index_id);
                catalog.indexes_by_name.insert(info.name.clone(), info);
            }
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

    pub fn create_index(
        &mut self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        name: &str,
        table_id: TableId,
        column_index: usize,
    ) -> Result<&IndexInfo, CatalogError> {
        if self.indexes_by_name.contains_key(name) {
            return Err(CatalogError::IndexAlreadyExists(name.to_string()));
        }

        let index_id = IndexId(self.next_index_id);
        let tree = BTreeIndex::create(buffer_pool, txn_id)?;
        let info = IndexInfo::new(index_id, name, table_id, column_index);

        let index_catalog_first_page = self.ensure_index_catalog_heap(buffer_pool, txn_id)?;
        let mut index_heap = TableHeap::open(buffer_pool, index_catalog_first_page);
        index_heap.insert_tuple(txn_id, &encode_index_row(&info, tree.root_page_id()))?;

        self.next_index_id += 1;
        self.indexes_by_id.insert(index_id, name.to_string());
        self.indexes_by_table.entry(table_id).or_default().push(index_id);
        self.indexes_by_name.insert(name.to_string(), info);
        Ok(&self.indexes_by_name[name])
    }

    pub fn index_for_column(&self, table_id: TableId, column_index: usize) -> Option<&IndexInfo> {
        self.indexes_by_table.get(&table_id)?.iter().find_map(|index_id| {
            let name = self.indexes_by_id.get(index_id)?;
            let info = self.indexes_by_name.get(name)?;
            (info.column_index == column_index).then_some(info)
        })
    }

    pub fn indexes_for_table(&self, table_id: TableId) -> impl Iterator<Item = &IndexInfo> {
        self.indexes_by_table
            .get(&table_id)
            .into_iter()
            .flatten()
            .filter_map(|index_id| self.indexes_by_id.get(index_id))
            .filter_map(|name| self.indexes_by_name.get(name))
    }

    pub fn index_root_page(
        &self,
        buffer_pool: &BufferPool,
        index_id: IndexId,
    ) -> Result<PageId, CatalogError> {
        let index_catalog_first_page = self.index_catalog_first_page.ok_or_else(|| {
            CatalogError::Corrupt(
                "index_root_page called with no index catalog heap bootstrapped yet".to_string(),
            )
        })?;
        let heap = TableHeap::open(buffer_pool, index_catalog_first_page);
        for entry in heap.iter() {
            let (_, bytes) = entry?;
            let (info, root_page_id) = decode_index_row(&bytes)?;
            if info.index_id == index_id {
                return Ok(root_page_id);
            }
        }
        Err(CatalogError::IndexNotFound(format!("index id {}", index_id.0)))
    }

    pub fn update_index_root_page(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        index_id: IndexId,
        new_root: PageId,
    ) -> Result<(), CatalogError> {
        let index_catalog_first_page = self.index_catalog_first_page.ok_or_else(|| {
            CatalogError::Corrupt(
                "update_index_root_page called with no index catalog heap bootstrapped yet"
                    .to_string(),
            )
        })?;
        let mut heap = TableHeap::open(buffer_pool, index_catalog_first_page);
        let mut found = None;
        for entry in heap.iter() {
            let (rid, bytes) = entry?;
            let (info, _root_page_id) = decode_index_row(&bytes)?;
            if info.index_id == index_id {
                found = Some((rid, info));
                break;
            }
        }
        let (rid, info) =
            found.ok_or_else(|| CatalogError::IndexNotFound(format!("index id {}", index_id.0)))?;
        heap.delete_tuple(txn_id, rid)?;
        heap.insert_tuple(txn_id, &encode_index_row(&info, new_root))?;
        Ok(())
    }

    fn ensure_index_catalog_heap(
        &mut self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
    ) -> Result<PageId, CatalogError> {
        if let Some(page_id) = self.index_catalog_first_page {
            return Ok(page_id);
        }
        if let Some(page_id) = buffer_pool.index_catalog_first_page()? {
            self.index_catalog_first_page = Some(page_id);
            return Ok(page_id);
        }

        let heap = TableHeap::create(buffer_pool, txn_id)?;
        let page_id = heap.first_page_id();
        buffer_pool.set_index_catalog_first_page(txn_id, page_id)?;
        self.index_catalog_first_page = Some(page_id);
        Ok(page_id)
    }
}
