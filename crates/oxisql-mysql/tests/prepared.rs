//! Integration tests for MySQL prepared statement support.
//!
//! These tests are gated behind `#[cfg(feature = "integration-mysql")]` and
//! `#[ignore]`.  They require a MySQL 8.x server accessible at
//! `mysql://root@localhost/test`.
//!
//! # Running
//!
//! ```sh
//! # Start MySQL (e.g. via Docker):
//! docker run --rm -e MYSQL_ALLOW_EMPTY_PASSWORD=yes \
//!     -p 3306:3306 mysql:8
//!
//! # Run the prepared-statement integration tests:
//! cargo test -p oxisql-mysql --features integration-mysql -- --ignored
//! ```

#[cfg(feature = "integration-mysql")]
use oxisql_core::Connection;
#[cfg(feature = "integration-mysql")]
use oxisql_mysql::{MyConnection, TlsMode};

/// Prepare a `SELECT` with no parameters and call `query()`.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_prepare_query_no_params() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    let mut stmt = conn
        .prepare("SELECT 1 AS val")
        .await
        .expect("prepare SELECT 1");

    let rows = stmt.query(&[]).await.expect("query with no params");
    assert_eq!(rows.len(), 1, "expected one row");
    let val = rows[0].get("val").expect("column 'val'");
    assert_eq!(*val, oxisql_core::Value::I64(1));
}

/// Prepare a parameterised `SELECT` with a `?` placeholder and call `query()`
/// with multiple different parameter sets.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_prepare_parameterized_query() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute(
        "CREATE TEMPORARY TABLE prep_query_t (id BIGINT, label TEXT)",
        &[],
    )
    .await
    .expect("CREATE TEMPORARY TABLE");

    conn.execute(
        "INSERT INTO prep_query_t VALUES (1, 'alpha'), (2, 'beta'), (3, 'gamma')",
        &[],
    )
    .await
    .expect("INSERT seed rows");

    let mut stmt = conn
        .prepare("SELECT id, label FROM prep_query_t WHERE id = ?")
        .await
        .expect("prepare SELECT");

    // First parameter set
    let rows = stmt.query(&[&1_i64]).await.expect("query id=1");
    assert_eq!(rows.len(), 1);
    assert_eq!(*rows[0].get("id").expect("id"), oxisql_core::Value::I64(1));
    assert_eq!(
        *rows[0].get("label").expect("label"),
        oxisql_core::Value::Text("alpha".to_string())
    );

    // Second parameter set — same prepared statement, different value
    let rows = stmt.query(&[&3_i64]).await.expect("query id=3");
    assert_eq!(rows.len(), 1);
    assert_eq!(*rows[0].get("id").expect("id"), oxisql_core::Value::I64(3));
    assert_eq!(
        *rows[0].get("label").expect("label"),
        oxisql_core::Value::Text("gamma".to_string())
    );

    // Parameter set that matches no rows
    let rows = stmt.query(&[&99_i64]).await.expect("query id=99");
    assert_eq!(rows.len(), 0, "no rows expected for id=99");
}

/// Prepare an `INSERT` with `?` placeholders and call `execute()` multiple
/// times, verifying the affected-rows count each time.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_prepare_insert_execute() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute(
        "CREATE TEMPORARY TABLE prep_insert_t (id BIGINT, name TEXT)",
        &[],
    )
    .await
    .expect("CREATE TEMPORARY TABLE");

    let mut stmt = conn
        .prepare("INSERT INTO prep_insert_t (id, name) VALUES (?, ?)")
        .await
        .expect("prepare INSERT");

    let affected = stmt
        .execute(&[&10_i64, &"first"])
        .await
        .expect("execute first INSERT");
    assert_eq!(affected, 1, "expected 1 affected row");

    let affected = stmt
        .execute(&[&20_i64, &"second"])
        .await
        .expect("execute second INSERT");
    assert_eq!(affected, 1, "expected 1 affected row");

    let affected = stmt
        .execute(&[&30_i64, &"third"])
        .await
        .expect("execute third INSERT");
    assert_eq!(affected, 1, "expected 1 affected row");

    // Verify all three rows were inserted
    let rows = conn
        .query("SELECT COUNT(*) AS cnt FROM prep_insert_t", &[])
        .await
        .expect("SELECT COUNT");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        *rows[0].get("cnt").expect("cnt"),
        oxisql_core::Value::I64(3)
    );
}

/// Verify that `PreparedStatement::sql()` returns the exact SQL text that was
/// passed to `prepare()`.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_prepare_sql_accessor() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    let sql = "SELECT ? AS x";
    let stmt = conn.prepare(sql).await.expect("prepare");

    assert_eq!(
        stmt.sql(),
        sql,
        "sql() must return the original SQL text verbatim"
    );
}
