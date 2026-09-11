use catalog::{Catalog, Column, Schema, TableInfo};
use common::{PageId, TableId};
use planner::{Binder, BoundExpr, BoundStatement, PlannerError, SessionContext};
use sql::{Lexer, Parser};
use types::{DataType, Value};

fn catalog_with_users() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, true),
        Column::new("name", DataType::Varchar(64), true),
        Column::new("active", DataType::Boolean, true),
    ]);
    Catalog::from_tables(vec![TableInfo::new(TableId(1), "users", schema, PageId(0))])
}

fn catalog_with_numeric_columns() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, true),
        Column::new("big", DataType::BigInt, true),
    ]);
    Catalog::from_tables(vec![TableInfo::new(TableId(1), "nums", schema, PageId(0))])
}

fn parse(source: &str) -> sql::Statement {
    match Lexer::new(source).tokenize().and_then(|tokens| Parser::new(tokens).parse()) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
    }
}

fn bind(catalog: &Catalog, source: &str) -> Result<BoundStatement, PlannerError> {
    Binder::new(catalog, SessionContext::new("binder_tests")).bind(parse(source))
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
    assert_eq!(select.table_id, Some(TableId(1)));
    assert_eq!(select.projections.len(), 3);
    assert!(select.predicate.is_none());
}

#[test]
fn binds_a_select_with_no_from_clause_against_no_table() {
    let catalog = catalog_with_users();
    let BoundStatement::Select(select) = bind_ok(&catalog, "SELECT 1, 'keep alive'") else {
        panic!("expected a bound SELECT");
    };
    assert_eq!(select.table_id, None);
    assert_eq!(select.projections.len(), 2);
    assert!(matches!(select.projections[0], BoundExpr::Literal(Value::BigInt(1))));
    assert!(select.predicate.is_none());
}

#[test]
fn a_column_in_a_select_with_no_from_clause_is_unknown() {
    let catalog = catalog_with_users();
    let err = bind(&catalog, "SELECT id").expect_err("a column with no FROM must not bind");
    assert!(matches!(err, PlannerError::UnknownColumn(_)), "unexpected error: {err}");
}

#[test]
fn a_select_with_no_from_clause_plans_a_projection_over_one_row() {
    let catalog = catalog_with_users();
    let bound = bind_ok(&catalog, "SELECT 1");
    let plan = planner::plan(bound).expect("planning a from-less SELECT must succeed");
    let planner::LogicalPlan::Projection { input, .. } = plan else {
        panic!("expected a projection");
    };
    assert!(matches!(*input, planner::LogicalPlan::OneRow));
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
fn integer_literal_narrows_to_the_integer_column_it_is_compared_against() {
    let catalog = catalog_with_users();
    let BoundStatement::Select(select) = bind_ok(&catalog, "SELECT * FROM users WHERE id = 1")
    else {
        panic!("expected a bound SELECT");
    };
    let Some(BoundExpr::BinaryOp { right, .. }) = &select.predicate else {
        panic!("expected a bound comparison predicate, got {:?}", select.predicate);
    };
    match right.as_ref() {
        BoundExpr::Literal(Value::Integer(1)) => {}
        other => panic!("expected a narrowed Integer(1) literal, got {other:?}"),
    }
}

#[test]
fn integer_literal_narrows_to_the_integer_column_it_is_inserted_into() {
    let catalog = catalog_with_users();
    let BoundStatement::Insert(insert) = bind_ok(&catalog, "INSERT INTO users (id) VALUES (1)")
    else {
        panic!("expected a bound INSERT");
    };
    match &insert.rows[0][0] {
        BoundExpr::Literal(Value::Integer(1)) => {}
        other => panic!("expected a narrowed Integer(1) literal, got {other:?}"),
    }
}

#[test]
fn oversized_literal_into_integer_column_is_out_of_range_not_type_mismatch() {
    let catalog = catalog_with_users();
    let too_big = i64::from(i32::MAX) + 1;
    let source = format!("INSERT INTO users (id) VALUES ({too_big})");
    match bind(&catalog, &source) {
        Err(PlannerError::LiteralOutOfRange { column, .. }) => assert_eq!(column, "id"),
        other => panic!("expected LiteralOutOfRange, got {other:?}"),
    }
}

#[test]
fn oversized_literal_against_integer_column_in_where_clause_is_out_of_range() {
    let catalog = catalog_with_users();
    let too_big = i64::from(i32::MAX) + 1;
    let source = format!("SELECT * FROM users WHERE id = {too_big}");
    match bind(&catalog, &source) {
        Err(PlannerError::LiteralOutOfRange { column, .. }) => assert_eq!(column, "id"),
        other => panic!("expected LiteralOutOfRange, got {other:?}"),
    }
}

#[test]
fn rejects_comparison_between_integer_and_bigint_columns() {
    let catalog = catalog_with_numeric_columns();
    match bind(&catalog, "SELECT * FROM nums WHERE id = big") {
        Err(PlannerError::TypeMismatch(_)) => {}
        other => panic!("expected TypeMismatch, got {other:?}"),
    }
}

#[test]
fn binds_is_null_and_is_not_null_to_boolean() {
    let catalog = catalog_with_users();
    let BoundStatement::Select(select) = bind_ok(&catalog, "SELECT * FROM users WHERE id IS NULL")
    else {
        panic!("expected a bound SELECT");
    };
    match &select.predicate {
        Some(BoundExpr::IsNull { negated, .. }) => assert!(!negated),
        other => panic!("expected a bound IsNull predicate, got {other:?}"),
    }

    let BoundStatement::Select(select) =
        bind_ok(&catalog, "SELECT * FROM users WHERE id IS NOT NULL")
    else {
        panic!("expected a bound SELECT");
    };
    match &select.predicate {
        Some(BoundExpr::IsNull { negated, .. }) => assert!(negated),
        other => panic!("expected a bound IsNull predicate, got {other:?}"),
    }
}

#[test]
fn is_null_accepts_any_operand_type_without_coercion() {
    let catalog = catalog_with_users();
    let result = bind(&catalog, "SELECT * FROM users WHERE name IS NULL AND active IS NOT NULL");
    assert!(result.is_ok(), "expected bind to succeed, got {result:?}");
}

#[test]
fn qualified_column_matches_the_table_name_when_no_alias_is_given() {
    let catalog = catalog_with_users();
    let result = bind(&catalog, "SELECT users.id FROM users WHERE users.active = TRUE");
    assert!(result.is_ok(), "expected bind to succeed, got {result:?}");
}

#[test]
fn qualified_column_matches_the_alias_once_one_is_given() {
    let catalog = catalog_with_users();
    let result = bind(&catalog, "SELECT u.id FROM users u WHERE u.active = TRUE");
    assert!(result.is_ok(), "expected bind to succeed, got {result:?}");
}

#[test]
fn qualified_column_using_the_original_name_after_aliasing_is_unknown_table() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT users.id FROM users u") {
        Err(PlannerError::UnknownTable(name)) => assert_eq!(name, "users"),
        other => panic!("expected UnknownTable, got {other:?}"),
    }
}

