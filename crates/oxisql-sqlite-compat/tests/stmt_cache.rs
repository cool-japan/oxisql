//! Phase B — statement-cache correctness tests.
//!
//! These tests exercise the LRU statement cache added to [`SqliteConnection`]
//! in Phase B of OxiSQL 0.1.0.  All tests use in-memory databases so no
//! external resources are required.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── test_stmt_cache_correctness_repeated_binds ────────────────────────────────

/// Execute the same parameterised INSERT 20 times with distinct values, then
/// verify that all 20 rows were inserted correctly.
///
/// On the first call the statement is compiled and inserted into the cache.
/// Calls 2-20 hit the cache, clone the `limbo::Statement`, and re-execute via
/// `Statement::execute()` which calls `reset()` before binding — so each run
/// receives fresh parameters.
#[tokio::test]
async fn test_stmt_cache_correctness_repeated_binds() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute(
        "CREATE TABLE cache_test (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
        &[],
    )
    .await
    .expect("CREATE TABLE failed");

    let insert_sql = "INSERT INTO cache_test VALUES ($1, $2)";

    for i in 0i64..20 {
        let tag = format!("value_{i}");
        let affected = conn
            .execute(insert_sql, &[&i, &tag.as_str()])
            .await
            .unwrap_or_else(|e| panic!("INSERT {i} failed: {e}"));
        assert_eq!(affected, 1, "expected 1 affected row for i={i}");
    }

    // Verify all 20 distinct rows exist.
    let rows = conn
        .query("SELECT id, val FROM cache_test ORDER BY id", &[])
        .await
        .expect("SELECT failed");

    assert_eq!(rows.len(), 20, "expected 20 rows, got {}", rows.len());

    for (idx, row) in rows.iter().enumerate() {
        let id = row.get_by_index(0);
        let val = row.get_by_index(1);
        assert_eq!(
            id,
            Some(&Value::I64(idx as i64)),
            "row {idx}: unexpected id {id:?}"
        );
        let expected_val = format!("value_{idx}");
        assert_eq!(
            val,
            Some(&Value::Text(expected_val.clone())),
            "row {idx}: unexpected val {val:?}"
        );
    }
}

// ── test_stmt_cache_eviction ──────────────────────────────────────────────────

/// Exceed the cache capacity by executing many distinct SQL strings so that the
/// LRU evicts old entries.  Verify that no panic or incorrect behaviour occurs.
///
/// After the table is created we insert CAPACITY + 10 rows where each INSERT
/// uses a different literal in the WHERE clause (forcing distinct prepared
/// statements).  This is a correctness smoke-test, not a performance test.
#[tokio::test]
async fn test_stmt_cache_eviction() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE evict_test (n INTEGER NOT NULL)", &[])
        .await
        .expect("CREATE TABLE failed");

    // 138 distinct SQL strings (STMT_CACHE_CAPACITY=128, so we overflow by 10).
    let num_stmts: usize = 138;
    for i in 0..num_stmts {
        // Embed the literal directly in the SQL so each string is unique and
        // forces a distinct cache entry.
        let sql = format!("INSERT INTO evict_test VALUES ({i})");
        conn.execute(&sql, &[])
            .await
            .unwrap_or_else(|e| panic!("INSERT {i} failed: {e}"));
    }

    let rows = conn
        .query("SELECT COUNT(*) FROM evict_test", &[])
        .await
        .expect("COUNT query failed");

    let count = rows
        .first()
        .and_then(|r| r.get_by_index(0))
        .and_then(|v| {
            if let Value::I64(n) = v {
                Some(*n)
            } else {
                None
            }
        })
        .expect("expected i64 count");

    assert_eq!(
        count, num_stmts as i64,
        "expected {num_stmts} rows after eviction, got {count}"
    );
}

// ── test_stmt_cache_interleaved ───────────────────────────────────────────────

/// Alternate between two different parameterised queries and verify that results
/// are correct for both — i.e. no cross-contamination between cache entries.
#[tokio::test]
async fn test_stmt_cache_interleaved() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE alpha (id INTEGER, name TEXT)", &[])
        .await
        .expect("CREATE alpha failed");

    conn.execute("CREATE TABLE beta (id INTEGER, score INTEGER)", &[])
        .await
        .expect("CREATE beta failed");

    let insert_alpha = "INSERT INTO alpha VALUES ($1, $2)";
    let insert_beta = "INSERT INTO beta VALUES ($1, $2)";

    for i in 0i64..10 {
        let name = format!("name_{i}");
        conn.execute(insert_alpha, &[&i, &name.as_str()])
            .await
            .unwrap_or_else(|e| panic!("alpha insert {i} failed: {e}"));

        let score = i * 7;
        conn.execute(insert_beta, &[&i, &score])
            .await
            .unwrap_or_else(|e| panic!("beta insert {i} failed: {e}"));
    }

    // Verify alpha rows.
    let alpha_rows = conn
        .query("SELECT id, name FROM alpha ORDER BY id", &[])
        .await
        .expect("alpha SELECT failed");
    assert_eq!(alpha_rows.len(), 10, "expected 10 alpha rows");
    for (idx, row) in alpha_rows.iter().enumerate() {
        assert_eq!(
            row.get_by_index(0),
            Some(&Value::I64(idx as i64)),
            "alpha row {idx}: wrong id"
        );
        let expected = format!("name_{idx}");
        assert_eq!(
            row.get_by_index(1),
            Some(&Value::Text(expected.clone())),
            "alpha row {idx}: wrong name"
        );
    }

    // Verify beta rows.
    let beta_rows = conn
        .query("SELECT id, score FROM beta ORDER BY id", &[])
        .await
        .expect("beta SELECT failed");
    assert_eq!(beta_rows.len(), 10, "expected 10 beta rows");
    for (idx, row) in beta_rows.iter().enumerate() {
        assert_eq!(
            row.get_by_index(0),
            Some(&Value::I64(idx as i64)),
            "beta row {idx}: wrong id"
        );
        let expected_score = (idx as i64) * 7;
        assert_eq!(
            row.get_by_index(1),
            Some(&Value::I64(expected_score)),
            "beta row {idx}: wrong score"
        );
    }
}
