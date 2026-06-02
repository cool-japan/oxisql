//! Live-backend DataFusion integration tests.
//!
//! These tests require a live Postgres or MySQL instance and are gated with
//! `#[ignore]`.  Run with:
//!   cargo test -p oxisql-datafusion --all-features -- --ignored

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use oxisql_core::Connection;

/// Register a live PostgreSQL connection as a DataFusion table and execute a
/// filtered query via the OxiSQL stream provider.
///
/// Requires a PostgreSQL server accepting connections at
/// `host=localhost user=postgres password=postgres dbname=testdb`.
#[tokio::test]
#[ignore] // requires live Postgres — run with: cargo test -- --ignored
async fn test_datafusion_with_postgres_backend() {
    use arrow::array::StringArray;
    use oxisql_datafusion::{OxiSqlContext, OxiSqlStreamProvider};
    use oxisql_postgres::{PgConnection, TlsMode};

    let conn = PgConnection::connect(
        "host=localhost user=postgres password=postgres dbname=testdb",
        TlsMode::Disabled,
    )
    .await
    .expect("connect to Postgres — ensure a local testdb is running");

    conn.execute("CREATE TEMP TABLE df_pg_test (id BIGINT, name TEXT)", &[])
        .await
        .expect("CREATE TEMP TABLE df_pg_test");

    conn.execute(
        "INSERT INTO df_pg_test VALUES (1, 'Alice'), (2, 'Bob')",
        &[],
    )
    .await
    .expect("INSERT INTO df_pg_test");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let provider = OxiSqlStreamProvider::new(conn_arc, "df_pg_test", schema);

    let ctx = OxiSqlContext::new();
    ctx.session_context()
        .register_table("pg_test", Arc::new(provider))
        .expect("register pg_test in DataFusion");

    let results = ctx
        .execute_sql("SELECT name FROM pg_test WHERE id = 1")
        .await
        .expect("execute SELECT on pg_test");

    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "WHERE id=1 should return exactly 1 row");

    // Verify the name column contains 'Alice'.
    let name_col = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column should be a StringArray");
    assert_eq!(name_col.value(0), "Alice", "first row name must be Alice");
}

/// Register a live MySQL connection as a DataFusion table and execute a
/// filtered query via the OxiSQL stream provider.
///
/// Requires a MySQL server accepting connections at
/// `mysql://root:root@localhost:3306/testdb`.
#[tokio::test]
#[ignore] // requires live MySQL — run with: cargo test -- --ignored
async fn test_datafusion_with_mysql_backend() {
    use arrow::array::StringArray;
    use oxisql_datafusion::{OxiSqlContext, OxiSqlStreamProvider};
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root:root@localhost:3306/testdb", TlsMode::Disabled)
        .await
        .expect("connect to MySQL — ensure a local testdb is running");

    // Use a unique table name to avoid collisions with concurrent tests.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS df_my_test (id BIGINT, name VARCHAR(255))",
        &[],
    )
    .await
    .expect("CREATE TABLE df_my_test");

    // Ensure a clean slate before inserting.
    conn.execute("DELETE FROM df_my_test", &[])
        .await
        .expect("DELETE FROM df_my_test");

    conn.execute(
        "INSERT INTO df_my_test VALUES (1, 'Charlie'), (2, 'Diana')",
        &[],
    )
    .await
    .expect("INSERT INTO df_my_test");

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
    ]));

    let conn_arc = Arc::new(conn) as Arc<dyn oxisql_core::Connection>;
    let provider = OxiSqlStreamProvider::new(conn_arc, "df_my_test", schema);

    let ctx = OxiSqlContext::new();
    ctx.session_context()
        .register_table("my_test", Arc::new(provider))
        .expect("register my_test in DataFusion");

    let results = ctx
        .execute_sql("SELECT name FROM my_test WHERE id = 2")
        .await
        .expect("execute SELECT on my_test");

    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "WHERE id=2 should return exactly 1 row");

    // Verify the name column contains 'Diana'.
    let name_col = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("name column should be a StringArray");
    assert_eq!(name_col.value(0), "Diana", "second row name must be Diana");
}

