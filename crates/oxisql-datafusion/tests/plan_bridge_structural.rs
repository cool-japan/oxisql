//! Structural Filter + Project lowering tests for `plan_bridge` (feature-gated on `parse`).
//!
//! Covers:
//! - `Filter` node lowers to a DataFusion Filter plan node.
//! - `Project` node lowers to a DataFusion Projection plan node.
//! - Both produce the same rows as the SQL round-trip on an in-memory table.
//! - Wildcard `SELECT *` falls back cleanly (returns `UnsupportedType`).
//! - Unsupported shape (`Aggregate`) falls back cleanly (returns `UnsupportedType`).

mod common;

#[cfg(feature = "parse")]
use std::sync::Arc;

#[cfg(feature = "parse")]
use arrow::array::{Float64Array, Int64Array, StringArray};
#[cfg(feature = "parse")]
use datafusion::prelude::SessionContext;
#[cfg(feature = "parse")]
use oxisql_datafusion::{to_datafusion_plan, OxiSqlFusionError, OxiSqlTableProvider};
#[cfg(feature = "parse")]
use oxisql_parse::LogicalPlan as OxiPlan;

// ── Test 1: Filter node lowers structurally ───────────────────────────────────

/// A `Filter` over a registered table with a valid simple predicate (`id > 1`)
/// lowers to a DataFusion `Filter` plan node.  Verified via the plan's
/// `display_indent()` string, which contains the word "Filter".
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_filter_lowers_structurally() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("filter_struct", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Filter {
        input: Box::new(OxiPlan::Scan {
            table: "filter_struct".to_string(),
            alias: None,
            limit: None,
        }),
        predicate: "id > 1".to_string(),
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("Filter structural lowering should succeed for a valid predicate");

    // The plan display must mention "Filter" — confirms structural node creation.
    let display = format!("{}", df_plan.display_indent());
    assert!(
        display.contains("Filter") || display.contains("filter"),
        "Structural lowering of Filter must produce a DF Filter node; got:\n{display}"
    );
    // Schema must be non-empty (columns from the scan are preserved).
    assert!(
        !df_plan.schema().fields().is_empty(),
        "Filter plan schema must be non-empty"
    );
}

// ── Test 2: Project node lowers structurally ──────────────────────────────────

/// A `Project` over a registered table with explicit (non-wildcard) column
/// names lowers to a DataFusion `Projection` plan node.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_project_lowers_structurally() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("proj_struct", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Project {
        input: Box::new(OxiPlan::Scan {
            table: "proj_struct".to_string(),
            alias: None,
            limit: None,
        }),
        columns: vec!["id".to_string(), "name".to_string()],
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("Project structural lowering should succeed for explicit column list");

    // The plan display must mention "Projection".
    let display = format!("{}", df_plan.display_indent());
    assert!(
        display.contains("Projection") || display.contains("projection"),
        "Structural lowering of Project must produce a DF Projection node; got:\n{display}"
    );
    // Projected schema must contain exactly the two requested columns.
    let field_names: Vec<&str> = df_plan
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().as_str())
        .collect();
    assert!(
        field_names.contains(&"id"),
        "Projected schema must contain 'id'; got {:?}",
        field_names
    );
    assert!(
        field_names.contains(&"name"),
        "Projected schema must contain 'name'; got {:?}",
        field_names
    );
}

// ── Test 3: Filter produces same rows as SQL round-trip ───────────────────────

