use catalog::{Catalog, Column, IndexInfo, Schema, TableInfo};
use common::{IndexId, PageId, Rid, TableId};
use planner::{Binder, BoundStatement, IndexScanRule, LogicalPlan, Optimizer};
use sql::{Lexer, Parser};
use types::DataType;

const TABLE_ID: TableId = TableId(1);
const INDEX_ID: IndexId = IndexId(1);

fn catalog_with_indexed_id_column() -> Catalog {
    let schema = Schema::new(vec![
        Column::new("id", DataType::Integer, true),
        Column::new("name", DataType::Varchar(64), true),
    ]);
    let table = TableInfo::new(TABLE_ID, "t", schema, PageId(0));
    let index =
        IndexInfo::new(INDEX_ID, "idx_t_id", TABLE_ID, 0, PageId(0), Rid::new(PageId(0), 0), 0);
    Catalog::from_tables_and_indexes(vec![table], vec![index])
}

fn optimized_plan(catalog: &Catalog, source: &str) -> LogicalPlan {
    let statement =
        match Lexer::new(source).tokenize().and_then(|tokens| Parser::new(tokens).parse()) {
            Ok(statement) => statement,
            Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
        };
    let bound = match Binder::new(catalog).bind(statement) {
        Ok(bound) => bound,
        Err(err) => panic!("unexpected bind error for {source:?}: {err}"),
    };
    let logical = match planner::plan(bound) {
        Ok(logical) => logical,
        Err(err) => panic!("unexpected plan error for {source:?}: {err}"),
    };
    Optimizer::new(vec![Box::new(IndexScanRule)]).optimize(logical, catalog)
}

fn filter_input(plan: &LogicalPlan) -> &LogicalPlan {
    match plan {
        LogicalPlan::Projection { input, .. } => match input.as_ref() {
            LogicalPlan::Filter { input, .. } => input.as_ref(),
            other => panic!("expected Filter under Projection, got {other:?}"),
        },
        other => panic!("expected Projection at the top of a SELECT plan, got {other:?}"),
    }
}

fn assert_filter_still_on_top(plan: &LogicalPlan) {
    match plan {
        LogicalPlan::Projection { input, .. } => {
            assert!(
                matches!(input.as_ref(), LogicalPlan::Filter { .. }),
                "the Filter must always stay on top of an IndexScan, got {input:?}"
            );
        }
        other => panic!("expected Projection at the top of a SELECT plan, got {other:?}"),
    }
}

#[test]
fn an_indexed_equality_predicate_is_rewritten_to_an_index_scan() {
    let catalog = catalog_with_indexed_id_column();
    let plan = optimized_plan(&catalog, "SELECT * FROM t WHERE id = 5");
    assert_filter_still_on_top(&plan);
    match filter_input(&plan) {
        LogicalPlan::IndexScan { index_id, table_id, .. } => {
            assert_eq!(*index_id, INDEX_ID);
            assert_eq!(*table_id, TABLE_ID);
        }
        other => panic!("expected IndexScan, got {other:?}"),
    }
}

#[test]
fn a_predicate_on_a_non_indexed_column_stays_a_seq_scan() {
    let catalog = catalog_with_indexed_id_column();
    let plan = optimized_plan(&catalog, "SELECT * FROM t WHERE name = 'x'");
    assert!(
        matches!(filter_input(&plan), LogicalPlan::SeqScan { .. }),
        "a predicate on a non-indexed column must not be rewritten"
    );
}

#[test]
fn a_table_with_no_index_at_all_stays_a_seq_scan() {
    let schema = Schema::new(vec![Column::new("id", DataType::Integer, true)]);
    let catalog = Catalog::from_tables(vec![TableInfo::new(TABLE_ID, "t", schema, PageId(0))]);
    let plan = optimized_plan(&catalog, "SELECT * FROM t WHERE id = 5");
    assert!(matches!(filter_input(&plan), LogicalPlan::SeqScan { .. }));
}

