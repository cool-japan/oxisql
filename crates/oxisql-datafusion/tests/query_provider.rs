//! Provider-level integration tests:
//! - Wave 9/11 provider tests (from_connection, refresh, display)
//! - Aggregation, multi-table JOIN
//! - Filter pushdown and range partitioning
//! - Window functions via OxiSqlContext
//! - `sort_order_builder` smoke test

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use oxisql_core::{Connection, Row, Value};
use oxisql_datafusion::OxiSqlTableProvider;

/// `COUNT(*)` aggregation returns the expected row count.
///
/// The data is provided through an in-memory `OxiSqlTableProvider` (snapshot)
/// so DataFusion's aggregation engine is exercised without hitting the GlueSQL
/// embedded parser which has limited aggregate support.
#[tokio::test]
async fn test_aggregation_query() {
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

    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("agg_test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT COUNT(*) AS cnt FROM agg_test")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    assert_eq!(batches[0].num_rows(), 1, "aggregate should return 1 row");

    // Extract the count value and verify it equals 5.
    use arrow::array::Int64Array;
    let col = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("count column should be Int64");
    assert_eq!(col.value(0), 5, "COUNT(*) should equal 5");
}

/// `Display` for `OxiSqlTableProvider` includes row and column counts.
#[test]
fn test_provider_display() {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let rows = vec![Row::new(vec!["id".into()], vec![Value::I64(1)])];
    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let s = format!("{provider}");
    // The formatted output must mention the row count (1).
    assert!(
        s.contains('1'),
        "Display must include '1' for row/col count: {s}"
    );
    // Verify the overall format.
    assert!(
        s.starts_with("OxiSqlTableProvider("),
        "Display must start with 'OxiSqlTableProvider(': {s}"
    );
}

/// `OxiSqlTableProvider::from_connection` builds a provider from a live
/// connection by issuing `SELECT * FROM table_name`.
#[tokio::test]
async fn test_from_connection() {
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE fc_test (id INTEGER, val TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO fc_test VALUES (1, 'hello')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO fc_test VALUES (2, 'world')", &[])
        .await
        .expect("INSERT 2");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("val", DataType::Utf8, true),
    ]));

    let provider = OxiSqlTableProvider::from_connection(&conn, "fc_test", schema)
        .await
        .expect("from_connection");
    assert_eq!(provider.len(), 2, "from_connection should load 2 rows");
}

/// `OxiSqlTableProvider::refresh` re-queries the connection and updates the
/// snapshot; the row count grows after a new row is inserted.
#[tokio::test]
async fn test_provider_refresh() {
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE refresh_test (n INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO refresh_test VALUES (1)", &[])
        .await
        .expect("INSERT 1");

    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, true)]));

    let mut provider = OxiSqlTableProvider::from_connection(&conn, "refresh_test", schema)
        .await
        .expect("from_connection");

    let initial_count = provider.len();
    assert_eq!(initial_count, 1, "initial snapshot should have 1 row");

    // Add a second row and refresh.
    conn.execute("INSERT INTO refresh_test VALUES (2)", &[])
        .await
        .expect("INSERT 2");
    provider
        .refresh(&conn, "refresh_test")
        .await
        .expect("refresh");

    let new_count = provider.len();
    assert_eq!(
        new_count,
        initial_count + 1,
        "after refresh there should be 2 rows"
    );
}

