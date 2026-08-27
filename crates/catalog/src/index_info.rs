use std::cell::Cell;

use common::{IndexId, PageId, Rid, TableId};

#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub index_id: IndexId,
    pub name: String,
    pub table_id: TableId,
    pub column_index: usize,
    catalog_rid: Rid,
    root_page_id_offset: usize,
    root_page_id: Cell<PageId>,
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
            root_page_id: Cell::new(root_page_id),
        }
    }

    pub fn root_page_id(&self) -> PageId {
        self.root_page_id.get()
    }

    pub(crate) fn catalog_rid(&self) -> Rid {
        self.catalog_rid
    }

    pub(crate) fn root_page_id_offset(&self) -> usize {
        self.root_page_id_offset
    }

    pub(crate) fn set_root_page_id(&self, new_root: PageId) {
        self.root_page_id.set(new_root);
    }
}
