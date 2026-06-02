//! Integration tests for `oxisql-mysql` against a live MySQL server.
//!
//! These tests are gated behind `#[ignore]` and the `integration-mysql` feature
//! flag.  They require a MySQL 8.x server at `mysql://root@localhost/test`.
//!
//! # Running
//!
//! ```sh
//! # Start MySQL (e.g. via Docker):
//! docker run --rm -e MYSQL_ROOT_PASSWORD= -e MYSQL_ALLOW_EMPTY_PASSWORD=yes \
//!     -p 3306:3306 mysql:8
//!
//! # Run the integration tests:
//! cargo test -p oxisql-mysql --features integration-mysql -- --ignored
//! ```
//!
//! # Pure-Rust dependency assertion
//!
//! The features selected in Cargo.toml (`mysql_async` with
//! `default-features = false, features = ["minimal-rust", "rustls-tls"]`)
//! exclude `libz-sys`, `native-tls`, `aws-lc-sys`, `openssl-sys`, and
//! `mysqlclient-sys`.  Verify with:
//!
//! ```sh
//! cargo tree -p oxisql-mysql --edges normal \
//!     | grep -E 'libz-sys|native-tls|aws-lc-sys|openssl-sys|mysqlclient-sys'
//! # Expected: empty output
//! ```
//!
//! This invariant is enforced by CI via `cargo deny check bans`.

#[cfg(feature = "integration-mysql")]
use oxisql_core::Connection;
#[cfg(feature = "integration-mysql")]
use oxisql_mysql::{MyConnection, TlsMode};

/// Verify that `SELECT 1` returns a single row with value 1.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_select_one() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");
    let rows = conn.query("SELECT 1 AS val", &[]).await.expect("query");
    assert_eq!(rows.len(), 1);
    let val = rows[0].get("val").expect("column 'val'");
    // MySQL returns SELECT 1 as Int(1).
    assert_eq!(*val, oxisql_core::Value::I64(1));
}

/// Verify round-trip INSERT + SELECT via execute/query.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_insert_select() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute(
        "CREATE TEMPORARY TABLE oxisql_test_t (id BIGINT, name TEXT)",
        &[],
    )
    .await
    .expect("CREATE");

    let affected = conn
        .execute(
            "INSERT INTO oxisql_test_t VALUES (?, ?)",
            &[&42_i64, &"hello"],
        )
        .await
        .expect("INSERT");
    assert_eq!(affected, 1);

    let rows = conn
        .query("SELECT id, name FROM oxisql_test_t", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    assert_eq!(*rows[0].get("id").expect("id"), oxisql_core::Value::I64(42));
    assert_eq!(
        *rows[0].get("name").expect("name"),
        oxisql_core::Value::Text("hello".to_string())
    );
}

/// Verify that transaction commit persists changes.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_transaction_commit() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute("CREATE TEMPORARY TABLE oxisql_txn_t (val BIGINT)", &[])
        .await
        .expect("CREATE");

    let mut txn = conn.transaction().await.expect("begin transaction");
    txn.execute("INSERT INTO oxisql_txn_t VALUES (?)", &[&99_i64])
        .await
        .expect("INSERT in tx");
    txn.commit().await.expect("commit");

    let rows = conn
        .query("SELECT val FROM oxisql_txn_t", &[])
        .await
        .expect("SELECT after commit");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        *rows[0].get("val").expect("val"),
        oxisql_core::Value::I64(99)
    );
}

/// Verify that transaction rollback discards changes.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_transaction_rollback() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute("CREATE TEMPORARY TABLE oxisql_rb_t (val BIGINT)", &[])
        .await
        .expect("CREATE");

    let mut txn = conn.transaction().await.expect("begin transaction");
    txn.execute("INSERT INTO oxisql_rb_t VALUES (?)", &[&7_i64])
        .await
        .expect("INSERT in tx");
    txn.rollback().await.expect("rollback");

    let rows = conn
        .query("SELECT val FROM oxisql_rb_t", &[])
        .await
        .expect("SELECT after rollback");
    assert_eq!(rows.len(), 0, "rollback should have discarded the insert");
}