/// Multi-table JOIN test: DataFusion joins `users` and `orders` built from
/// in-memory snapshots; Alice's aggregated total should equal 300.0.
#[tokio::test]
async fn test_multi_table_join() {
    let user_schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    let order_schema = Arc::new(Schema::new(vec![
        Field::new("user_id", DataType::Int64, false),
        Field::new("total", DataType::Float64, false),
    ]));

    let users = vec![
        Row::new(
            vec!["id".into(), "name".into()],
            vec![Value::I64(1), Value::Text("Alice".into())],
        ),
        Row::new(
            vec!["id".into(), "name".into()],
            vec![Value::I64(2), Value::Text("Bob".into())],
        ),
    ];
    let orders = vec![
        Row::new(
            vec!["user_id".into(), "total".into()],
            vec![Value::I64(1), Value::F64(100.0)],
        ),
        Row::new(
            vec!["user_id".into(), "total".into()],
            vec![Value::I64(1), Value::F64(200.0)],
        ),
    ];

    let ctx = SessionContext::new();
    ctx.register_table(
        "join_users",
        Arc::new(OxiSqlTableProvider::from_rows(users, user_schema)),
    )
    .expect("register users");
    ctx.register_table(
        "join_orders",
        Arc::new(OxiSqlTableProvider::from_rows(orders, order_schema)),
    )
    .expect("register orders");

    let df = ctx
        .sql(
            "SELECT u.name, SUM(o.total) AS total \
             FROM join_users u \
             JOIN join_orders o ON u.id = o.user_id \
             GROUP BY u.name \
             ORDER BY u.name",
        )
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    assert!(!batches.is_empty(), "JOIN query should return rows");

    // Verify Alice's total is 300.
    use arrow::array::{Array, Float64Array, StringArray};
    let names = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column should be Utf8");
    let totals = batches[0]
        .column(1)
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("total column should be Float64");

    let alice_pos = (0..names.len()).find(|&i| names.value(i) == "Alice");
    assert!(alice_pos.is_some(), "Alice should appear in results");
    let pos = alice_pos.expect("alice_pos already checked");
    assert!(
        (totals.value(pos) - 300.0).abs() < 1e-6,
        "Alice's total should be 300.0, got {}",
        totals.value(pos)
    );
}

/// Smoke test: `OxiSqlStreamProvider::with_sort` compiles, sets the order,
/// and the accessor returns the correct values.
#[test]
fn test_sort_order_builder() {
    use oxisql_core::Connection;
    use oxisql_datafusion::{OxiSqlStreamProvider, SortOrder};

    let schema = Arc::new(Schema::new(vec![
        Field::new("score", DataType::Float64, true),
        Field::new("id", DataType::Int64, true),
    ]));

    // Build a provider with sort order using a stub connection (Arc<dyn Connection>).
    // We only need the provider to be constructed — no async calls are made.
    use oxisql_embedded::EmbeddedConnection;
    let conn =
        Arc::new(EmbeddedConnection::open_memory().expect("open_memory")) as Arc<dyn Connection>;

    let order = vec![
        ("score".to_string(), SortOrder::Desc),
        ("id".to_string(), SortOrder::Asc),
    ];
    let provider = OxiSqlStreamProvider::new(conn, "t", schema).with_sort(order.clone());

    let got = provider.sort_order().expect("sort_order should be Some");
    assert_eq!(got.len(), 2, "two sort columns configured");
    assert_eq!(got[0].0, "score");
    assert_eq!(got[0].1, SortOrder::Desc);
    assert_eq!(got[1].0, "id");
    assert_eq!(got[1].1, SortOrder::Asc);
}

/// `OxiSqlTableProvider` reports `Inexact` for a simple binary-expression filter.
///
/// `supports_filters_pushdown` must return `Inexact` (not `Unsupported`) for a
/// straightforward `id = 2` equality predicate so that DataFusion knows the
/// provider will attempt the filter.
#[test]
fn test_snapshot_filter_returns_inexact() {
    use datafusion::common::Column;
    use datafusion::datasource::TableProvider;
    use datafusion::logical_expr::{BinaryExpr, Expr, Operator, TableProviderFilterPushDown};
    use datafusion::scalar::ScalarValue;

    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let filter = Expr::BinaryExpr(BinaryExpr {
        left: Box::new(Expr::Column(Column::new_unqualified("id"))),
        op: Operator::Eq,
        right: Box::new(Expr::Literal(ScalarValue::Int64(Some(2)), None)),
    });

    let result = provider
        .supports_filters_pushdown(&[&filter])
        .expect("supports_filters_pushdown should not fail");

    assert_eq!(result.len(), 1, "should return one result per filter");
    assert_eq!(
        result[0],
        TableProviderFilterPushDown::Inexact,
        "binary equality filter should be Inexact"
    );
}

