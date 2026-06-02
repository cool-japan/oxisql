//! Integration tests for `OxiSqlStreamProvider`:
//! - `expr_to_sql` expression translation
//! - Basic stream provider queries
//! - Filter, projection, and limit pushdown

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use oxisql_core::Connection;

/// `expr_to_sql` correctly renders an equality comparison.
#[test]
fn test_expr_to_sql_eq() {
    use datafusion::common::Column;
    use datafusion::logical_expr::{BinaryExpr, Expr, Operator};
    use datafusion::scalar::ScalarValue;

    let expr = Expr::BinaryExpr(BinaryExpr {
        left: Box::new(Expr::Column(Column::new_unqualified("id"))),
        op: Operator::Eq,
        right: Box::new(Expr::Literal(ScalarValue::Int64(Some(42)), None)),
    });
    let sql = oxisql_datafusion::stream::expr_to_sql(&expr).expect("should translate");
    assert_eq!(sql, "(id = 42)");
}

/// `expr_to_sql` correctly renders IS NULL.
#[test]
fn test_expr_to_sql_is_null() {
    use datafusion::common::Column;
    use datafusion::logical_expr::Expr;

    let expr = Expr::IsNull(Box::new(Expr::Column(Column::new_unqualified("name"))));
    let sql = oxisql_datafusion::stream::expr_to_sql(&expr).expect("should translate");
    assert_eq!(sql, "(name IS NULL)");
}

/// `expr_to_sql` correctly renders IS NOT NULL.
#[test]
fn test_expr_to_sql_is_not_null() {
    use datafusion::common::Column;
    use datafusion::logical_expr::Expr;

    let expr = Expr::IsNotNull(Box::new(Expr::Column(Column::new_unqualified("age"))));
    let sql = oxisql_datafusion::stream::expr_to_sql(&expr).expect("should translate");
    assert_eq!(sql, "(age IS NOT NULL)");
}

/// `expr_to_sql` correctly renders a NOT expression.
#[test]
fn test_expr_to_sql_not() {
    use datafusion::common::Column;
    use datafusion::logical_expr::{BinaryExpr, Expr, Operator};
    use datafusion::scalar::ScalarValue;

    let inner = Expr::BinaryExpr(BinaryExpr {
        left: Box::new(Expr::Column(Column::new_unqualified("active"))),
        op: Operator::Eq,
        right: Box::new(Expr::Literal(ScalarValue::Boolean(Some(true)), None)),
    });
    let expr = Expr::Not(Box::new(inner));
    let sql = oxisql_datafusion::stream::expr_to_sql(&expr).expect("should translate");
    assert_eq!(sql, "(NOT (active = TRUE))");
}

/// `expr_to_sql` returns `None` for an unsupported expression.
///
/// `Expr::IsTrue` is not handled by our translation layer and must yield `None`.
#[test]
fn test_expr_to_sql_unsupported_returns_none() {
    use datafusion::common::Column;
    use datafusion::logical_expr::Expr;

    // IsTrue is not in our pushdown allow-list — must yield None.
    let expr = Expr::IsTrue(Box::new(Expr::Column(Column::new_unqualified("flag"))));
    assert!(oxisql_datafusion::stream::expr_to_sql(&expr).is_none());
}

/// `OxiSqlStreamProvider` can serve rows through DataFusion without filters.
#[tokio::test]
async fn test_stream_provider_basic() {
    use oxisql_datafusion::OxiSqlStreamProvider;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO users VALUES (2, 'Bob')", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO users VALUES (3, 'Carol')", &[])
        .await
        .expect("INSERT 3");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let provider = OxiSqlStreamProvider::new(conn_arc, "users", schema);

    let ctx = SessionContext::new();
    ctx.register_table("users", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT * FROM users ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 3, "expected 3 rows");
}

/// `OxiSqlStreamProvider` respects LIMIT pushdown.
#[tokio::test]
async fn test_stream_provider_with_limit() {
    use oxisql_datafusion::OxiSqlStreamProvider;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE nums (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    for i in 0_i64..10 {
        conn.execute(&format!("INSERT INTO nums VALUES ({i})"), &[])
            .await
            .expect("INSERT");
    }

    let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, true)]));
    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let provider = OxiSqlStreamProvider::new(conn_arc, "nums", schema);

    let ctx = SessionContext::new();
    ctx.register_table("nums", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT v FROM nums ORDER BY v LIMIT 5")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 5, "LIMIT 5 should return exactly 5 rows");
}

/// Filter pushdown reduces the number of rows returned from the backend.
#[tokio::test]
async fn test_filter_pushdown_reduces_rows() {
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open");
    conn.execute("CREATE TABLE scores (id INTEGER, score INTEGER)", &[])
        .await
        .expect("create");
    for i in 1i64..=10 {
        conn.execute(
            &format!("INSERT INTO scores (id, score) VALUES ({i}, {})", i * 10),
            &[],
        )
        .await
        .expect("insert");
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("score", DataType::Int64, true),
    ]));

    let provider = oxisql_datafusion::OxiSqlStreamProvider::new(
        Arc::new(conn) as Arc<dyn oxisql_core::Connection>,
        "scores",
        Arc::clone(&schema),
    );

    let ctx = SessionContext::new();
    ctx.register_table("scores", Arc::new(provider))
        .expect("register");
    let df = ctx
        .sql("SELECT id FROM scores WHERE score > 50")
        .await
        .expect("sql");
    let results = df.collect().await.expect("collect");
    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    // score > 50 means id > 5, so 5 rows (id=6,7,8,9,10)
    assert_eq!(total_rows, 5, "filter should reduce to 5 rows");
}

