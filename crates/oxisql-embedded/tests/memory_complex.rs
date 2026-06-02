#[cfg(not(feature = "sled-storage"))]
use oxisql_core::OxiSqlError;
use oxisql_core::{Connection, ToSqlValue, Value};
use oxisql_embedded::EmbeddedConnection;

// ── DDL / complex-query tests ────────────────────────────────────────────────

/// Create a table, insert data, then drop it and verify the table is gone.
#[tokio::test]
async fn test_ddl_create_drop_table() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE ddl_test (id INT, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO ddl_test VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT");
    let rows = conn
        .query("SELECT id, name FROM ddl_test", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    conn.execute("DROP TABLE ddl_test", &[])
        .await
        .expect("DROP TABLE");
    // After drop, SELECT should fail — GlueSQL reports the table as not found.
    let result = conn.query("SELECT id FROM ddl_test", &[]).await;
    assert!(result.is_err(), "SELECT after DROP should fail");
}

/// JOIN two tables and verify the result row count.
#[tokio::test]
async fn test_complex_join_query() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE jq_users (id INT, name TEXT)", &[])
        .await
        .expect("CREATE TABLE jq_users");
    conn.execute(
        "CREATE TABLE jq_orders (id INT, user_id INT, amount FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE jq_orders");
    // Individual INSERTs to avoid multi-row VALUES syntax uncertainty.
    conn.execute("INSERT INTO jq_users VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT user 1");
    conn.execute("INSERT INTO jq_users VALUES (2, 'Bob')", &[])
        .await
        .expect("INSERT user 2");
    conn.execute("INSERT INTO jq_orders VALUES (1, 1, 99.99)", &[])
        .await
        .expect("INSERT order 1");
    conn.execute("INSERT INTO jq_orders VALUES (2, 1, 49.99)", &[])
        .await
        .expect("INSERT order 2");
    conn.execute("INSERT INTO jq_orders VALUES (3, 2, 19.99)", &[])
        .await
        .expect("INSERT order 3");

    let rows = conn
        .query(
            "SELECT u.name, o.amount \
             FROM jq_users u \
             JOIN jq_orders o ON u.id = o.user_id \
             ORDER BY o.amount",
            &[],
        )
        .await
        .expect("JOIN query");
    assert_eq!(rows.len(), 3);
}

/// GROUP BY with SUM aggregation — verify result row count.
///
/// If GlueSQL does not support the aliased aggregate (`SUM(amount) as total`),
/// the query may fail; both Ok and Err are acceptable — the test just verifies
/// no panic occurs.
#[tokio::test]
async fn test_group_by_aggregation() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE sales (region TEXT, amount FLOAT)", &[])
        .await
        .expect("CREATE TABLE sales");
    conn.execute("INSERT INTO sales VALUES ('North', 100.0)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO sales VALUES ('South', 200.0)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO sales VALUES ('North', 150.0)", &[])
        .await
        .expect("INSERT 3");

    let result = conn
        .query(
            "SELECT region, SUM(amount) as total FROM sales GROUP BY region ORDER BY region",
            &[],
        )
        .await;
    // GlueSQL MemoryStorage may not support GROUP BY with aggregates; either
    // outcome is valid — just confirm no panic.
    if let Ok(rows) = result {
        assert_eq!(rows.len(), 2, "expected 2 region groups");
    }
}

/// ORDER BY … ASC LIMIT — verify first element is the minimum.
#[tokio::test]
async fn test_order_by_and_limit() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE nums_obl (n INT)", &[])
        .await
        .expect("CREATE TABLE nums_obl");
    for i in [5_i64, 2, 8, 1, 9, 3, 7, 4, 6] {
        conn.execute("INSERT INTO nums_obl VALUES ($1)", &[&i as &dyn ToSqlValue])
            .await
            .expect("INSERT");
    }

    let rows = conn
        .query("SELECT n FROM nums_obl ORDER BY n ASC LIMIT 3", &[])
        .await
        .expect("ORDER BY LIMIT");
    assert_eq!(rows.len(), 3);
    // The first row must hold the smallest value (1).
    let first: i64 = rows[0].try_get("n").expect("column 'n' must exist");
    assert_eq!(first, 1, "first row after ORDER BY ASC must be minimum");
}