/// Test `OxiSqlStreamProvider::from_postgres` convenience constructor.
///
/// Verifies that a `PgConnection` can be passed directly — without boxing it
/// manually — and the resulting provider executes a pushed-down filter query.
///
/// Requires a PostgreSQL server at:
///   `host=localhost user=postgres password=postgres dbname=testdb`
#[cfg(feature = "postgres")]
#[tokio::test]
#[ignore = "requires live Postgres at host=localhost user=postgres password=postgres dbname=testdb"]
async fn test_from_postgres_constructor() {
    use arrow::array::Int64Array;
    use oxisql_datafusion::{OxiSqlContext, OxiSqlStreamProvider};
    use oxisql_postgres::{PgConnection, TlsMode};

    let conn = PgConnection::connect(
        "host=localhost user=postgres password=postgres dbname=testdb",
        TlsMode::Disabled,
    )
    .await
    .expect("connect to Postgres — ensure testdb is running");

    conn.execute(
        "CREATE TEMP TABLE df_from_pg (id BIGINT, score DOUBLE PRECISION)",
        &[],
    )
    .await
    .expect("CREATE TEMP TABLE df_from_pg");

    conn.execute(
        "INSERT INTO df_from_pg VALUES (10, 1.5), (20, 2.5), (30, 3.5)",
        &[],
    )
    .await
    .expect("INSERT INTO df_from_pg");

    let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("id", arrow::datatypes::DataType::Int64, true),
        arrow::datatypes::Field::new("score", arrow::datatypes::DataType::Float64, true),
    ]));

    // Use the convenience constructor — no manual Arc::new() wrapping needed.
    let provider = OxiSqlStreamProvider::from_postgres(conn, "df_from_pg", schema);

    let ctx = OxiSqlContext::new();
    ctx.session_context()
        .register_table("from_pg", std::sync::Arc::new(provider))
        .expect("register from_pg");

    let results = ctx
        .execute_sql("SELECT id FROM from_pg WHERE id > 15 ORDER BY id")
        .await
        .expect("SELECT from from_pg");

    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "WHERE id>15 should return rows 20 and 30");

    let id_col = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("id column should be Int64Array");
    assert_eq!(id_col.value(0), 20, "first matching id must be 20");
}

/// Test `OxiSqlStreamProvider::from_mysql` convenience constructor.
///
/// Verifies that a `MyConnection` can be passed directly — without boxing it
/// manually — and the resulting provider executes a pushed-down filter query.
///
/// Requires a MySQL server at: `mysql://root:root@localhost:3306/testdb`
#[cfg(feature = "mysql")]
#[tokio::test]
#[ignore = "requires live MySQL at mysql://root:root@localhost:3306/testdb"]
async fn test_from_mysql_constructor() {
    use arrow::array::Int64Array;
    use oxisql_datafusion::{OxiSqlContext, OxiSqlStreamProvider};
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root:root@localhost:3306/testdb", TlsMode::Disabled)
        .await
        .expect("connect to MySQL — ensure testdb is running");

    conn.execute(
        "CREATE TABLE IF NOT EXISTS df_from_my (id BIGINT, score DOUBLE)",
        &[],
    )
    .await
    .expect("CREATE TABLE df_from_my");

    conn.execute("DELETE FROM df_from_my", &[])
        .await
        .expect("DELETE FROM df_from_my");

    conn.execute(
        "INSERT INTO df_from_my VALUES (10, 1.5), (20, 2.5), (30, 3.5)",
        &[],
    )
    .await
    .expect("INSERT INTO df_from_my");

    let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(vec![
        arrow::datatypes::Field::new("id", arrow::datatypes::DataType::Int64, true),
        arrow::datatypes::Field::new("score", arrow::datatypes::DataType::Float64, true),
    ]));

    // Use the convenience constructor — no manual Arc::new() wrapping needed.
    let provider = OxiSqlStreamProvider::from_mysql(conn, "df_from_my", schema);

    let ctx = OxiSqlContext::new();
    ctx.session_context()
        .register_table("from_my", std::sync::Arc::new(provider))
        .expect("register from_my");

    let results = ctx
        .execute_sql("SELECT id FROM from_my WHERE id < 25 ORDER BY id")
        .await
        .expect("SELECT from from_my");

    let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "WHERE id<25 should return rows 10 and 20");

    let id_col = results[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("id column should be Int64Array");
    assert_eq!(id_col.value(0), 10, "first matching id must be 10");
}