/// A DataFusion query with a pushed-down equality filter returns exactly one row.
///
/// Registers a snapshot provider with two rows (id=1, id=2), then runs
/// `SELECT * FROM t WHERE id = 2` and verifies that exactly one row comes back.
#[tokio::test]
async fn test_snapshot_filter_pushdown_equality() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("t_filter", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT * FROM t_filter WHERE id = 2")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "WHERE id = 2 should return exactly 1 row");
}

/// `with_range_partition` sorts rows by the key column and splits into N
/// contiguous partitions.
///
/// 9 rows with unsorted ids (9,3,7,1,5,2,8,4,6) are partitioned by "id" into
/// 3 partitions; after sorting they should be split as 1-3, 4-6, 7-9 (3 rows
/// each).  We verify that:
/// - the total is still 9 rows after partitioning
/// - DataFusion returns all 9 rows when queried
#[tokio::test]
async fn test_range_partition_sorts_and_splits() {
    let schema = Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("id", DataType::Int64, false),
    ]));

    let col = vec!["id".to_string()];
    let ids: Vec<i64> = vec![9, 3, 7, 1, 5, 2, 8, 4, 6];
    let rows: Vec<Row> = ids
        .iter()
        .map(|&i| Row::new(col.clone(), vec![Value::I64(i)]))
        .collect();

    let provider =
        OxiSqlTableProvider::from_rows(rows, Arc::clone(&schema)).with_range_partition("id", 3);

    // Total row count must still be 9.
    assert_eq!(provider.len(), 9, "with_range_partition must not drop rows");

    // Register in DataFusion and verify all 9 rows are returned.
    let ctx = SessionContext::new();
    ctx.register_table("range_test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id FROM range_test ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total_rows, 9,
        "all 9 rows must be returned after partitioning"
    );

    // Verify the rows are in ascending order.
    use arrow::array::Int64Array;
    let all_ids: Vec<i64> = batches
        .iter()
        .flat_map(|b| {
            b.column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("id column should be Int64")
                .values()
                .to_vec()
        })
        .collect();
    let mut sorted_ids = all_ids.clone();
    sorted_ids.sort_unstable();
    assert_eq!(
        all_ids, sorted_ids,
        "ids should be returned in ascending order"
    );
}

/// `with_range_partition` with `n_partitions=1` behaves like no partitioning.
///
/// All rows should still be accessible, just grouped into a single partition.
#[tokio::test]
async fn test_range_partition_single() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema).with_range_partition("id", 1);

    let ctx = SessionContext::new();
    ctx.register_table("single_part", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id FROM single_part ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "n_partitions=1 must still return all rows");
}

/// `ROW_NUMBER() OVER (ORDER BY salary DESC)` numbers three employees correctly.
///
/// DataFusion's window-function support is exercised via `OxiSqlTableProvider`
/// snapshot data; GlueSQL is not involved in the window-function evaluation.
#[tokio::test]
async fn test_window_function_row_number() {
    use oxisql_datafusion::OxiSqlContext;

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("salary", DataType::Int64, false),
    ]));

    let cols: Vec<String> = vec!["id".into(), "name".into(), "salary".into()];
    let rows = vec![
        Row::new(
            cols.clone(),
            vec![Value::I64(1), Value::Text("Alice".into()), Value::I64(1000)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::I64(2), Value::Text("Bob".into()), Value::I64(2000)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::I64(3), Value::Text("Carol".into()), Value::I64(1500)],
        ),
    ];

    let ctx = OxiSqlContext::new();
    ctx.register_snapshot("employees", rows, schema)
        .expect("register_snapshot");

    // ROW_NUMBER() OVER (ORDER BY salary DESC)
    let results = ctx
        .execute_sql(
            "SELECT name, salary, \
             ROW_NUMBER() OVER (ORDER BY salary DESC) AS rn \
             FROM employees",
        )
        .await
        .expect("execute_sql");

    assert_eq!(
        results.len(),
        1,
        "window query should return a single batch"
    );
    assert_eq!(results[0].num_rows(), 3, "expected 3 employee rows");

    // Verify the window function produces a 3-column result (name, salary, rn).
    assert_eq!(
        results[0].num_columns(),
        3,
        "result should have name, salary, and rn columns"
    );
}
