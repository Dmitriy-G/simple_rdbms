//! Asserts the crate compiles and its main types can be constructed.

use catalog::{Catalog, Column, Schema};
use types::DataType;

#[test]
fn catalog_constructs_empty() {
    let catalog = Catalog::new();
    assert!(catalog.get_table("missing").is_err());
}

#[test]
fn schema_constructs_from_columns() {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, false),
        Column::new("name", DataType::Varchar(64), true),
    ]);
    assert_eq!(schema.columns().len(), 2);
}