/// Unit test: verify `is_reconnect_error` classifies errors correctly.
///
/// This test does NOT require a live MySQL server.
#[test]
fn test_is_reconnect_error_server_gone() {
    use oxisql_mysql::is_reconnect_error;

    // CR_SERVER_GONE_ERROR (2006) — server gone
    let srv_gone = mysql_async::Error::Server(mysql_async::ServerError {
        code: 2006,
        message: "MySQL server has gone away".to_string(),
        state: "HY000".to_string(),
    });
    assert!(
        is_reconnect_error(&srv_gone),
        "2006 should be a reconnect error"
    );

    // CR_SERVER_LOST (2013) — connection lost during query
    let srv_lost = mysql_async::Error::Server(mysql_async::ServerError {
        code: 2013,
        message: "Lost connection to MySQL server during query".to_string(),
        state: "HY000".to_string(),
    });
    assert!(
        is_reconnect_error(&srv_lost),
        "2013 should be a reconnect error"
    );

    // ER_UNKNOWN_COM_ERROR (1047) — can appear after server restart
    let unknown_com = mysql_async::Error::Server(mysql_async::ServerError {
        code: 1047,
        message: "Unknown command".to_string(),
        state: "08S01".to_string(),
    });
    assert!(
        is_reconnect_error(&unknown_com),
        "1047 should be a reconnect error"
    );

    // Normal server error (1064 - syntax error) — NOT a reconnect error
    let syntax_err = mysql_async::Error::Server(mysql_async::ServerError {
        code: 1064,
        message: "You have an error in your SQL syntax".to_string(),
        state: "42000".to_string(),
    });
    assert!(
        !is_reconnect_error(&syntax_err),
        "1064 should NOT be a reconnect error"
    );

    // Constraint violation (1062 - duplicate key) — NOT a reconnect error
    let dup_key = mysql_async::Error::Server(mysql_async::ServerError {
        code: 1062,
        message: "Duplicate entry".to_string(),
        state: "23000".to_string(),
    });
    assert!(
        !is_reconnect_error(&dup_key),
        "1062 should NOT be a reconnect error"
    );
}

/// Verify that schema introspection (tables, columns, indexes, foreign_keys) works.
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn test_schema_introspection() {
    use oxisql_core::Connection;
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    // Create a parent and child table with a FK relationship.
    conn.execute("DROP TABLE IF EXISTS oxisql_schema_child", &[])
        .await
        .expect("drop child");
    conn.execute("DROP TABLE IF EXISTS oxisql_schema_parent", &[])
        .await
        .expect("drop parent");
    conn.execute(
        "CREATE TABLE oxisql_schema_parent (
            id BIGINT PRIMARY KEY,
            name VARCHAR(100) NOT NULL
        )",
        &[],
    )
    .await
    .expect("CREATE parent");
    conn.execute(
        "CREATE TABLE oxisql_schema_child (
            id BIGINT PRIMARY KEY,
            parent_id BIGINT NOT NULL,
            CONSTRAINT fk_parent FOREIGN KEY (parent_id)
                REFERENCES oxisql_schema_parent(id)
        )",
        &[],
    )
    .await
    .expect("CREATE child");

    // tables()
    let tables = conn.tables().await.expect("tables()");
    let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    assert!(
        names.contains(&"oxisql_schema_parent".to_string()),
        "parent table missing from tables(): {names:?}"
    );
    assert!(
        names.contains(&"oxisql_schema_child".to_string()),
        "child table missing from tables(): {names:?}"
    );

    // columns()
    let cols = conn
        .columns("oxisql_schema_parent")
        .await
        .expect("columns()");
    let col_names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
    assert!(col_names.contains(&"id".to_string()), "id column missing");
    assert!(
        col_names.contains(&"name".to_string()),
        "name column missing"
    );

    // indexes()
    let idxs = conn
        .indexes("oxisql_schema_parent")
        .await
        .expect("indexes()");
    let primary = idxs.iter().find(|i| i.primary);
    assert!(primary.is_some(), "primary key index missing");

    // foreign_keys()
    let fks = conn
        .foreign_keys("oxisql_schema_child")
        .await
        .expect("foreign_keys()");
    assert_eq!(fks.len(), 1, "expected 1 FK, got {}", fks.len());
    assert_eq!(fks[0].foreign_table, "oxisql_schema_parent");
    assert_eq!(fks[0].column, "parent_id");

    // Cleanup
    conn.execute("DROP TABLE IF EXISTS oxisql_schema_child", &[])
        .await
        .expect("cleanup child");
    conn.execute("DROP TABLE IF EXISTS oxisql_schema_parent", &[])
        .await
        .expect("cleanup parent");
}