/// Execute a structurally-lowered `Filter` and compare the collected rows to the
/// result of the equivalent SQL round-trip.  Both must return the same single row.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_filter_rows_match_sql_roundtrip() {
    use datafusion::prelude::col;
    use oxisql_datafusion::sql_to_datafusion_plan;

    let (rows, schema) = common::make_test_rows();
    let provider_a = OxiSqlTableProvider::from_rows(rows.clone(), schema.clone());
    let provider_b = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx_structural = SessionContext::new();
    ctx_structural
        .register_table("filter_match_a", Arc::new(provider_a))
        .expect("register_table a");

    let ctx_roundtrip = SessionContext::new();
    ctx_roundtrip
        .register_table("filter_match_b", Arc::new(provider_b))
        .expect("register_table b");

    // Structural path: Filter { Scan("filter_match_a"), predicate="id = 1" }
    let oxi_plan = OxiPlan::Filter {
        input: Box::new(OxiPlan::Scan {
            table: "filter_match_a".to_string(),
            alias: None,
            limit: None,
        }),
        predicate: "id = 1".to_string(),
    };
    let df_struct = to_datafusion_plan(&oxi_plan, &ctx_structural)
        .await
        .expect("structural Filter should succeed");
    let structural_batches = ctx_structural
        .execute_logical_plan(df_struct)
        .await
        .expect("execute structural plan")
        .collect()
        .await
        .expect("collect structural results");

    // SQL round-trip path
    let df_roundtrip =
        sql_to_datafusion_plan("SELECT * FROM filter_match_b WHERE id = 1", &ctx_roundtrip)
            .await
            .expect("sql round-trip plan should succeed");
    let roundtrip_batches = ctx_roundtrip
        .execute_logical_plan(df_roundtrip)
        .await
        .expect("execute roundtrip plan")
        .collect()
        .await
        .expect("collect roundtrip results");

    let structural_rows: usize = structural_batches.iter().map(|b| b.num_rows()).sum();
    let roundtrip_rows: usize = roundtrip_batches.iter().map(|b| b.num_rows()).sum();

    assert_eq!(
        structural_rows, roundtrip_rows,
        "structural Filter must return the same row count as the SQL round-trip: \
         structural={structural_rows}, roundtrip={roundtrip_rows}"
    );
    assert_eq!(
        structural_rows, 1,
        "id=1 filter must return exactly 1 row, got {structural_rows}"
    );

    // Verify the id value in the returned row.
    let ids_structural: Vec<i64> = structural_batches
        .iter()
        .flat_map(|b| {
            // find the "id" column by name
            let schema = b.schema();
            let idx = schema.index_of("id").expect("id column present");
            b.column(idx)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("id is Int64")
                .values()
                .to_vec()
        })
        .collect();
    assert_eq!(
        ids_structural,
        vec![1i64],
        "structural Filter must return id=1, got {:?}",
        ids_structural
    );
    // suppress unused import warning in non-parse builds
    let _ = col("id");
}

// ── Test 3b: Project produces same rows as SQL round-trip ─────────────────────