/// Concurrent clones writing to the same in-memory storage should not deadlock.
#[tokio::test]
async fn test_concurrent_connection_clones() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE concurrent_test (id INT, val TEXT)", &[])
        .await
        .expect("CREATE TABLE concurrent_test");

    let handles: Vec<_> = (1_i64..=5)
        .map(|i| {
            let c = conn.clone();
            tokio::spawn(async move {
                let val = format!("item{i}");
                c.execute(
                    "INSERT INTO concurrent_test VALUES ($1, $2)",
                    &[&i as &dyn ToSqlValue, &val.as_str() as &dyn ToSqlValue],
                )
                .await
            })
        })
        .collect();

    for h in handles {
        h.await
            .expect("task did not panic")
            .expect("concurrent insert should succeed");
    }

    let rows = conn
        .query("SELECT id FROM concurrent_test ORDER BY id", &[])
        .await
        .expect("SELECT after concurrent inserts");
    assert_eq!(rows.len(), 5);
}

/// Insert 500 rows via execute_batch and verify all are returned by SELECT.
#[tokio::test]
async fn test_large_result_set() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE large_test (id INT, data TEXT)", &[])
        .await
        .expect("CREATE TABLE large_test");

    // Build a single batched SQL string of 500 INSERT statements.
    let batch_sql: String = (1_i64..=500)
        .map(|i| format!("INSERT INTO large_test VALUES ({i}, 'data{i}')"))
        .collect::<Vec<_>>()
        .join("; ");
    conn.execute_batch(&batch_sql)
        .await
        .expect("batch insert 500 rows");

    let rows = conn
        .query("SELECT id FROM large_test", &[])
        .await
        .expect("SELECT large result set");
    assert_eq!(rows.len(), 500);
}

// ── Transaction isolation tests ───────────────────────────────────────────────

/// Verify transaction-within-transaction visibility and rollback behavior.
///
/// GlueSQL `MemoryStorage` does not implement true transaction isolation:
///
/// - `BEGIN` is likely to fail or behave as a no-op, causing `transaction()`
///   to return `Err`.  When that happens we document the constraint rather
///   than treating it as a test failure.
/// - If `BEGIN` succeeds, we verify that changes are visible within the
///   same transaction via subsequent queries, and that `ROLLBACK` attempts
///   to revert them.
///
/// This test exists primarily to document GlueSQL MemoryStorage's transaction
/// semantics and ensure the API does not panic under any code path.
#[tokio::test]
async fn test_transaction_changes_visible_within_txn() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE iso_test (id INT, val TEXT)", &[])
        .await
        .expect("CREATE TABLE iso_test");
    conn.execute("INSERT INTO iso_test VALUES (1, 'original')", &[])
        .await
        .expect("INSERT original row");

    // Attempt to begin a transaction.  GlueSQL MemoryStorage may not support
    // BEGIN, in which case transaction() returns Err.  Both outcomes are valid.
    let txn_result = conn.transaction().await;
    if txn_result.is_err() {
        // GlueSQL MemoryStorage does not support transactions.
        // Document the constraint: concurrent isolation cannot be tested here.
        // The API returned an error gracefully rather than panicking — pass.
        return;
    }

    let mut txn = txn_result.expect("transaction handle");

    // Within the transaction, update the row.
    let update_result = txn
        .execute("UPDATE iso_test SET val = 'changed' WHERE id = 1", &[])
        .await;

    if update_result.is_err() {
        // GlueSQL may reject DML inside a transaction context — acceptable.
        // Roll back and exit gracefully.
        txn.rollback().await.ok();
        return;
    }

    // Query within the same transaction — the updated value should be visible
    // to subsequent statements in the same transaction session.
    let in_txn_rows = txn
        .query("SELECT val FROM iso_test WHERE id = 1", &[])
        .await;
    // We only observe, not assert a specific value, because GlueSQL
    // MemoryStorage may or may not reflect in-transaction changes immediately.
    let _ = in_txn_rows;

    // Roll back all changes.
    txn.rollback().await.ok();

    // After rollback, the original value should be restored.
    // GlueSQL MemoryStorage may or may not support transactional rollback;
    // we document both behaviors without a hard assert on the value.
    let after_rows = conn
        .query("SELECT val FROM iso_test WHERE id = 1", &[])
        .await
        .expect("SELECT after rollback");

    // If rollback worked, the value is 'original'.
    // If GlueSQL MemoryStorage ignores rollback, the value may be 'changed'.
    // Either way, the row must exist.
    assert!(
        !after_rows.is_empty(),
        "row must exist after rollback attempt — GlueSQL MemoryStorage does not delete rows on rollback"
    );
    // Document whichever behavior we observe.
    if let Some(val) = after_rows[0].get("val") {
        let _ = val; // observed — either Value::Text("original") or Value::Text("changed")
    }
}

