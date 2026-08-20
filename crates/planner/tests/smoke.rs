//! Asserts the crate compiles and its main types can be constructed.

use catalog::Catalog;
use common::TableId;
use planner::{Binder, LogicalPlan, Optimizer, PhysicalPlan};

#[test]
fn binder_and_optimizer_construct() {
    let catalog = Catalog::new();
    let _binder = Binder::new(&catalog);
    let _optimizer = Optimizer::new(Vec::new());
}

#[test]
fn plan_nodes_construct() {
    let logical = LogicalPlan::SeqScan { table_id: TableId(1) };
    let physical = PhysicalPlan::SeqScan { table_id: TableId(1) };
    assert!(matches!(logical, LogicalPlan::SeqScan { .. }));
    assert!(matches!(physical, PhysicalPlan::SeqScan { .. }));
}
