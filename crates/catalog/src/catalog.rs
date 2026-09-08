use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

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
struct CatalogState {
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

impl CatalogState {
    fn insert_table(&mut self, info: TableInfo) {
        self.next_table_id = self.next_table_id.max(info.table_id.0 + 1);
        self.tables_by_id.insert(info.table_id, info.name.clone());
        self.tables_by_name.insert(info.name.clone(), info);
    }

    fn insert_index(&mut self, info: IndexInfo) {
        self.next_index_id = self.next_index_id.max(info.index_id.0 + 1);
        self.indexes_by_id.insert(info.index_id, info.name.clone());
        self.indexes_by_table.entry(info.table_id).or_default().push(info.index_id);
        self.indexes_by_name.insert(info.name.clone(), info);
    }

    fn table_by_id(&self, table_id: TableId) -> Result<&TableInfo, CatalogError> {
        let name = self
            .tables_by_id
            .get(&table_id)
            .ok_or_else(|| CatalogError::TableNotFound(format!("table id {}", table_id.0)))?;
        self.tables_by_name.get(name).ok_or_else(|| CatalogError::TableNotFound(name.clone()))
    }

    fn index_by_id(&self, index_id: IndexId) -> Result<&IndexInfo, CatalogError> {
        let name = self
            .indexes_by_id
            .get(&index_id)
            .ok_or_else(|| CatalogError::IndexNotFound(format!("index id {}", index_id.0)))?;
        self.indexes_by_name
            .get(name)
            .ok_or_else(|| CatalogError::IndexNotFound(format!("index id {}", index_id.0)))
    }
}

#[derive(Debug, Default)]
pub struct Catalog {
    state: RwLock<CatalogState>,
    mutation: Mutex<()>,
}

impl Catalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_tables(tables: Vec<TableInfo>) -> Self {
        let mut state = CatalogState::default();
        for info in tables {
            state.insert_table(info);
        }
        Self { state: RwLock::new(state), mutation: Mutex::new(()) }
    }

    pub fn from_tables_and_indexes(tables: Vec<TableInfo>, indexes: Vec<IndexInfo>) -> Self {
        let catalog = Self::from_tables(tables);
        {
            let mut state = catalog.write_state();
            for info in indexes {
                state.insert_index(info);
            }
        }
        catalog
    }

    pub fn open(buffer_pool: &BufferPool, txn_id: TxnId) -> Result<Self, CatalogError> {
        let catalog = Self::new();
        catalog.reload(buffer_pool, txn_id)?;
        Ok(catalog)
    }

    pub fn reload(&self, buffer_pool: &BufferPool, txn_id: TxnId) -> Result<(), CatalogError> {
        let _mutation = self.lock_mutation();
        let catalog_first_page = self.ensure_catalog_heap(buffer_pool, txn_id)?;

        let mut fresh =
            CatalogState { catalog_first_page: Some(catalog_first_page), ..Default::default() };

        let heap = TableHeap::open(buffer_pool, catalog_first_page);
        for entry in heap.iter() {
            let (_, bytes) = entry?;
            fresh.insert_table(decode_table_info(&bytes)?);
        }

        if let Some(index_catalog_first_page) = buffer_pool.index_catalog_first_page()? {
            fresh.index_catalog_first_page = Some(index_catalog_first_page);
            let index_heap = TableHeap::open(buffer_pool, index_catalog_first_page);
            for entry in index_heap.iter() {
                let (rid, bytes) = entry?;
                fresh.insert_index(decode_index_row(&bytes, rid)?);
            }
        }

        *self.write_state() = fresh;
        Ok(())
    }

    pub fn create_table(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        name: &str,
        schema: Schema,
    ) -> Result<TableInfo, CatalogError> {
        let _mutation = self.lock_mutation();
        let table_id = {
            let state = self.read_state();
            if state.tables_by_name.contains_key(name) {
                return Err(CatalogError::TableAlreadyExists(name.to_string()));
            }
            TableId(state.next_table_id)
        };

        let table_heap = TableHeap::create(buffer_pool, txn_id)?;
        let info = TableInfo::new(table_id, name, schema, table_heap.first_page_id());

        let catalog_first_page = self.ensure_catalog_heap(buffer_pool, txn_id)?;
        let mut catalog_heap = TableHeap::open(buffer_pool, catalog_first_page);
        catalog_heap.insert_tuple(txn_id, &encode_table_info(&info))?;

        self.write_state().insert_table(info.clone());
        Ok(info)
    }

    pub fn get_table(&self, name: &str) -> Result<TableInfo, CatalogError> {
        self.read_state()
            .tables_by_name
            .get(name)
            .cloned()
            .ok_or_else(|| CatalogError::TableNotFound(name.to_string()))
    }

    pub fn get_table_by_id(&self, table_id: TableId) -> Result<TableInfo, CatalogError> {
        self.read_state().table_by_id(table_id).cloned()
    }

