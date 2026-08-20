use std::collections::HashMap;

use common::TableId;

use crate::error::CatalogError;
use crate::schema::Schema;
use crate::table_info::TableInfo;

/// The system catalog: an in-memory registry of every table's metadata,
/// keyed by name. Sits between the planner/executor and `storage`,
/// resolving table and column names to the physical locations operators
/// read from and write to.
#[derive(Debug, Default)]
pub struct Catalog {
    tables_by_name: HashMap<String, TableInfo>,
    #[allow(dead_code)]
    next_table_id: u32,
}

impl Catalog {
    /// Creates an empty catalog.
    pub fn new() -> Self {
        Self { tables_by_name: HashMap::new(), next_table_id: 0 }
    }

    /// Registers a new table with the given `name` and `schema`, allocating
    /// its heap storage and a fresh `TableId`.
    pub fn create_table(&mut self, name: &str, schema: Schema) -> Result<TableId, CatalogError> {
        let _ = (name, schema);
        todo!("check for a name collision, allocate the heap's first page, insert a TableInfo")
    }

    /// Looks up a table's metadata by name.
    pub fn get_table(&self, name: &str) -> Result<&TableInfo, CatalogError> {
        self.tables_by_name.get(name).ok_or_else(|| CatalogError::TableNotFound(name.to_string()))
    }

    /// Removes a table's metadata from the catalog. Does not reclaim its
    /// heap storage.
    pub fn drop_table(&mut self, name: &str) -> Result<(), CatalogError> {
        let _ = name;
        todo!("remove the entry from tables_by_name, erroring if absent")
    }
}
