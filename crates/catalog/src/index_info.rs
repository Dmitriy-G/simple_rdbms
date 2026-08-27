use std::sync::atomic::{AtomicU32, Ordering};

use common::{IndexId, PageId, Rid, TableId};

#[derive(Debug)]
pub struct IndexInfo {
    pub index_id: IndexId,
    pub name: String,
    pub table_id: TableId,
    pub column_index: usize,
    catalog_rid: Rid,
    root_page_id_offset: usize,
    root_page_id: AtomicU32,
}

impl IndexInfo {
    pub fn new(
        index_id: IndexId,
        name: impl Into<String>,
        table_id: TableId,
        column_index: usize,
        root_page_id: PageId,
        catalog_rid: Rid,
        root_page_id_offset: usize,
    ) -> Self {
        Self {
            index_id,
            name: name.into(),
            table_id,
            column_index,
            catalog_rid,
            root_page_id_offset,
            root_page_id: AtomicU32::new(root_page_id.0),
        }
    }

    pub fn root_page_id(&self) -> PageId {
        PageId(self.root_page_id.load(Ordering::Relaxed))
    }

    pub(crate) fn catalog_rid(&self) -> Rid {
        self.catalog_rid
    }

    pub(crate) fn root_page_id_offset(&self) -> usize {
        self.root_page_id_offset
    }

    pub(crate) fn set_root_page_id(&self, new_root: PageId) {
        self.root_page_id.store(new_root.0, Ordering::Relaxed);
    }
}

impl Clone for IndexInfo {
    fn clone(&self) -> Self {
        Self {
            index_id: self.index_id,
            name: self.name.clone(),
            table_id: self.table_id,
            column_index: self.column_index,
            catalog_rid: self.catalog_rid,
            root_page_id_offset: self.root_page_id_offset,
            root_page_id: AtomicU32::new(self.root_page_id.load(Ordering::Relaxed)),
        }
    }
}

impl PartialEq for IndexInfo {
    fn eq(&self, other: &Self) -> bool {
        self.index_id == other.index_id
            && self.name == other.name
            && self.table_id == other.table_id
            && self.column_index == other.column_index
            && self.catalog_rid == other.catalog_rid
            && self.root_page_id_offset == other.root_page_id_offset
            && self.root_page_id.load(Ordering::Relaxed)
                == other.root_page_id.load(Ordering::Relaxed)
    }
}