/// Projection pushdown returns only the requested columns.
#[tokio::test]
async fn test_projection_pushdown() {
    use oxisql_datafusion::OxiSqlStreamProvider;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open");
    conn.execute(
        "CREATE TABLE items (id INTEGER, name TEXT, value INTEGER)",
        &[],
    )
    .await
    .expect("create");
    for i in 1i64..=3 {
        conn.execute(
            &format!(
                "INSERT INTO items (id, name, value) VALUES ({i}, 'item{i}', {})",
                i * 100
            ),
            &[],
        )
        .await
        .expect("insert");
    }

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("value", DataType::Int64, true),
    ]));

    let provider = OxiSqlStreamProvider::new(
        Arc::new(conn) as Arc<dyn oxisql_core::Connection>,
        "items",
        Arc::clone(&schema),
    );

    let ctx = SessionContext::new();
    ctx.register_table("items", Arc::new(provider))
        .expect("register");
    // Select only id and name (projection of 2 out of 3 columns).
    let df = ctx.sql("SELECT id, name FROM items").await.expect("sql");
    let results = df.collect().await.expect("collect");
    assert_eq!(results[0].num_columns(), 2);
    assert_eq!(results[0].num_rows(), 3);
}

/// Limit pushdown returns at most N rows from the backend.
#[tokio::test]
async fn test_limit_pushdown() {
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open");
    conn.execute("CREATE TABLE nums (n INTEGER)", &[])
        .await
        .expect("create");
    for i in 1i64..=20 {
        conn.execute(&format!("INSERT INTO nums (n) VALUES ({i})"), &[])
            .await
            .expect("insert");
    }

    let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, true)]));
    let provider = oxisql_datafusion::OxiSqlStreamProvider::new(
        Arc::new(conn) as Arc<dyn oxisql_core::Connection>,
        "nums",
        Arc::clone(&schema),
    );

    let ctx = SessionContext::new();
    ctx.register_table("nums", Arc::new(provider))
        .expect("register");
    let df = ctx.sql("SELECT n FROM nums LIMIT 5").await.expect("sql");
    let results = df.collect().await.expect("collect");
    let total: usize = results.iter().map(|b| b.num_rows()).sum();
    assert!(
        total <= 5,
        "limit should produce at most 5 rows, got {total}"
    );
}

/// `OxiSqlStreamProvider` returns the projected columns only.
#[tokio::test]
async fn test_stream_provider_projection() {
    use oxisql_datafusion::OxiSqlStreamProvider;
    use oxisql_embedded::EmbeddedConnection;

    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE items (id INTEGER, label TEXT, score FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute("INSERT INTO items VALUES (1, 'foo', 1.5)", &[])
        .await
        .expect("INSERT");
    conn.execute("INSERT INTO items VALUES (2, 'bar', 2.5)", &[])
        .await
        .expect("INSERT");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("label", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let provider = OxiSqlStreamProvider::new(conn_arc, "items", schema);

    let ctx = SessionContext::new();
    ctx.register_table("items", Arc::new(provider))
        .expect("register_table");

    // Request only `id` and `label` — the scan should produce 2-column batches.
    let df = ctx
        .sql("SELECT id, label FROM items ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2);
    // All returned batches should have exactly 2 columns.
    for batch in &batches {
        assert_eq!(
            batch.num_columns(),
            2,
            "projected batch must have 2 columns"
        );
    }
}

/// `OxiSqlStreamProvider::from_sqlite` streams SQLite rows through DataFusion.
///
/// Opens an in-memory SQLite database via `SqliteConnection`, inserts a few
/// rows, then registers the table with a DataFusion `SessionContext` using the
/// `from_sqlite` constructor.  Asserts that a `SELECT *` query returns the
/// expected row count and that a filtered query returns the correct subset.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_sqlite_stream_provider() {
    use oxisql_datafusion::stream::OxiSqlStreamProvider;
    use oxisql_sqlite_compat::SqliteConnection;

    let conn = SqliteConnection::open_memory()
        .await
        .expect("open in-memory SQLite");

    conn.execute(
        "CREATE TABLE products (id INTEGER, name TEXT, price FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE products");

    conn.execute("INSERT INTO products VALUES (1, 'Alpha', 9.99)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO products VALUES (2, 'Beta', 19.99)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO products VALUES (3, 'Gamma', 29.99)", &[])
        .await
        .expect("INSERT 3");
    conn.execute("INSERT INTO products VALUES (4, 'Delta', 4.99)", &[])
        .await
        .expect("INSERT 4");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
        Field::new("price", DataType::Float64, true),
    ]));

    let provider = OxiSqlStreamProvider::from_sqlite(conn, "products", Arc::clone(&schema));

    let ctx = SessionContext::new();
    ctx.register_table("products", Arc::new(provider))
        .expect("register_table");

    // Full table scan — all 4 rows expected.
    let df = ctx
        .sql("SELECT * FROM products ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 4, "expected 4 rows from SQLite via DataFusion");

    // Filter pushdown — products with price >= 10.0 (Beta and Gamma).
    let df_filtered = ctx
        .sql("SELECT name FROM products WHERE price >= 10.0 ORDER BY id")
        .await
        .expect("sql parse filtered");
    let batches_filtered = df_filtered.collect().await.expect("collect filtered");
    let filtered_rows: usize = batches_filtered.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        filtered_rows, 2,
        "filter price >= 10.0 should return 2 rows (Beta, Gamma)"
    );
}