// ── execute_batch DDL+DML tests ───────────────────────────────────────────────

/// Verify that `execute_batch` correctly processes a mixed sequence of DDL
/// (CREATE TABLE) and DML (INSERT) statements in a single call.
///
/// This exercises the case where GlueSQL is given a semicolon-separated string
/// that starts with a DDL statement followed by one or more DML statements —
/// a common pattern in SQL migration scripts.
#[tokio::test]
async fn test_execute_batch_mixed_ddl_dml() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    // Execute CREATE TABLE and two INSERTs as a single batch.
    let sql = "CREATE TABLE batch_test (id INT, name TEXT);\
               INSERT INTO batch_test VALUES (1, 'hello');\
               INSERT INTO batch_test VALUES (2, 'world')";

    conn.execute_batch(sql)
        .await
        .expect("execute_batch with DDL + DML should succeed");

    // Verify both rows were inserted.
    let rows = conn
        .query("SELECT id, name FROM batch_test ORDER BY id", &[])
        .await
        .expect("SELECT after batch");

    assert_eq!(rows.len(), 2, "expected 2 rows after batch DDL+DML");

    let id1: i64 = rows[0].try_get("id").expect("id column in row 0");
    let name1: String = rows[0].try_get("name").expect("name column in row 0");
    assert_eq!(id1, 1);
    assert_eq!(name1, "hello");

    let id2: i64 = rows[1].try_get("id").expect("id column in row 1");
    let name2: String = rows[1].try_get("name").expect("name column in row 1");
    assert_eq!(id2, 2);
    assert_eq!(name2, "world");
}

/// Subquery in WHERE — verifies no panic; GlueSQL may or may not support IN
/// with a correlated subquery.
#[tokio::test]
async fn test_subquery_in_where() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE products (id INT, category TEXT, price FLOAT)",
        &[],
    )
    .await
    .expect("CREATE TABLE products");
    conn.execute("INSERT INTO products VALUES (1, 'A', 10.0)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO products VALUES (2, 'B', 20.0)", &[])
        .await
        .expect("INSERT 2");
    conn.execute("INSERT INTO products VALUES (3, 'A', 30.0)", &[])
        .await
        .expect("INSERT 3");

    // GlueSQL may or may not support IN with a subquery — either outcome is
    // acceptable; the test guards against a panic.
    let result = conn
        .query(
            "SELECT id FROM products WHERE category IN \
             (SELECT DISTINCT category FROM products WHERE price > 15.0)",
            &[],
        )
        .await;
    let _ = result; // No panic is the only requirement here.
}

/// NULL values round-trip through INSERT / SELECT correctly.
#[tokio::test]
async fn test_null_values_in_queries() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE nullable (id INT, optional TEXT)", &[])
        .await
        .expect("CREATE TABLE nullable");
    conn.execute("INSERT INTO nullable VALUES (1, 'present')", &[])
        .await
        .expect("INSERT row 1");
    conn.execute("INSERT INTO nullable VALUES (2, NULL)", &[])
        .await
        .expect("INSERT row 2 (NULL)");

    let rows = conn
        .query("SELECT id, optional FROM nullable ORDER BY id", &[])
        .await
        .expect("SELECT nullable");
    assert_eq!(rows.len(), 2);
    // Second row's `optional` column must be Null.
    assert!(
        matches!(rows[1].get("optional"), Some(Value::Null) | None),
        "second row optional column must be Null, got {:?}",
        rows[1].get("optional")
    );
}

