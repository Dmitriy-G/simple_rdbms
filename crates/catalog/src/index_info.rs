use common::{IndexId, TableId};

#[derive(Debug, Clone, PartialEq)]
pub struct IndexInfo {
    pub index_id: IndexId,
    pub name: String,
    pub table_id: TableId,
    pub column_index: usize,
}

impl IndexInfo {
    pub fn new(
        index_id: IndexId,
        name: impl Into<String>,
        table_id: TableId,
        column_index: usize,
    ) -> Self {
        Self { index_id, name: name.into(), table_id, column_index }
    }
}
