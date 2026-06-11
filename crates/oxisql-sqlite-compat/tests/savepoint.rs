//! SAVEPOINT / RELEASE / ROLLBACK TO SAVEPOINT integration tests.
//!
//! These tests verify correct WAL-frame-level isolation: only changes made
//! after a savepoint are undone by ROLLBACK TO; changes before the savepoint
//! survive.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;
use std::env;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Open a fresh in-memory connection with a simple integer table.
async fn open_with_table() -> SqliteConnection {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");
    conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY)", &[])
        .await
        .expect("CREATE TABLE failed");
    conn
}

/// Count rows in table `t`.
async fn count_rows(conn: &SqliteConnection) -> i64 {
    let rows = conn
        .query("SELECT COUNT(*) FROM t", &[])
        .await
        .expect("COUNT(*) failed");
    match rows.first().and_then(|r| r.get_by_index(0)) {
        Some(Value::I64(n)) => *n,
        other => panic!("unexpected COUNT(*): {other:?}"),
    }
}

/// Collect all ids from table `t` in ascending order.
async fn all_ids(conn: &SqliteConnection) -> Vec<i64> {
    let rows = conn
        .query("SELECT id FROM t ORDER BY id", &[])
        .await
        .expect("SELECT failed");
    rows.iter()
        .filter_map(|r| {
            if let Some(Value::I64(n)) = r.get_by_index(0) {
                Some(*n)
            } else {
                None
            }
        })
        .collect()
}

// ── Test 1: basic SAVEPOINT + ROLLBACK TO ────────────────────────────────────

/// BEGIN; INSERT(1); SAVEPOINT s1; INSERT(2); ROLLBACK TO s1; COMMIT
/// → only row 1 survives.
#[tokio::test]
async fn test_savepoint_rollback_to() {
    let conn = open_with_table().await;

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    conn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT 1 failed");
    conn.execute("SAVEPOINT s1", &[])
        .await
        .expect("SAVEPOINT s1 failed");
    conn.execute("INSERT INTO t VALUES (2)", &[])
        .await
        .expect("INSERT 2 failed");
    // Verify that row 2 is visible before rollback.
    assert_eq!(
        count_rows(&conn).await,
        2,
        "expected 2 rows before rollback"
    );
    conn.execute("ROLLBACK TO s1", &[])
        .await
        .expect("ROLLBACK TO s1 failed");
    // Row 2 must be gone; row 1 must survive.
    assert_eq!(
        count_rows(&conn).await,
        1,
        "expected 1 row after ROLLBACK TO"
    );
    conn.execute("COMMIT", &[]).await.expect("COMMIT failed");

    assert_eq!(all_ids(&conn).await, vec![1], "only row 1 should persist");
}

// ── Test 2: SAVEPOINT + RELEASE (no rollback) ────────────────────────────────

/// BEGIN; INSERT(1); SAVEPOINT s1; INSERT(2); RELEASE s1; COMMIT
/// → both rows survive.
#[tokio::test]
async fn test_savepoint_release_commits() {
    let conn = open_with_table().await;

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    conn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT 1 failed");
    conn.execute("SAVEPOINT s1", &[])
        .await
        .expect("SAVEPOINT s1 failed");
    conn.execute("INSERT INTO t VALUES (2)", &[])
        .await
        .expect("INSERT 2 failed");
    conn.execute("RELEASE s1", &[])
        .await
        .expect("RELEASE s1 failed");
    conn.execute("COMMIT", &[]).await.expect("COMMIT failed");

    assert_eq!(
        all_ids(&conn).await,
        vec![1, 2],
        "both rows must survive after RELEASE + COMMIT"
    );
}

// ── Test 3: nested SAVEPOINTs ─────────────────────────────────────────────────

/// BEGIN; INSERT(1); SAVEPOINT outer; INSERT(2); SAVEPOINT inner; INSERT(3);
/// ROLLBACK TO inner; INSERT(4); RELEASE inner; RELEASE outer; COMMIT
/// → rows 1, 2, 4 survive (row 3 was rolled back).
#[tokio::test]
async fn test_nested_savepoints() {
    let conn = open_with_table().await;

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    conn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT 1 failed");

    conn.execute("SAVEPOINT outer", &[])
        .await
        .expect("SAVEPOINT outer failed");
    conn.execute("INSERT INTO t VALUES (2)", &[])
        .await
        .expect("INSERT 2 failed");

    conn.execute("SAVEPOINT inner", &[])
        .await
        .expect("SAVEPOINT inner failed");
    conn.execute("INSERT INTO t VALUES (3)", &[])
        .await
        .expect("INSERT 3 failed");
    assert_eq!(count_rows(&conn).await, 3, "expected 3 rows");

    // Roll back row 3.
    conn.execute("ROLLBACK TO inner", &[])
        .await
        .expect("ROLLBACK TO inner failed");
    assert_eq!(
        count_rows(&conn).await,
        2,
        "expected 2 rows after inner rollback"
    );

    // Insert row 4 (inside inner savepoint scope again).
    conn.execute("INSERT INTO t VALUES (4)", &[])
        .await
        .expect("INSERT 4 failed");

    conn.execute("RELEASE inner", &[])
        .await
        .expect("RELEASE inner failed");
    conn.execute("RELEASE outer", &[])
        .await
        .expect("RELEASE outer failed");
    conn.execute("COMMIT", &[]).await.expect("COMMIT failed");

    let ids = all_ids(&conn).await;
    assert_eq!(ids, vec![1, 2, 4], "rows 1, 2, 4 must survive: {ids:?}");
}

