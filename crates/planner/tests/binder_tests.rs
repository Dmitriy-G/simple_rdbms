//! Binds parsed statements against a populated catalog and checks that
//! unknown columns/tables and type mismatches are rejected.

use catalog::{Catalog, Column, Schema, TableInfo};
use common::{PageId, TableId};
use planner::{Binder, BoundStatement, PlannerError};
use sql::{Lexer, Parser};
use types::DataType;

fn catalog_with_users() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, true),
        Column::new("name", DataType::Varchar(64), true),
        Column::new("active", DataType::Boolean, true),
    ]);
    Catalog::from_tables(vec![TableInfo::new(TableId(1), "users", schema, PageId(0))])
}

fn parse(source: &str) -> sql::Statement {
    match Lexer::new(source).tokenize().and_then(|tokens| Parser::new(tokens).parse()) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
    }
}

fn bind(catalog: &Catalog, source: &str) -> Result<BoundStatement, PlannerError> {
    Binder::new(catalog).bind(parse(source))
}

fn bind_ok(catalog: &Catalog, source: &str) -> BoundStatement {
    match bind(catalog, source) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected bind error for {source:?}: {err}"),
    }
}

#[test]
fn binds_select_wildcard_to_every_column() {
    let catalog = catalog_with_users();
    let BoundStatement::Select(select) = bind_ok(&catalog, "SELECT * FROM users") else {
        panic!("expected a bound SELECT");
    };
    assert_eq!(select.table_id, TableId(1));
    assert_eq!(select.projections.len(), 3);
    assert!(select.predicate.is_none());
}

#[test]
fn binds_select_with_matching_where_clause() {
    let catalog = catalog_with_users();
    let result = bind(&catalog, "SELECT id FROM users WHERE id = 1 AND active = TRUE");
    assert!(result.is_ok(), "expected bind to succeed, got {result:?}");
}

#[test]
fn rejects_unknown_table() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT * FROM missing") {
        Err(PlannerError::UnknownTable(name)) => assert_eq!(name, "missing"),
        other => panic!("expected UnknownTable, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_column_in_select_list() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT ghost FROM users") {
        Err(PlannerError::UnknownColumn(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownColumn, got {other:?}"),
    }
}

#[test]
fn rejects_unknown_column_in_where_clause() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT * FROM users WHERE ghost = 1") {
        Err(PlannerError::UnknownColumn(name)) => assert_eq!(name, "ghost"),
        other => panic!("expected UnknownColumn, got {other:?}"),
    }
}

#[test]
fn rejects_comparison_between_mismatched_types() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT * FROM users WHERE id = 'oops'") {
        Err(PlannerError::TypeMismatch(_)) => {}
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn rejects_non_boolean_and_operand() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT * FROM users WHERE id AND active = TRUE") {
        Err(PlannerError::TypeMismatch(_)) => {}
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn rejects_insert_value_type_mismatch() {
    let catalog = catalog_with_users();
    match bind(&catalog, "INSERT INTO users (id, name) VALUES ('nope', 'ok')") {
        Err(PlannerError::TypeMismatch(_)) => {}
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn rejects_insert_column_count_mismatch() {
    let catalog = catalog_with_users();
    match bind(&catalog, "INSERT INTO users (id, name) VALUES (1)") {
        Err(PlannerError::ColumnCountMismatch { expected, found }) => {
            assert_eq!(expected, 2);
            assert_eq!(found, 1);
        }
        other => panic!("expected ColumnCountMismatch, got {other:?}"),
    }
}

#[test]
fn insert_without_column_list_defaults_missing_trailing_columns_are_not_allowed() {
    let catalog = catalog_with_users();
    // 3 columns declared, only 2 values supplied, no explicit column list.
    match bind(&catalog, "INSERT INTO users VALUES (1, 'a')") {
        Err(PlannerError::ColumnCountMismatch { expected, found }) => {
            assert_eq!(expected, 3);
            assert_eq!(found, 2);
        }
        other => panic!("expected ColumnCountMismatch, got {other:?}"),
    }
}

#[test]
fn insert_leaves_unlisted_columns_null() {
    let catalog = catalog_with_users();
    let BoundStatement::Insert(insert) = bind_ok(&catalog, "INSERT INTO users (id) VALUES (1)")
    else {
        panic!("expected a bound INSERT");
    };
    assert_eq!(insert.rows.len(), 1);
    assert_eq!(insert.rows[0].len(), 3);
}

#[test]
fn binds_create_table() {
    let catalog = catalog_with_users();
    let BoundStatement::CreateTable(create) =
        bind_ok(&catalog, "CREATE TABLE t (a INTEGER, b TEXT)")
    else {
        panic!("expected a bound CREATE TABLE");
    };
    assert_eq!(create.table_name, "t");
    assert_eq!(create.columns.len(), 2);
}