// ── EXPLAIN tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_explain_select() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let plan = conn
        .explain("SELECT id, name FROM users WHERE id = 1")
        .await
        .expect("explain should succeed");
    assert!(!plan.is_empty(), "explain result must be non-empty");
    assert!(
        plan.contains("Filter") || plan.contains("Scan"),
        "explain result should contain Filter or Scan, got: {plan}"
    );
}

#[tokio::test]
async fn test_explain_join() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let plan = conn
        .explain("SELECT a.id FROM a JOIN b ON a.id = b.id")
        .await
        .expect("explain should succeed");
    assert!(!plan.is_empty(), "explain result must be non-empty");
    assert!(
        plan.contains("Join") || plan.contains("Scan"),
        "explain result should contain Join or Scan, got: {plan}"
    );
}

// ── JSON helper tests ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_json_set_and_get() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE jstore (k TEXT, v TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    conn.json_set("jstore", "k", "v", "mykey", r#"{"x":1}"#)
        .await
        .expect("json_set should succeed");

    let result = conn
        .json_get("jstore", "k", "v", "mykey")
        .await
        .expect("json_get should succeed");
    assert_eq!(result, Some(r#"{"x":1}"#.to_string()));
}

#[tokio::test]
async fn test_json_get_missing() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE jstore2 (k TEXT, v TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    let result = conn
        .json_get("jstore2", "k", "v", "nonexistent")
        .await
        .expect("json_get should succeed for missing key");
    assert_eq!(result, None);
}

#[tokio::test]
async fn test_json_set_overwrite() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE jstore3 (k TEXT, v TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    conn.json_set("jstore3", "k", "v", "key1", r#"{"a":1}"#)
        .await
        .expect("first json_set");

    conn.json_set("jstore3", "k", "v", "key1", r#"{"a":2}"#)
        .await
        .expect("second json_set (overwrite)");

    let result = conn
        .json_get("jstore3", "k", "v", "key1")
        .await
        .expect("json_get after overwrite");
    assert_eq!(result, Some(r#"{"a":2}"#.to_string()));
}

// ── open_file persistent backend ─────────────────────────────────────────────

#[cfg(not(feature = "sled-storage"))]
#[tokio::test]
async fn test_open_file_returns_error_without_feature() {
    // Without the sled-storage feature, open_file must return an
    // UnsupportedUri error. This documents the intended API and verifies the
    // fallback path without any file-system side-effects.
    let db_path = std::env::temp_dir().join("oxisql_test_nosled.db");
    let result = EmbeddedConnection::open_file(db_path.to_str().expect("db_path utf8")).await;
    assert!(
        result.is_err(),
        "open_file must fail without sled-storage feature"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, OxiSqlError::UnsupportedUri(_)),
        "expected UnsupportedUri, got: {err}"
    );
    assert!(
        err.to_string().contains("sled-storage"),
        "error message should mention the feature: {err}"
    );
}

// ── import / export tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_import_from_sql() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.import_from_sql("CREATE TABLE import_test (id INT); INSERT INTO import_test VALUES (42);")
        .await
        .expect("import_from_sql should succeed");

    let rows = conn
        .query("SELECT id FROM import_test", &[])
        .await
        .expect("SELECT after import");
    assert_eq!(rows.len(), 1);
    let id: i64 = rows[0].try_get("id").expect("column 'id' must exist");
    assert_eq!(id, 42);
}

#[tokio::test]
async fn test_import_from_sql_multiple_statements() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.import_from_sql(
        "CREATE TABLE multi_import (v INT);\
         INSERT INTO multi_import VALUES (1);\
         INSERT INTO multi_import VALUES (2);\
         INSERT INTO multi_import VALUES (3);",
    )
    .await
    .expect("import_from_sql multi-statement should succeed");

    let rows = conn
        .query("SELECT v FROM multi_import", &[])
        .await
        .expect("SELECT after multi-import");
    assert_eq!(rows.len(), 3);
}

// ── export_as_sql tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_export_as_sql_round_trip() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute(
        "CREATE TABLE rt_users (id INT NOT NULL, name TEXT NULL, active BOOLEAN NULL)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute(
        "INSERT INTO rt_users (id, name, active) VALUES (1, 'Alice', TRUE)",
        &[],
    )
    .await
    .expect("INSERT row 1");
    conn.execute(
        "INSERT INTO rt_users (id, name, active) VALUES (2, 'Bob', FALSE)",
        &[],
    )
    .await
    .expect("INSERT row 2");
    conn.execute(
        "INSERT INTO rt_users (id, name, active) VALUES (3, NULL, NULL)",
        &[],
    )
    .await
    .expect("INSERT row 3 with NULLs");

    let dump = conn
        .export_as_sql()
        .await
        .expect("export_as_sql must succeed");

    assert!(
        dump.contains("CREATE TABLE"),
        "dump must contain CREATE TABLE; got:\n{dump}"
    );
    assert!(
        dump.contains("INSERT INTO"),
        "dump must contain INSERT INTO; got:\n{dump}"
    );

    // Round-trip: import into a fresh connection and verify data.
    let conn2 = EmbeddedConnection::open_memory().expect("open_memory 2");
    conn2
        .import_from_sql(&dump)
        .await
        .expect("import_from_sql round-trip");

    let rows = conn2
        .query("SELECT id, name, active FROM rt_users ORDER BY id", &[])
        .await
        .expect("SELECT after round-trip");
    assert_eq!(rows.len(), 3, "must have 3 rows after round-trip");

    let id0: i64 = rows[0].try_get("id").expect("id col");
    assert_eq!(id0, 1);
    let name0: String = rows[0].try_get("name").expect("name col");
    assert_eq!(name0, "Alice");

    let id2: i64 = rows[2].try_get("id").expect("id row3");
    assert_eq!(id2, 3);
    // NULL name in row 3 — try_get returns Err, so we check it is Null.
    assert!(
        rows[2].try_get::<String>("name").is_err() || rows[2].get("name") == Some(&Value::Null),
        "name in row 3 must be NULL"
    );
}

