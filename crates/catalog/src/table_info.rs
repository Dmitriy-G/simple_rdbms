use common::{PageId, TableId};

use crate::schema::Schema;

#[derive(Debug, Clone, PartialEq)]
pub struct TableInfo {
    pub table_id: TableId,
    pub name: String,
    pub schema: Schema,
    pub first_page_id: PageId,
}

impl TableInfo {
    pub fn new(
        table_id: TableId,
        name: impl Into<String>,
        schema: Schema,
        first_page_id: PageId,
    ) -> Self {
        Self { table_id, name: name.into(), schema, first_page_id }
    }
}
