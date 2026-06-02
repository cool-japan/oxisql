//! Integration tests for `OxiSqlContext`, `register_oxisql_table`,
//! `register_embedded_table`, multi-table joins, and deregister_table.

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use oxisql_core::Connection;

/// `OxiSqlContext` can register an embedded table and execute a basic query.
#[tokio::test]
async fn test_oxisql_context_basic() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE products (id INTEGER, name TEXT, price FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute("INSERT INTO products VALUES (1, 'Widget', 9.99)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO products VALUES (2, 'Gadget', 24.99)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO products VALUES (3, 'Doohickey', 4.99)", &[])
        .await
        .expect("INSERT 3");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("price", DataType::Float64, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let ctx = OxiSqlContext::new();
    ctx.register_table("products", conn_arc, schema)
        .expect("register_table");

    let batches = ctx
        .execute_sql("SELECT * FROM products ORDER BY id")
        .await
        .expect("execute_sql");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3);
}

/// `register_oxisql_table` free function works with a bare `SessionContext`.
#[tokio::test]
async fn test_register_oxisql_table_fn() {
    use datafusion::prelude::SessionContext;
    use oxisql_datafusion::register_oxisql_table;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO t VALUES (10)", &[])
        .await
        .expect("INSERT");

    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, true)]));
    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;

    let session_ctx = SessionContext::new();
    register_oxisql_table(&session_ctx, "t", conn_arc, schema).expect("register_oxisql_table");

    let df = session_ctx.sql("SELECT v FROM t").await.expect("sql");
    let batches = df.collect().await.expect("collect");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 1);
}

/// Multiple tables can be registered in the same `OxiSqlContext` and joined.
#[tokio::test]
async fn test_multi_table_via_single_connection() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = Arc::new(EmbeddedConnection::open_memory().expect("open_memory"))
        as Arc<dyn oxisql_core::Connection>;

    conn.execute("CREATE TABLE a (id INTEGER, val TEXT)", &[])
        .await
        .expect("CREATE a");
    conn.execute("CREATE TABLE b (id INTEGER, num INTEGER)", &[])
        .await
        .expect("CREATE b");
    conn.execute("INSERT INTO a VALUES (1, 'x')", &[])
        .await
        .expect("INSERT a");
    conn.execute("INSERT INTO b VALUES (1, 42)", &[])
        .await
        .expect("INSERT b");

    let schema_a = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("val", DataType::Utf8, true),
    ]));
    let schema_b = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("num", DataType::Int64, true),
    ]));

    let ctx = OxiSqlContext::new();
    ctx.register_table("a", Arc::clone(&conn), schema_a)
        .expect("register a");
    ctx.register_table("b", Arc::clone(&conn), schema_b)
        .expect("register b");

    let batches = ctx
        .execute_sql("SELECT a.val, b.num FROM a, b WHERE a.id = b.id")
        .await
        .expect("execute_sql");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total, 1);
}

/// `deregister_table` removes a table; subsequent deregister returns `false`.
#[tokio::test]
async fn test_deregister_table() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE tmp (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, true)]));
    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;

    let ctx = OxiSqlContext::new();
    ctx.register_table("tmp", conn_arc, schema)
        .expect("register_table");

    let removed = ctx.deregister_table("tmp").expect("deregister");
    assert!(removed, "first deregister should return true");

    let removed_again = ctx.deregister_table("tmp").expect("deregister again");
    assert!(!removed_again, "second deregister should return false");
}

/// `EmbeddedConnection` registered in DataFusion and queried via `execute_sql`.
///
/// Verifies that a live GlueSQL in-memory connection can be wired through the
/// `OxiSqlContext` stream provider path and that basic SELECT + ORDER BY works.
#[tokio::test]
async fn test_embedded_connection_in_datafusion() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE products (id INTEGER, name TEXT, price FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute("INSERT INTO products VALUES (1, 'Widget', 9.99)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO products VALUES (2, 'Gadget', 19.99)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO products VALUES (3, 'Doohickey', 4.99)", &[])
        .await
        .expect("INSERT 3");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("price", DataType::Float64, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let ctx = OxiSqlContext::new();
    ctx.register_table("products", conn_arc, schema)
        .expect("register_table");

    let results = ctx
        .execute_sql("SELECT name, price FROM products ORDER BY price")
        .await
        .expect("execute_sql");

    assert!(!results.is_empty(), "expected at least one batch");
    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3, "expected 3 product rows");
}