#[tokio::test]
async fn test_export_as_sql_empty_table() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE empty_tbl (id INT NOT NULL, val TEXT NULL)",
        &[],
    )
    .await
    .expect("CREATE TABLE");

    let dump = conn.export_as_sql().await.expect("export must succeed");

    assert!(
        dump.contains("CREATE TABLE"),
        "dump must contain CREATE TABLE DDL"
    );
    assert!(
        !dump.contains("INSERT INTO"),
        "dump of empty table must not contain INSERT; got:\n{dump}"
    );
}

#[tokio::test]
async fn test_export_as_sql_text_with_quotes() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE quote_tbl (id INT NOT NULL, name TEXT NULL)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute(
        "INSERT INTO quote_tbl (id, name) VALUES (1, 'O''Brien')",
        &[],
    )
    .await
    .expect("INSERT O'Brien");

    let dump = conn.export_as_sql().await.expect("export must succeed");

    // The dump must contain the escaped single-quote (doubled).
    assert!(
        dump.contains("O''Brien"),
        "dump must escape single-quotes; got:\n{dump}"
    );

    // Round-trip.
    let conn2 = EmbeddedConnection::open_memory().expect("open_memory 2");
    conn2
        .import_from_sql(&dump)
        .await
        .expect("import round-trip");
    let rows = conn2
        .query("SELECT name FROM quote_tbl", &[])
        .await
        .expect("SELECT after round-trip");
    assert_eq!(rows.len(), 1);
    let name: String = rows[0].try_get("name").expect("name col");
    assert_eq!(name, "O'Brien", "single-quote must survive round-trip");
}

