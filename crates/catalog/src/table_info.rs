use common::{PageId, TableId};

use crate::schema::Schema;

/// Everything the rest of the engine needs to know about one table: its
/// catalog identity, its schema, and where its heap storage begins.
#[derive(Debug, Clone, PartialEq)]
pub struct TableInfo {
    /// The table's stable identity within the catalog.
    pub table_id: TableId,
    /// The table's name, unique within the catalog.
    pub name: String,
    /// The table's column schema.
    pub schema: Schema,
    /// The first page of the table's heap file; the entry point for a
    /// sequential scan.
    pub first_page_id: PageId,
}

impl TableInfo {
    /// Builds a table's catalog entry.
    pub fn new(
        table_id: TableId,
        name: impl Into<String>,
        schema: Schema,
        first_page_id: PageId,
    ) -> Self {
        Self { table_id, name: name.into(), schema, first_page_id }
    }
}