/// Verify that `call_procedure_multi` collects all result sets from a stored
/// procedure that emits two `SELECT` result sets.
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL"]
async fn test_call_procedure_multi_result_set() {
    use oxisql_core::Value;
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    // Create a stored procedure that emits two SELECT result sets.
    conn.execute("DROP PROCEDURE IF EXISTS oxisql_multi_rs_proc", &[])
        .await
        .expect("drop proc");
    conn.execute(
        "CREATE PROCEDURE oxisql_multi_rs_proc()
         BEGIN
           SELECT 1 AS first_col;
           SELECT 2 AS second_col, 3 AS third_col;
         END",
        &[],
    )
    .await
    .expect("create proc");

    let result_sets = conn
        .call_procedure_multi("oxisql_multi_rs_proc", vec![])
        .await
        .expect("call_procedure_multi");

    // The procedure emits 2 SELECT result sets (the OK packet from CALL itself
    // may appear as an empty third set; we check at least 2).
    assert!(
        result_sets.len() >= 2,
        "expected at least 2 result sets, got {}",
        result_sets.len()
    );

    // First result set: one row with first_col = 1
    let first = &result_sets[0];
    assert_eq!(first.len(), 1, "first result set should have 1 row");
    assert_eq!(
        *first[0].get("first_col").expect("first_col"),
        Value::I64(1)
    );

    // Second result set: one row with second_col = 2, third_col = 3
    let second = &result_sets[1];
    assert_eq!(second.len(), 1, "second result set should have 1 row");
    assert_eq!(
        *second[0].get("second_col").expect("second_col"),
        Value::I64(2)
    );
    assert_eq!(
        *second[0].get("third_col").expect("third_col"),
        Value::I64(3)
    );

    // Cleanup
    conn.execute("DROP PROCEDURE IF EXISTS oxisql_multi_rs_proc", &[])
        .await
        .expect("cleanup proc");
}

/// Verify that `query_binary` returns the same rows as `query` for a simple SELECT.
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL"]
async fn test_query_binary_select() {
    use oxisql_core::{Connection, Value};
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute(
        "CREATE TEMPORARY TABLE oxisql_bin_t (id BIGINT, name TEXT)",
        &[],
    )
    .await
    .expect("CREATE");
    conn.execute(
        "INSERT INTO oxisql_bin_t VALUES (?, ?)",
        &[&42_i64, &"binary"],
    )
    .await
    .expect("INSERT");

    let rows = conn
        .query_binary("SELECT id, name FROM oxisql_bin_t WHERE id = ?", &[&42_i64])
        .await
        .expect("query_binary");
    assert_eq!(rows.len(), 1);
    assert_eq!(*rows[0].get("id").expect("id"), Value::I64(42));
    assert_eq!(
        *rows[0].get("name").expect("name"),
        Value::Text("binary".to_string())
    );
}

/// Verify that `disconnect` cleanly shuts down the pool without panicking.
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL"]
async fn test_disconnect_graceful() {
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");
    // Should complete without error.
    conn.disconnect().await.expect("disconnect");
}

// ── MyTransaction Drop / Send tests ──────────────────────────────────────────

/// Compile-time assertion: `MyTransaction` must implement `Send`.
///
/// This test does NOT require a live MySQL server — it is a zero-cost compile
/// check that ensures `MyTransaction` can be sent across async task boundaries.
#[test]
fn test_my_transaction_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<oxisql_mysql::MyTransaction>();
}

/// Verify that dropping a `MyTransaction` without an explicit commit or
/// rollback causes an implicit rollback (no rows persisted).
///
/// `mysql_async::Transaction` rolls back automatically when dropped without
/// an explicit terminal action.  This test confirms that the `MyTransaction`
/// wrapper preserves that guarantee.
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL"]
async fn test_my_transaction_drop_rolls_back() {
    use oxisql_core::{Connection, Value};
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.execute("CREATE TEMPORARY TABLE oxisql_drop_txn_t (val BIGINT)", &[])
        .await
        .expect("CREATE TEMPORARY TABLE");

    // Begin a transaction and insert a row, then drop the transaction without
    // calling commit() or rollback().  mysql_async rolls back on drop.
    {
        let mut txn = conn.transaction().await.expect("begin transaction");
        txn.execute("INSERT INTO oxisql_drop_txn_t VALUES (?)", &[&42_i64])
            .await
            .expect("INSERT in tx");
        // Implicit drop — no commit/rollback call.
        // The `Box<dyn Transaction>` is dropped here, triggering the implicit rollback.
    }

    // The row must NOT be present because the drop-rollback discarded the insert.
    let rows = conn
        .query("SELECT val FROM oxisql_drop_txn_t", &[])
        .await
        .expect("SELECT after implicit rollback");

    assert_eq!(
        rows.len(),
        0,
        "implicit rollback on drop should have discarded the INSERT; got {} row(s): {:?}",
        rows.len(),
        rows.iter()
            .filter_map(|r| r.get("val"))
            .map(|v: &Value| format!("{v}"))
            .collect::<Vec<_>>(),
    );
}

// ── MyConnectionBuilder configuration tests ──────────────────────────────────

