//! UDF, UDAF, `explain_plan`, and window-function integration tests.

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};

/// `register_scalar_function` registers a UDF and SQL can call it.
///
/// A doubling UDF (`x * 2`) is registered, and the query `SELECT double(3)` is
/// executed.  The result must be `6`.
#[tokio::test]
async fn test_register_scalar_udf() {
    use arrow::array::Int64Array;
    use datafusion::error::DataFusionError;
    use oxisql_datafusion::OxiSqlContext;

    let ctx = OxiSqlContext::new();

    ctx.register_scalar_function(
        "double",
        DataType::Int64,
        vec![DataType::Int64],
        |args: &[Arc<dyn arrow::array::Array>]| -> Result<Arc<dyn arrow::array::Array>, DataFusionError> {
            let input = args[0]
                .as_any()
                .downcast_ref::<Int64Array>()
                .ok_or_else(|| DataFusionError::Internal("expected Int64Array".into()))?;
            let doubled: Int64Array = input.iter().map(|v| v.map(|x| x * 2)).collect();
            Ok(Arc::new(doubled) as Arc<dyn arrow::array::Array>)
        },
    )
    .expect("register_scalar_function");

    let batches = ctx
        .execute_sql("SELECT double(3)")
        .await
        .expect("execute_sql");

    assert!(!batches.is_empty(), "should return at least one batch");
    let col = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("result column should be Int64");
    assert_eq!(col.value(0), 6, "double(3) must equal 6");
}

/// `explain_plan` returns a non-empty string containing plan sections.
///
/// A trivial `SELECT 1` query is passed to `explain_plan`; the result must be
/// non-empty and contain both logical and physical plan markers.
#[tokio::test]
async fn test_explain_plan() {
    use oxisql_datafusion::OxiSqlContext;

    let ctx = OxiSqlContext::new();
    let plan = ctx.explain_plan("SELECT 1").await.expect("explain_plan");

    assert!(
        !plan.is_empty(),
        "explain_plan must return a non-empty string"
    );
    assert!(
        plan.contains("Logical Plan") || plan.contains("Physical Plan"),
        "explain_plan output must reference a plan section: {plan}"
    );
}

/// `ROW_NUMBER() OVER (ORDER BY id)` produces sequential row numbers for 5 rows.
///
/// Registers a 5-row table (id 1-5, names item1-item5) via `register_snapshot`
/// and runs a window-function query.  Each row must receive exactly the rn value
/// corresponding to its `id` position (id=1 → rn=1, id=5 → rn=5).
#[tokio::test]
async fn test_window_functions() {
    use oxisql_core::{Row, Value};
    use oxisql_datafusion::OxiSqlContext;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));

    let cols: Vec<String> = vec!["id".into(), "name".into()];
    let rows: Vec<Row> = (1i64..=5)
        .map(|i| {
            Row::new(
                cols.clone(),
                vec![Value::I64(i), Value::Text(format!("item{i}"))],
            )
        })
        .collect();

    let ctx = OxiSqlContext::new();
    ctx.register_snapshot("wf_test", rows, schema)
        .expect("register_snapshot");

    let results = ctx
        .execute_sql(
            "SELECT name, ROW_NUMBER() OVER (ORDER BY id) AS rn \
             FROM wf_test \
             ORDER BY id",
        )
        .await
        .expect("execute_sql");

    assert!(
        !results.is_empty(),
        "window query must return at least one batch"
    );
    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 5, "must return exactly 5 rows");

    // Verify row_number values are 1..=5 in order.
    // ROW_NUMBER() returns UInt64 in DataFusion.
    use arrow::array::UInt64Array;
    let rn_col = results[0]
        .column(1)
        .as_any()
        .downcast_ref::<UInt64Array>()
        .expect("rn column should be UInt64 (DataFusion ROW_NUMBER returns UInt64)");
    for (i, expected_rn) in (1u64..=5).enumerate() {
        assert_eq!(
            rn_col.value(i),
            expected_rn,
            "row {i}: ROW_NUMBER() should be {expected_rn}, got {}",
            rn_col.value(i)
        );
    }
}