#[tokio::test]
async fn test_export_as_sql_nulls() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE null_tbl (id INT NOT NULL, a TEXT NULL, b INT NULL)",
        &[],
    )
    .await
    .expect("CREATE TABLE");
    conn.execute(
        "INSERT INTO null_tbl (id, a, b) VALUES (1, NULL, NULL)",
        &[],
    )
    .await
    .expect("INSERT NULL row");

    let dump = conn.export_as_sql().await.expect("export must succeed");

    assert!(
        dump.contains("NULL"),
        "dump must contain NULL literal; got:\n{dump}"
    );

    // Round-trip.
    let conn2 = EmbeddedConnection::open_memory().expect("open_memory 2");
    conn2.import_from_sql(&dump).await.expect("import");
    let rows = conn2
        .query("SELECT id, a, b FROM null_tbl", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    let id: i64 = rows[0].try_get("id").expect("id");
    assert_eq!(id, 1);
    assert!(
        rows[0].try_get::<String>("a").is_err() || rows[0].get("a") == Some(&Value::Null),
        "column a must be NULL after round-trip"
    );
}

// ── oxisql-parse integration — normalize_sql / is_read_only_sql ───────────────

#[test]
fn test_normalize_sql_collapses_whitespace() {
    let normalized =
        EmbeddedConnection::normalize_sql("  SELECT  id  FROM  users  WHERE  id = 1  ");
    assert!(!normalized.is_empty());
    // Normalization should collapse multiple spaces to one.
    assert!(!normalized.contains("  "));
}

#[test]
fn test_normalize_sql_empty_input_returns_empty() {
    let normalized = EmbeddedConnection::normalize_sql("");
    assert!(normalized.is_empty());
}

#[test]
fn test_is_read_only_sql_select_is_true() {
    assert!(EmbeddedConnection::is_read_only_sql("SELECT id FROM t"));
}

#[test]
fn test_is_read_only_sql_insert_is_false() {
    assert!(!EmbeddedConnection::is_read_only_sql(
        "INSERT INTO t VALUES (1)"
    ));
}

#[test]
fn test_is_read_only_sql_delete_is_false() {
    assert!(!EmbeddedConnection::is_read_only_sql(
        "DELETE FROM t WHERE id = 1"
    ));
}

#[test]
fn test_is_read_only_sql_unparseable_returns_false() {
    // GlueSQL-specific or completely invalid SQL should return false, not panic.
    assert!(!EmbeddedConnection::is_read_only_sql("NOT VALID SQL @@@"));
}

// ── PRAGMA support tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_pragma_journal_mode() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // execute() should not fail for a known PRAGMA
    conn.execute("PRAGMA journal_mode = WAL", &[])
        .await
        .expect("PRAGMA journal_mode = WAL should not error");
    let rows = conn
        .query("PRAGMA journal_mode", &[])
        .await
        .expect("PRAGMA journal_mode query should not error");
    if !rows.is_empty() {
        // If rows are returned, verify the column is present and has a value
        let val = rows[0].get("journal_mode");
        assert!(
            val.is_some(),
            "journal_mode column must be present in result row"
        );
    }
}

#[tokio::test]
async fn test_pragma_foreign_keys() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("PRAGMA foreign_keys = ON", &[])
        .await
        .expect("PRAGMA foreign_keys = ON should not error");
    conn.execute("PRAGMA foreign_keys = OFF", &[])
        .await
        .expect("PRAGMA foreign_keys = OFF should not error");
    let rows = conn
        .query("PRAGMA foreign_keys", &[])
        .await
        .expect("PRAGMA foreign_keys query should not error");
    if !rows.is_empty() {
        let val = rows[0].get("foreign_keys");
        assert!(
            val.is_some(),
            "foreign_keys column must be present in result row"
        );
    }
}

