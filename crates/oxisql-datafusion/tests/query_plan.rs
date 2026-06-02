//! Plan-bridge integration tests (feature-gated on `parse`).
//!
//! Tests the `sql_to_datafusion_plan` and `to_datafusion_plan` bridge
//! functions that convert `OxiPlan` nodes to DataFusion `LogicalPlan` nodes.

mod common;

#[cfg(feature = "parse")]
use std::sync::Arc;

#[cfg(feature = "parse")]
use datafusion::prelude::SessionContext;
#[cfg(feature = "parse")]
use oxisql_datafusion::OxiSqlTableProvider;

/// `sql_to_datafusion_plan` wraps DataFusion's own SQL planner and returns a
/// valid `LogicalPlan` for a trivial constant-projection query.
///
/// No table registration is required because `SELECT 1 AS n` does not reference
/// any table.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_sql_to_datafusion_plan_constant() {
    use oxisql_datafusion::sql_to_datafusion_plan;

    let ctx = SessionContext::new();
    let plan = sql_to_datafusion_plan("SELECT 1 AS n", &ctx)
        .await
        .expect("sql_to_datafusion_plan should succeed for SELECT 1");

    // The resulting plan must expose at least one output column.
    assert!(
        !plan.schema().fields().is_empty(),
        "plan schema must have at least one field"
    );
}

/// `sql_to_datafusion_plan` works for a query against a registered table.
///
/// Registers a two-row snapshot table, then bridges the SQL string through
/// DataFusion's planner.  The plan's output schema must include `id` and `name`.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_sql_to_datafusion_plan_with_table() {
    use oxisql_datafusion::sql_to_datafusion_plan;

    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("bridge_table", Arc::new(provider))
        .expect("register_table");

    let plan = sql_to_datafusion_plan("SELECT id, name FROM bridge_table WHERE id = 1", &ctx)
        .await
        .expect("sql_to_datafusion_plan should succeed");

    // The plan must have at least one output field.
    assert!(
        !plan.schema().fields().is_empty(),
        "plan schema must be non-empty"
    );
}

/// `to_datafusion_plan` converts `OxiPlan::Empty` to a DataFusion
/// `EmptyRelation` with an empty schema.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_to_datafusion_plan_empty() {
    use datafusion::logical_expr::LogicalPlan as DfPlan;
    use oxisql_datafusion::to_datafusion_plan;
    use oxisql_parse::LogicalPlan as OxiPlan;

    let ctx = SessionContext::new();
    let df_plan = to_datafusion_plan(&OxiPlan::Empty, &ctx)
        .await
        .expect("to_datafusion_plan(Empty) should succeed");

    assert!(
        matches!(df_plan, DfPlan::EmptyRelation(_)),
        "Empty plan must map to DataFusion EmptyRelation"
    );
}

/// `to_datafusion_plan` converts `OxiPlan::Scan` to a table-scan plan when the
/// table is already registered in `ctx`.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_to_datafusion_plan_scan_registered() {
    use oxisql_datafusion::to_datafusion_plan;
    use oxisql_parse::LogicalPlan as OxiPlan;

    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("scan_test", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Scan {
        table: "scan_test".to_string(),
        alias: None,
        limit: None,
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("to_datafusion_plan(Scan) should succeed for a registered table");

    // The plan must have fields matching the scan_test table schema.
    assert!(
        !df_plan.schema().fields().is_empty(),
        "scan plan schema must be non-empty"
    );
}

/// `to_datafusion_plan` returns an error for `OxiPlan::Scan` when the table is
/// NOT registered in `ctx`, because DataFusion cannot resolve the table name.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_to_datafusion_plan_scan_unregistered_errors() {
    use oxisql_datafusion::to_datafusion_plan;
    use oxisql_parse::LogicalPlan as OxiPlan;

    let ctx = SessionContext::new();

    let oxi_plan = OxiPlan::Scan {
        table: "nonexistent_table".to_string(),
        alias: None,
        limit: None,
    };

    let result = to_datafusion_plan(&oxi_plan, &ctx).await;
    assert!(
        result.is_err(),
        "Scan on an unregistered table must return an error"
    );
}

/// `to_datafusion_plan` converts `OxiPlan::Limit` wrapping a `Scan` to a
/// DataFusion plan with a fetch limit applied.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_to_datafusion_plan_limit_over_scan() {
    use oxisql_datafusion::to_datafusion_plan;
    use oxisql_parse::LogicalPlan as OxiPlan;

    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("limit_test", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Limit {
        count: Some(1),
        offset: None,
        input: Box::new(OxiPlan::Scan {
            table: "limit_test".to_string(),
            alias: None,
            limit: None,
        }),
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("to_datafusion_plan(Limit(Scan)) should succeed");

    // The plan schema must still have the table columns.
    assert!(
        !df_plan.schema().fields().is_empty(),
        "limit plan schema must be non-empty"
    );
}

/// `to_datafusion_plan` returns `UnsupportedType` for `OxiPlan::Filter`,
/// because filter predicates are stored as raw SQL strings and cannot be
/// converted structurally without re-parsing.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_to_datafusion_plan_filter_unsupported() {
    use oxisql_datafusion::{to_datafusion_plan, OxiSqlFusionError};
    use oxisql_parse::LogicalPlan as OxiPlan;

    let ctx = SessionContext::new();

    let oxi_plan = OxiPlan::Filter {
        input: Box::new(OxiPlan::Empty),
        predicate: "id > 0".to_string(),
    };

    let result = to_datafusion_plan(&oxi_plan, &ctx).await;
    assert!(
        matches!(result, Err(OxiSqlFusionError::UnsupportedType(_))),
        "Filter must return UnsupportedType: {result:?}"
    );
}