fn bounds(catalog: &Catalog, source: &str) -> (Option<Vec<u8>>, Option<Vec<u8>>) {
    match filter_input(&optimized_plan(catalog, source)) {
        LogicalPlan::IndexScan { start, end, .. } => (start.clone(), end.clone()),
        other => panic!("expected IndexScan, got {other:?}"),
    }
}

#[test]
fn equality_produces_an_inclusive_start_and_an_exclusive_end() {
    let catalog = catalog_with_indexed_id_column();
    let (start, end) = bounds(&catalog, "SELECT * FROM t WHERE id = 5");
    assert!(start.is_some());
    assert!(end.is_some());
    assert!(start < end, "start must sort strictly before end for a single-value equality");
}

#[test]
fn less_than_produces_only_an_exclusive_end() {
    let catalog = catalog_with_indexed_id_column();
    let (start, end) = bounds(&catalog, "SELECT * FROM t WHERE id < 5");
    assert!(start.is_none(), "< must not constrain the start");
    assert!(end.is_some());
}

#[test]
fn less_than_or_equal_end_is_strictly_past_the_equality_end() {
    let catalog = catalog_with_indexed_id_column();
    let (_, lt_end) = bounds(&catalog, "SELECT * FROM t WHERE id < 5");
    let (_, lte_end) = bounds(&catalog, "SELECT * FROM t WHERE id <= 5");
    assert!(lte_end > lt_end, "<= must admit strictly more than < at the same literal");
}

#[test]
fn greater_than_produces_only_an_inclusive_start() {
    let catalog = catalog_with_indexed_id_column();
    let (start, end) = bounds(&catalog, "SELECT * FROM t WHERE id > 5");
    assert!(start.is_some());
    assert!(end.is_none(), "> must not constrain the end");
}

#[test]
fn greater_than_or_equal_start_is_strictly_before_the_greater_than_start() {
    let catalog = catalog_with_indexed_id_column();
    let (gt_start, _) = bounds(&catalog, "SELECT * FROM t WHERE id > 5");
    let (gte_start, _) = bounds(&catalog, "SELECT * FROM t WHERE id >= 5");
    assert!(gte_start < gt_start, ">= must admit strictly more than > at the same literal");
}

#[test]
fn compound_and_on_the_same_column_tightens_both_bounds() {
    let catalog = catalog_with_indexed_id_column();
    let (start, end) = bounds(&catalog, "SELECT * FROM t WHERE id > 1 AND id < 10");
    let (start_alone, _) = bounds(&catalog, "SELECT * FROM t WHERE id > 1");
    let (_, end_alone) = bounds(&catalog, "SELECT * FROM t WHERE id < 10");
    assert_eq!(start, start_alone);
    assert_eq!(end, end_alone);
}

#[test]
fn a_redundant_and_conjunct_does_not_loosen_an_already_tighter_bound() {
    let catalog = catalog_with_indexed_id_column();
    let (tight, _) = bounds(&catalog, "SELECT * FROM t WHERE id > 5 AND id > 1");
    let (loose, _) = bounds(&catalog, "SELECT * FROM t WHERE id > 1");
    assert!(tight > loose, "combining bounds on the same column must keep the tighter one");
}

#[test]
fn binds_create_index() {
    let catalog = catalog_with_indexed_id_column();
    let source = "CREATE INDEX idx_t_name ON t (name)";
    let statement =
        match Lexer::new(source).tokenize().and_then(|tokens| Parser::new(tokens).parse()) {
            Ok(statement) => statement,
            Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
        };
    let bound = match Binder::new(&catalog).bind(statement) {
        Ok(bound) => bound,
        Err(err) => panic!("unexpected bind error for {source:?}: {err}"),
    };
    let BoundStatement::CreateIndex(create) = bound else {
        panic!("expected a bound CREATE INDEX, got {bound:?}");
    };
    assert_eq!(create.index_name, "idx_t_name");
    assert_eq!(create.table_id, TABLE_ID);
    assert_eq!(create.column_index, 1);
}
