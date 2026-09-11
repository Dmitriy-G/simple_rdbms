use catalog::{Catalog, Column, Schema, TableInfo};
use common::{PageId, TableId};
use planner::{Binder, PlannerError, SessionContext, infer_parameter_types, substitute_parameters};
use sql::{Lexer, Parser, Statement};
use types::{DataType, Value};

fn catalog_with_users() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, true),
        Column::new("name", DataType::Varchar(64), true),
        Column::new("active", DataType::Boolean, true),
    ]);
    Catalog::from_tables(vec![TableInfo::new(TableId(1), "users", schema, PageId(0))])
}

fn catalog_with_ab() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("a", DataType::Integer, true),
        Column::new("b", DataType::Varchar(32), true),
    ]);
    Catalog::from_tables(vec![TableInfo::new(TableId(1), "t", schema, PageId(0))])
}

fn parse(source: &str) -> Statement {
    match Lexer::new(source).tokenize().and_then(|tokens| Parser::new(tokens).parse()) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
    }
}

fn infer(catalog: &Catalog, source: &str) -> Result<Vec<Option<DataType>>, PlannerError> {
    infer_parameter_types(&parse(source), catalog)
}

fn bind_debug(catalog: &Catalog, source: &str) -> String {
    let bound = match Binder::new(catalog, SessionContext::new("parameters")).bind(parse(source)) {
        Ok(bound) => bound,
        Err(err) => panic!("unexpected bind error for {source:?}: {err}"),
    };
    format!("{bound:?}")
}

#[test]
fn infers_parameter_type_from_a_where_clause_comparison() {
    let catalog = catalog_with_users();
    let types = match infer(&catalog, "SELECT * FROM users WHERE id = $1") {
        Ok(types) => types,
        Err(err) => panic!("unexpected infer error: {err}"),
    };
    assert_eq!(types, vec![Some(DataType::Integer)]);
}

#[test]
fn insert_parameter_types_follow_the_named_column_list_not_table_order() {
    let catalog = catalog_with_ab();
    let types = match infer(&catalog, "INSERT INTO t (b, a) VALUES ($1, $2)") {
        Ok(types) => types,
        Err(err) => panic!("unexpected infer error: {err}"),
    };
    assert_eq!(types, vec![Some(DataType::Varchar(32)), Some(DataType::Integer)]);
}

#[test]
fn a_placeholder_with_no_column_context_is_unknown() {
    let catalog = catalog_with_users();
    let types = match infer(&catalog, "SELECT $1 FROM users") {
        Ok(types) => types,
        Err(err) => panic!("unexpected infer error: {err}"),
    };
    assert_eq!(types, vec![None]);
}

#[test]
fn a_numbering_gap_is_undefined_parameter() {
    let catalog = catalog_with_users();
    match infer(&catalog, "SELECT * FROM users WHERE id = $1 OR id = $3") {
        Err(PlannerError::UndefinedParameter { index }) => assert_eq!(index, 2),
        other => panic!("expected UndefinedParameter, got {other:?}"),
    }
}

#[test]
fn substitution_produces_the_same_bound_plan_as_the_equivalent_literal_statement() {
    let catalog = catalog_with_users();
    let substituted = match substitute_parameters(
        parse("SELECT * FROM users WHERE id = $1"),
        &[Value::Integer(1)],
    ) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected substitution error: {err}"),
    };
    let substituted_bound =
        match Binder::new(&catalog, SessionContext::new("parameters")).bind(substituted) {
            Ok(bound) => format!("{bound:?}"),
            Err(err) => panic!("unexpected bind error: {err}"),
        };
    let literal_bound = bind_debug(&catalog, "SELECT * FROM users WHERE id = 1");
    assert_eq!(substituted_bound, literal_bound);
}

#[test]
fn too_few_values_is_a_parameter_count_mismatch() {
    match substitute_parameters(parse("SELECT * FROM users WHERE id = $1"), &[]) {
        Err(PlannerError::ParameterCountMismatch { expected, found }) => {
            assert_eq!(expected, 1);
            assert_eq!(found, 0);
        }
        other => panic!("expected ParameterCountMismatch, got {other:?}"),
    }
}

#[test]
fn too_many_values_is_a_parameter_count_mismatch() {
    match substitute_parameters(
        parse("SELECT * FROM users WHERE id = $1"),
        &[Value::Integer(1), Value::Integer(2)],
    ) {
        Err(PlannerError::ParameterCountMismatch { expected, found }) => {
            assert_eq!(expected, 1);
            assert_eq!(found, 2);
        }
        other => panic!("expected ParameterCountMismatch, got {other:?}"),
    }
}