// ── Test 4: SAVEPOINT in autocommit (implicit transaction) ───────────────────

/// No explicit BEGIN; SAVEPOINT s1; INSERT; RELEASE s1
/// → INSERT is committed (RELEASE of owner-savepoint commits).
#[tokio::test]
async fn test_savepoint_autocommit_release() {
    let conn = open_with_table().await;

    // In autocommit mode SAVEPOINT starts an implicit transaction.
    conn.execute("SAVEPOINT s1", &[])
        .await
        .expect("SAVEPOINT s1 failed");
    conn.execute("INSERT INTO t VALUES (42)", &[])
        .await
        .expect("INSERT failed");
    conn.execute("RELEASE s1", &[])
        .await
        .expect("RELEASE s1 failed");

    // The implicit transaction should have committed.
    assert_eq!(
        all_ids(&conn).await,
        vec![42],
        "row must be committed after autocommit RELEASE"
    );
}

// ── Test 5: ROLLBACK TO + RELEASE (no net change) ────────────────────────────

/// SAVEPOINT s1; INSERT; ROLLBACK TO s1; RELEASE s1
/// → nothing committed.
#[tokio::test]
async fn test_savepoint_rollback_then_release() {
    let conn = open_with_table().await;

    conn.execute("SAVEPOINT s1", &[])
        .await
        .expect("SAVEPOINT s1 failed");
    conn.execute("INSERT INTO t VALUES (99)", &[])
        .await
        .expect("INSERT failed");
    conn.execute("ROLLBACK TO s1", &[])
        .await
        .expect("ROLLBACK TO s1 failed");
    conn.execute("RELEASE s1", &[])
        .await
        .expect("RELEASE s1 failed");

    // After rollback and release, nothing should be committed.
    assert_eq!(
        count_rows(&conn).await,
        0,
        "no rows must be committed after ROLLBACK TO + RELEASE"
    );
}

// ── Test 6: savepoint name is case-insensitive ────────────────────────────────

/// SAVEPOINT MyPoint; INSERT; ROLLBACK TO mypoint → insert gone.
#[tokio::test]
async fn test_savepoint_name_case_insensitive() {
    let conn = open_with_table().await;

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    conn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT 1 failed");
    conn.execute("SAVEPOINT MyPoint", &[])
        .await
        .expect("SAVEPOINT MyPoint failed");
    conn.execute("INSERT INTO t VALUES (2)", &[])
        .await
        .expect("INSERT 2 failed");
    // Use lowercase name to roll back.
    conn.execute("ROLLBACK TO mypoint", &[])
        .await
        .expect("ROLLBACK TO mypoint failed");
    conn.execute("COMMIT", &[]).await.expect("COMMIT failed");

    assert_eq!(all_ids(&conn).await, vec![1], "row 2 must be rolled back");
}

// ── Test 7: full ROLLBACK beats savepoints ───────────────────────────────────

/// BEGIN; SAVEPOINT s1; INSERT; ROLLBACK → everything gone.
#[tokio::test]
async fn test_full_rollback_clears_savepoints() {
    let conn = open_with_table().await;

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    conn.execute("SAVEPOINT s1", &[])
        .await
        .expect("SAVEPOINT s1 failed");
    conn.execute("INSERT INTO t VALUES (7)", &[])
        .await
        .expect("INSERT failed");
    conn.execute("ROLLBACK", &[])
        .await
        .expect("ROLLBACK failed");

    assert_eq!(
        count_rows(&conn).await,
        0,
        "full ROLLBACK must discard all changes"
    );
}

// ── Test 8: multiple rows, correct count after nested rollback ────────────────

/// Verify that exactly the right number of rows survive a nested savepoint
/// rollback (file-backed DB to exercise WAL pager path).
#[tokio::test]
async fn test_nested_savepoint_correct_row_count_file() {
    let tmp_dir = env::temp_dir();
    let db_path = tmp_dir.join("oxisql_sp_test.db");
    // Clean up from previous runs (ignore error if file doesn't exist).
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(tmp_dir.join("oxisql_sp_test.db-wal"));

    let conn = SqliteConnection::open(db_path.to_str().expect("valid path"))
        .await
        .expect("open file db failed");
    conn.execute("CREATE TABLE nums (n INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    conn.execute("BEGIN", &[]).await.expect("BEGIN failed");
    // Insert rows 1–5.
    for i in 1i64..=5 {
        conn.execute("INSERT INTO nums VALUES ($1)", &[&i])
            .await
            .expect("INSERT failed");
    }

    conn.execute("SAVEPOINT sp", &[])
        .await
        .expect("SAVEPOINT sp failed");
    // Insert rows 6–10 (to be rolled back).
    for i in 6i64..=10 {
        conn.execute("INSERT INTO nums VALUES ($1)", &[&i])
            .await
            .expect("INSERT in savepoint failed");
    }

    conn.execute("ROLLBACK TO sp", &[])
        .await
        .expect("ROLLBACK TO sp failed");

    // Only rows 1–5 should be visible.
    let rows = conn
        .query("SELECT COUNT(*) FROM nums", &[])
        .await
        .expect("COUNT(*) failed");
    let count = match rows.first().and_then(|r| r.get_by_index(0)) {
        Some(Value::I64(n)) => *n,
        other => panic!("unexpected COUNT(*): {other:?}"),
    };
    assert_eq!(
        count, 5,
        "only 5 rows should survive after savepoint rollback"
    );

    conn.execute("COMMIT", &[]).await.expect("COMMIT failed");

    // Clean up.
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(tmp_dir.join("oxisql_sp_test.db-wal"));
}