/// GROUP BY aggregation via DataFusion over an `EmbeddedConnection` snapshot.
///
/// Confirms that DataFusion's aggregation engine (SUM + GROUP BY) operates
/// correctly on data loaded from GlueSQL into a snapshot provider.
#[tokio::test]
async fn test_embedded_datafusion_aggregation() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE sales (region TEXT, amount FLOAT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO sales VALUES ('North', 100.0)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO sales VALUES ('South', 200.0)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO sales VALUES ('North', 150.0)", &[])
        .await
        .expect("INSERT 3");

    // Load the data as a snapshot so DataFusion's aggregation engine is used.
    let schema = Arc::new(Schema::new(vec![
        Field::new("region", DataType::Utf8, true),
        Field::new("amount", DataType::Float64, true),
    ]));

    let rows = conn.query("SELECT * FROM sales", &[]).await.expect("query");

    let ctx = OxiSqlContext::new();
    ctx.register_snapshot("sales", rows, schema)
        .expect("register_snapshot");

    let results = ctx
        .execute_sql(
            "SELECT region, SUM(amount) AS total \
             FROM sales \
             GROUP BY region \
             ORDER BY region",
        )
        .await
        .expect("execute_sql");

    assert_eq!(results.len(), 1, "GROUP BY should return a single batch");
    // Two groups: North (100 + 150 = 250), South (200)
    assert_eq!(results[0].num_rows(), 2, "expected 2 region groups");
}

/// `register_embedded_table` fetches a snapshot from a live `Connection` and
/// registers it in a DataFusion `SessionContext` with an auto-inferred schema.
///
/// A two-row table is created in an embedded connection; after registration,
/// `COUNT(*)` via DataFusion must return 2.
#[tokio::test]
async fn test_register_embedded_table() {
    use datafusion::prelude::SessionContext;
    use oxisql_datafusion::register_embedded_table;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE emb_test (id INT, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO emb_test VALUES (1, 'alpha')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO emb_test VALUES (2, 'beta')", &[])
        .await
        .expect("INSERT 2");

    let ctx = SessionContext::new();
    register_embedded_table(&ctx, &conn, "emb_test")
        .await
        .expect("register_embedded_table");

    let df = ctx.sql("SELECT COUNT(*) FROM emb_test").await.expect("sql");
    let batches = df.collect().await.expect("collect");

    use arrow::array::Int64Array;
    let count = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("COUNT(*) should be Int64")
        .value(0);
    assert_eq!(count, 2, "COUNT(*) must be 2 after registering 2 rows");
}

/// `OxiSqlContext::register_embedded_table` works through the context wrapper.
#[tokio::test]
async fn test_context_register_embedded_table() {
    use oxisql_datafusion::OxiSqlContext;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE ctx_emb (score FLOAT, active BOOLEAN)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO ctx_emb VALUES (9.5, TRUE)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO ctx_emb VALUES (7.0, FALSE)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO ctx_emb VALUES (8.5, TRUE)", &[])
        .await
        .expect("INSERT 3");

    let ctx = OxiSqlContext::new();
    ctx.register_embedded_table(&conn, "ctx_emb")
        .await
        .expect("register_embedded_table");

    let results = ctx
        .execute_sql("SELECT COUNT(*) FROM ctx_emb WHERE active = true")
        .await
        .expect("execute_sql");

    use arrow::array::Int64Array;
    let count = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("COUNT should be Int64")
        .value(0);
    assert_eq!(count, 2, "two active rows expected");
}

/// `register_embedded_table` gracefully skips empty tables (no rows → no schema).
#[tokio::test]
async fn test_register_embedded_table_empty() {
    use datafusion::prelude::SessionContext;
    use oxisql_datafusion::register_embedded_table;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE empty_emb (id INT)", &[])
        .await
        .expect("CREATE TABLE");

    let ctx = SessionContext::new();
    // Should succeed without registering anything (empty → skip).
    register_embedded_table(&ctx, &conn, "empty_emb")
        .await
        .expect("register_embedded_table with empty table should not error");
}