#[test]
fn qualified_column_with_an_unknown_table_prefix_is_unknown_table() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT bogus.id FROM users") {
        Err(PlannerError::UnknownTable(name)) => assert_eq!(name, "bogus"),
        other => panic!("expected UnknownTable, got {other:?}"),
    }
}

#[test]
fn qualified_column_in_insert_values_is_unknown_table() {
    let catalog = catalog_with_users();
    match bind(&catalog, "INSERT INTO users (id) VALUES (t.id)") {
        Err(PlannerError::UnknownTable(name)) => assert_eq!(name, "t"),
        other => panic!("expected UnknownTable, got {other:?}"),
    }
}

#[test]
fn binding_an_unsubstituted_parameter_is_undefined_parameter() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT * FROM users WHERE id = $1") {
        Err(PlannerError::UndefinedParameter { index }) => assert_eq!(index, 1),
        other => panic!("expected UndefinedParameter, got {other:?}"),
    }
}

#[test]
fn a_session_context_expression_binds_to_a_value_and_names_its_own_column() {
    let catalog = catalog_with_users();
    let BoundStatement::Select(select) =
        bind_ok(&catalog, "SELECT current_catalog, current_schema, current_user")
    else {
        panic!("expected a bound SELECT");
    };
    assert_eq!(
        select.column_names,
        vec![
            "current_catalog".to_string(),
            "current_schema".to_string(),
            "current_user".to_string()
        ],
        "a driver reads these back by name, so the projection must not call them columnN"
    );
    let values: Vec<Value> = select
        .projections
        .into_iter()
        .map(|expr| match expr {
            BoundExpr::SessionContext { value, .. } => value,
            other => panic!("expected a bound session-context value, got {other:?}"),
        })
        .collect();
    assert_eq!(
        values,
        vec![
            Value::Varchar("binder_tests".to_string()),
            Value::Varchar("public".to_string()),
            Value::Varchar("postgres".to_string()),
        ],
        "the values come from the SessionContext the binder was built with, not from a catalog"
    );
}

#[test]
fn a_real_column_named_like_a_session_context_function_still_resolves() {
    let catalog = catalog_with_users();
    match bind(&catalog, "SELECT current_catalog FROM users") {
        Ok(_) => {}
        other => panic!("expected the keyword form to bind even with a FROM, got {other:?}"),
    }
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