/// Execute a structurally-lowered `Project` and compare collected rows to the
/// SQL round-trip.  Both must project the same `id` and `name` values.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_project_rows_match_sql_roundtrip() {
    use oxisql_datafusion::sql_to_datafusion_plan;

    let (rows, schema) = common::make_test_rows();
    let provider_a = OxiSqlTableProvider::from_rows(rows.clone(), schema.clone());
    let provider_b = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx_structural = SessionContext::new();
    ctx_structural
        .register_table("proj_match_a", Arc::new(provider_a))
        .expect("register_table a");

    let ctx_roundtrip = SessionContext::new();
    ctx_roundtrip
        .register_table("proj_match_b", Arc::new(provider_b))
        .expect("register_table b");

    // Structural path: Project { Scan("proj_match_a"), columns=["id","name"] }
    let oxi_plan = OxiPlan::Project {
        input: Box::new(OxiPlan::Scan {
            table: "proj_match_a".to_string(),
            alias: None,
            limit: None,
        }),
        columns: vec!["id".to_string(), "name".to_string()],
    };
    let df_struct = to_datafusion_plan(&oxi_plan, &ctx_structural)
        .await
        .expect("structural Project should succeed");
    let structural_batches = ctx_structural
        .execute_logical_plan(df_struct)
        .await
        .expect("execute structural project plan")
        .collect()
        .await
        .expect("collect structural project results");

    // SQL round-trip path
    let df_roundtrip = sql_to_datafusion_plan("SELECT id, name FROM proj_match_b", &ctx_roundtrip)
        .await
        .expect("sql round-trip plan should succeed");
    let roundtrip_batches = ctx_roundtrip
        .execute_logical_plan(df_roundtrip)
        .await
        .expect("execute roundtrip project plan")
        .collect()
        .await
        .expect("collect roundtrip project results");

    let structural_rows: usize = structural_batches.iter().map(|b| b.num_rows()).sum();
    let roundtrip_rows: usize = roundtrip_batches.iter().map(|b| b.num_rows()).sum();

    assert_eq!(
        structural_rows, roundtrip_rows,
        "structural Project must return same row count as SQL round-trip: \
         structural={structural_rows}, roundtrip={roundtrip_rows}"
    );

    // Collect id and name columns from structural result.
    let ids_structural: Vec<i64> = structural_batches
        .iter()
        .flat_map(|b| {
            let schema = b.schema();
            let idx = schema.index_of("id").expect("id column present");
            b.column(idx)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("id is Int64")
                .values()
                .to_vec()
        })
        .collect();
    let names_structural: Vec<String> = structural_batches
        .iter()
        .flat_map(|b| {
            let schema = b.schema();
            let idx = schema.index_of("name").expect("name column present");
            b.column(idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("name is Utf8")
                .iter()
                .filter_map(|v| v.map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(
        ids_structural,
        vec![1i64, 2i64],
        "structural Project must return id=[1,2], got {:?}",
        ids_structural
    );
    assert_eq!(
        names_structural,
        vec!["Alice".to_string(), "Bob".to_string()],
        "structural Project must return name=[Alice,Bob], got {:?}",
        names_structural
    );

    // Confirm `score` is NOT in the projected schema.
    let field_names: Vec<String> = structural_batches
        .first()
        .map(|b| {
            b.schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert!(
        !field_names.iter().any(|n| n == "score"),
        "structural Project must drop 'score' column; schema fields: {:?}",
        field_names
    );
    // suppress unused import warning
    let _ = Float64Array::from(vec![0.0f64]);
}

// ── Test 3c: Sort node lowers structurally ────────────────────────────────────

/// A `Sort` over a registered table with a valid column key lowers to a
/// DataFusion `Sort` plan node.  The explain output must contain "Sort" and
/// the collected rows must be in the requested order.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_sort_lowers_structurally() {
    use arrow::array::Int64Array;
    use oxisql_parse::SortExpr as OxiSortExpr;

    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("sort_struct", Arc::new(provider))
        .expect("register_table");

    // Sort descending by id — rows should come back in [2, 1] order.
    let oxi_plan = OxiPlan::Sort {
        input: Box::new(OxiPlan::Scan {
            table: "sort_struct".to_string(),
            alias: None,
            limit: None,
        }),
        order_by: vec![OxiSortExpr {
            column: "id".to_string(),
            ascending: false,
        }],
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("Sort structural lowering should succeed for a valid column key");

    // The plan display must mention "Sort".
    let display = format!("{}", df_plan.display_indent());
    assert!(
        display.to_lowercase().contains("sort"),
        "Structural lowering of Sort must produce a DF Sort node; got:\n{display}"
    );

    // Execute and verify descending row order.
    let batches = ctx
        .execute_logical_plan(df_plan)
        .await
        .expect("execute sort plan")
        .collect()
        .await
        .expect("collect sort results");

    let all_ids: Vec<i64> = batches
        .iter()
        .flat_map(|b| {
            let idx = b.schema().index_of("id").expect("id column present");
            b.column(idx)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("id is Int64")
                .values()
                .to_vec()
        })
        .collect();

    assert_eq!(
        all_ids,
        vec![2i64, 1i64],
        "Sort DESC on id must yield [2, 1]; got {:?}",
        all_ids
    );
}

// ── Test 3d: Limit node lowers structurally ───────────────────────────────────

/// A `Limit` plan over a registered table restricts the number of returned rows.
/// With a 2-row table and `count = Some(1)`, exactly one row must be returned.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_limit_lowers_structurally() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("limit_struct", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Limit {
        input: Box::new(OxiPlan::Scan {
            table: "limit_struct".to_string(),
            alias: None,
            limit: None,
        }),
        count: Some(1),
        offset: None,
    };

    let df_plan = to_datafusion_plan(&oxi_plan, &ctx)
        .await
        .expect("Limit structural lowering should succeed");

    let batches = ctx
        .execute_logical_plan(df_plan)
        .await
        .expect("execute limit plan")
        .collect()
        .await
        .expect("collect limit results");

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total_rows, 1,
        "Limit(1) on a 2-row table must return exactly 1 row; got {total_rows}"
    );
}

// ── Test 4: Wildcard `SELECT *` falls back cleanly ────────────────────────────

/// A `Project` with a wildcard (`*`) column must return `UnsupportedType`
/// rather than panicking or returning a wrong result.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_project_wildcard_fallback() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("wildcard_tbl", Arc::new(provider))
        .expect("register_table");

    let oxi_plan = OxiPlan::Project {
        input: Box::new(OxiPlan::Scan {
            table: "wildcard_tbl".to_string(),
            alias: None,
            limit: None,
        }),
        columns: vec!["*".to_string()],
    };

    let result = to_datafusion_plan(&oxi_plan, &ctx).await;
    assert!(
        matches!(result, Err(OxiSqlFusionError::UnsupportedType(_))),
        "Wildcard Project must return UnsupportedType (no panic); got: {result:?}"
    );
}

// ── Test 5: Unsupported shape (Aggregate) falls back cleanly ──────────────────

/// An `Aggregate` node is not structurally lowered and must return
/// `UnsupportedType` without panicking.
#[cfg(feature = "parse")]
#[tokio::test]
async fn test_aggregate_falls_back_to_unsupported() {
    let ctx = SessionContext::new();

    let oxi_plan = OxiPlan::Aggregate {
        input: Box::new(OxiPlan::Empty),
        group_by: vec!["category".to_string()],
        aggregates: vec!["COUNT(*)".to_string()],
    };

    let result = to_datafusion_plan(&oxi_plan, &ctx).await;
    assert!(
        matches!(result, Err(OxiSqlFusionError::UnsupportedType(_))),
        "Aggregate must return UnsupportedType (use sql_to_datafusion_plan); got: {result:?}"
    );
}