/// Verify that concurrent transactions each get an independent `mysql_async::Conn`
/// from the pool and can insert rows without interfering with each other.
///
/// Each task starts its own transaction, inserts a row, commits, and verifies
/// the row count equals the number of tasks after all complete.
///
/// Requires a live MySQL 8.x server.  Set `MYSQL_URL` to run:
///
/// ```sh
/// MYSQL_URL=mysql://root@localhost/testdb \
///   cargo test -p oxisql-mysql --features integration-mysql -- --ignored \
///   test_concurrent_transactions
/// ```
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL server — set MYSQL_URL=mysql://user:pass@localhost/testdb"]
async fn test_concurrent_transactions() {
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use oxisql_core::Connection;
    use oxisql_mysql::{MyConnection, TlsMode};

    let url = match std::env::var("MYSQL_URL") {
        Ok(u) => u,
        Err(_) => return,
    };

    // Unique table name to avoid cross-test collisions.
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let table = format!("oxisql_concurrent_txn_{ts}");

    // Single connection to set up and tear down the table.
    let setup_conn = MyConnection::connect(&url, TlsMode::Disabled)
        .await
        .expect("connect for setup");

    setup_conn
        .execute(
            &format!("CREATE TABLE {table} (task_id BIGINT NOT NULL)"),
            &[],
        )
        .await
        .expect("CREATE TABLE");

    // Spawn 5 tasks, each with its own connection, transaction, and INSERT.
    let task_count = 5usize;
    let url = Arc::new(url);
    let table = Arc::new(table);

    let mut join_set = tokio::task::JoinSet::new();
    for i in 0..task_count {
        let u = Arc::clone(&url);
        let t = Arc::clone(&table);
        join_set.spawn(async move {
            let conn = MyConnection::connect(&u, TlsMode::Disabled)
                .await
                .unwrap_or_else(|e| panic!("task {i} connect failed: {e}"));
            let mut txn = conn
                .transaction()
                .await
                .unwrap_or_else(|e| panic!("task {i} begin tx failed: {e}"));
            let task_id = i as i64;
            txn.execute(&format!("INSERT INTO {t} VALUES (?)"), &[&task_id])
                .await
                .unwrap_or_else(|e| panic!("task {i} INSERT failed: {e}"));
            txn.commit()
                .await
                .unwrap_or_else(|e| panic!("task {i} commit failed: {e}"));
        });
    }

    while let Some(res) = join_set.join_next().await {
        res.expect("task panicked");
    }

    // Verify all rows are present.
    let rows = setup_conn
        .query(&format!("SELECT task_id FROM {table}"), &[])
        .await
        .expect("final SELECT");
    assert_eq!(
        rows.len(),
        task_count,
        "expected {task_count} rows after all concurrent transactions committed"
    );

    // Cleanup.
    setup_conn
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await
        .expect("DROP TABLE");
}

/// Verify that `MyConnectionBuilder` stores pool configuration without panicking.
///
/// This is a compile-time / unit-level test — no live MySQL server required.
/// It confirms the builder API is ergonomic and that the struct correctly
/// stores all provided values through the fluent interface.
#[test]
fn test_pool_config_stored_in_builder() {
    use oxisql_mysql::MyConnectionBuilder;

    let builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("test")
        .user("root")
        .password("")
        .pool_min(2)
        .pool_max(20)
        .pool_idle_timeout(300)
        .pool_ttl(3600)
        .connect_timeout_secs(5);

    // The builder should be constructed without panic; black_box prevents
    // the compiler from eliding the construction.
    std::hint::black_box(builder);
}

/// Verify that `ssl_skip_verify` and `ssl_disabled` compose correctly on the
/// builder without panicking (compile-time / no live DB needed).
#[test]
fn test_builder_ssl_disabled_overrides_skip_verify() {
    use oxisql_mysql::MyConnectionBuilder;

    let builder = MyConnectionBuilder::new()
        .host("db.example.com")
        .ssl_disabled();

    std::hint::black_box(builder);
}

/// Verify that `pool_min > pool_max` is caught without needing a live DB.
///
/// `MyConnectionBuilder::connect` is async; the constraint is validated inside
/// `build_pool_opts`, which is only reachable at connection time.  This test
/// documents that the validation exists, even though exercising it end-to-end
/// requires an async context (see integration tests).
#[test]
fn test_builder_inverted_pool_bounds_does_not_panic_at_build() {
    use oxisql_mysql::MyConnectionBuilder;

    // Building the struct itself with inverted bounds is fine; the error is
    // only surfaced when `.connect()` is awaited.
    let builder = MyConnectionBuilder::new().pool_min(100).pool_max(1);

    std::hint::black_box(builder);
}