    pub fn table_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.read_state().tables_by_name.keys().cloned().collect();
        names.sort_unstable();
        names
    }

    pub fn drop_table(&self, name: &str) -> Result<(), CatalogError> {
        let _ = name;
        todo!("remove the entry from tables_by_name, erroring if absent")
    }

    fn ensure_catalog_heap(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
    ) -> Result<PageId, CatalogError> {
        if let Some(page_id) = self.read_state().catalog_first_page {
            return Ok(page_id);
        }
        if let Some(page_id) = buffer_pool.catalog_first_page()? {
            self.write_state().catalog_first_page = Some(page_id);
            return Ok(page_id);
        }

        let heap = TableHeap::create(buffer_pool, txn_id)?;
        let page_id = heap.first_page_id();
        buffer_pool.set_catalog_first_page(txn_id, page_id)?;
        self.write_state().catalog_first_page = Some(page_id);
        Ok(page_id)
    }

    pub fn create_index(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        name: &str,
        table_id: TableId,
        column_index: usize,
    ) -> Result<IndexInfo, CatalogError> {
        let _mutation = self.lock_mutation();
        let index_id = {
            let state = self.read_state();
            if state.indexes_by_name.contains_key(name) {
                return Err(CatalogError::IndexAlreadyExists(name.to_string()));
            }
            IndexId(state.next_index_id)
        };

        let tree = BTreeIndex::create(buffer_pool, txn_id)?;
        let bytes = encode_index_row(index_id, name, table_id, column_index, tree.root_page_id());
        let root_page_id_offset = bytes.len() - 4;

        let index_catalog_first_page = self.ensure_index_catalog_heap(buffer_pool, txn_id)?;
        let mut index_heap = TableHeap::open(buffer_pool, index_catalog_first_page);
        let catalog_rid = index_heap.insert_tuple(txn_id, &bytes)?;

        let info = IndexInfo::new(
            index_id,
            name,
            table_id,
            column_index,
            tree.root_page_id(),
            catalog_rid,
            root_page_id_offset,
        );

        self.write_state().insert_index(info.clone());
        Ok(info)
    }

    pub fn index_for_column(&self, table_id: TableId, column_index: usize) -> Option<IndexInfo> {
        let state = self.read_state();
        state.indexes_by_table.get(&table_id)?.iter().find_map(|index_id| {
            let name = state.indexes_by_id.get(index_id)?;
            let info = state.indexes_by_name.get(name)?;
            (info.column_index == column_index).then(|| info.clone())
        })
    }

    pub fn indexes_for_table(&self, table_id: TableId) -> Vec<IndexInfo> {
        let state = self.read_state();
        state
            .indexes_by_table
            .get(&table_id)
            .into_iter()
            .flatten()
            .filter_map(|index_id| state.indexes_by_id.get(index_id))
            .filter_map(|name| state.indexes_by_name.get(name))
            .cloned()
            .collect()
    }

    pub fn index_root_page(&self, index_id: IndexId) -> Result<PageId, CatalogError> {
        Ok(self.read_state().index_by_id(index_id)?.root_page_id())
    }

    pub fn update_index_root_page(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
        index_id: IndexId,
        new_root: PageId,
    ) -> Result<(), CatalogError> {
        let _mutation = self.lock_mutation();
        let (catalog_rid, root_page_id_offset, index_catalog_first_page) = {
            let state = self.read_state();
            let info = state.index_by_id(index_id)?;
            (info.catalog_rid(), info.root_page_id_offset(), state.index_catalog_first_page)
        };
        let index_catalog_first_page = index_catalog_first_page.ok_or_else(|| {
            CatalogError::Corrupt(
                "update_index_root_page called with no index catalog heap bootstrapped yet"
                    .to_string(),
            )
        })?;

        let mut heap = TableHeap::open(buffer_pool, index_catalog_first_page);
        heap.update_tuple_in_place(
            txn_id,
            catalog_rid,
            root_page_id_offset,
            &new_root.0.to_le_bytes(),
        )?;

        self.read_state().index_by_id(index_id)?.set_root_page_id(new_root);
        Ok(())
    }

    fn ensure_index_catalog_heap(
        &self,
        buffer_pool: &BufferPool,
        txn_id: TxnId,
    ) -> Result<PageId, CatalogError> {
        if let Some(page_id) = self.read_state().index_catalog_first_page {
            return Ok(page_id);
        }
        if let Some(page_id) = buffer_pool.index_catalog_first_page()? {
            self.write_state().index_catalog_first_page = Some(page_id);
            return Ok(page_id);
        }

        let heap = TableHeap::create(buffer_pool, txn_id)?;
        let page_id = heap.first_page_id();
        buffer_pool.set_index_catalog_first_page(txn_id, page_id)?;
        self.write_state().index_catalog_first_page = Some(page_id);
        Ok(page_id)
    }

    fn lock_mutation(&self) -> MutexGuard<'_, ()> {
        self.mutation.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read_state(&self) -> RwLockReadGuard<'_, CatalogState> {
        self.state.read().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write_state(&self) -> RwLockWriteGuard<'_, CatalogState> {
        self.state.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