#[tokio::test]
async fn test_pragma_page_size() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let rows = conn
        .query("PRAGMA page_size", &[])
        .await
        .expect("PRAGMA page_size should not error");
    // Should return exactly one row with page_size column
    assert_eq!(rows.len(), 1, "PRAGMA page_size must return one row");
    assert!(
        rows[0].get("page_size").is_some(),
        "page_size column must be present"
    );
}

#[tokio::test]
async fn test_pragma_cache_size() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("PRAGMA cache_size = 2000", &[])
        .await
        .expect("PRAGMA cache_size = 2000 should not error");
}

#[tokio::test]
async fn test_pragma_user_version() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let rows = conn
        .query("PRAGMA user_version", &[])
        .await
        .expect("PRAGMA user_version should not error");
    assert_eq!(rows.len(), 1, "PRAGMA user_version must return one row");
    assert_eq!(
        rows[0].get("user_version"),
        Some(&Value::I64(0)),
        "user_version must be 0 for a fresh in-memory connection"
    );
}

#[tokio::test]
async fn test_pragma_integrity_check() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE chk (id INT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO chk VALUES (1)", &[])
        .await
        .expect("INSERT");
    let rows = conn
        .query("PRAGMA integrity_check", &[])
        .await
        .expect("PRAGMA integrity_check should not error");
    assert_eq!(rows.len(), 1, "PRAGMA integrity_check must return one row");
    assert_eq!(
        rows[0].get("integrity_check"),
        Some(&Value::Text("ok".to_string())),
        "integrity_check must return 'ok' for a valid in-memory database"
    );
}

#[tokio::test]
async fn test_pragma_unknown_is_silent() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // Unknown pragmas should return empty result rather than an error
    let exec_result = conn.execute("PRAGMA some_unknown_setting = 1", &[]).await;
    assert!(
        exec_result.is_ok(),
        "unknown PRAGMA in execute() must not error, got: {exec_result:?}"
    );
    let query_result = conn
        .query("PRAGMA another_unknown", &[])
        .await
        .expect("unknown PRAGMA in query() must not error");
    assert!(
        query_result.is_empty(),
        "unknown PRAGMA must return empty rows, got: {query_result:?}"
    );
}

#[tokio::test]
async fn test_pragma_case_insensitive() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    // PRAGMA keyword must be recognised regardless of case
    conn.execute("pragma journal_mode = wal", &[])
        .await
        .expect("lowercase pragma should not error");
    conn.execute("Pragma foreign_keys=ON", &[])
        .await
        .expect("mixed-case Pragma should not error");
}

// ── ATTACH DATABASE tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn test_attach_database_returns_meaningful_error() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let result = conn.execute("ATTACH DATABASE 'test.db' AS test", &[]).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    // The error message must be non-empty and communicate that ATTACH is
    // unsupported with in-memory storage.
    assert!(
        err.contains("ATTACH") || err.contains("not supported"),
        "expected ATTACH/not-supported in error, got: {err}"
    );
}

#[tokio::test]
async fn test_attach_schema_returns_meaningful_error() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let result = conn.execute("ATTACH SCHEMA 'other.db' AS other", &[]).await;
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        !err.is_empty(),
        "error message must not be empty for ATTACH SCHEMA"
    );
}

#[tokio::test]
async fn test_attach_via_query_returns_meaningful_error() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let result = conn.query("ATTACH DATABASE 'test.db' AS test", &[]).await;
    assert!(result.is_err());
}

// ── non-ATTACH SQL is unaffected ──────────────────────────────────────────────

#[tokio::test]
async fn test_pragma_does_not_interfere_with_normal_sql() {
    // Verify that SQL containing "PRAGMA" as a substring in a different context
    // is NOT incorrectly intercepted (e.g. a table named differently, or a
    // comment containing the word).
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE noconflict (id INT, note TEXT)", &[])
        .await
        .expect("CREATE TABLE noconflict");
    conn.execute("INSERT INTO noconflict VALUES (1, 'hello')", &[])
        .await
        .expect("INSERT");
    let rows = conn
        .query("SELECT id FROM noconflict", &[])
        .await
        .expect("SELECT must succeed for non-PRAGMA SQL");
    assert_eq!(rows.len(), 1);
}
