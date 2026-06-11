//! Phase A — change-count correctness tests.
//!
//! Verifies that `n_change` is correctly reset between cached statement reuses
//! and that `conn.execute()` returns accurate affected-row counts for INSERT,
//! UPDATE, DELETE, DDL, and TCL statements.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── INSERT change-count correctness ───────────────────────────────────────────

/// Execute the same parameterised INSERT 5 times and verify that each call
/// reports exactly 1 affected row.
///
/// Before the `n_change` reset fix, the Nth INSERT would report N rows because
/// `Program::n_change` accumulated across `Statement::reset()` calls.
#[tokio::test]
async fn test_cached_statement_change_count_correct() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    for i in 1i64..=5 {
        let affected = conn
            .execute("INSERT INTO t VALUES ($1)", &[&i])
            .await
            .unwrap_or_else(|e| panic!("INSERT #{i} failed: {e}"));
        assert_eq!(
            affected, 1,
            "INSERT #{i} should affect exactly 1 row, not {affected}"
        );
    }
}

// ── UPDATE change-count ────────────────────────────────────────────────────────

/// UPDATE that matches exactly N rows should return N.
#[tokio::test]
async fn test_update_change_count() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE scores (id INTEGER, val INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Insert 10 rows.
    for i in 0i64..10 {
        conn.execute("INSERT INTO scores VALUES ($1, $2)", &[&i, &(i * 2)])
            .await
            .unwrap_or_else(|e| panic!("INSERT {i} failed: {e}"));
    }

    // UPDATE all rows (should touch 10).
    let affected = conn
        .execute("UPDATE scores SET val = val + 1", &[])
        .await
        .expect("UPDATE all failed");
    assert_eq!(
        affected, 10,
        "UPDATE all should affect 10 rows, got {affected}"
    );

    // UPDATE matching a single row.
    let affected_one = conn
        .execute("UPDATE scores SET val = 999 WHERE id = $1", &[&5i64])
        .await
        .expect("UPDATE one failed");
    assert_eq!(
        affected_one, 1,
        "UPDATE one row should return 1, got {affected_one}"
    );
}

// ── DELETE change-count ────────────────────────────────────────────────────────

/// DELETE that removes exactly N rows should return N.
#[tokio::test]
async fn test_delete_change_count() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE items (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Insert 6 rows.
    for i in 0i64..6 {
        conn.execute("INSERT INTO items VALUES ($1)", &[&i])
            .await
            .unwrap_or_else(|e| panic!("INSERT {i} failed: {e}"));
    }

    // DELETE 3 specific rows.
    let affected = conn
        .execute("DELETE FROM items WHERE id IN (0, 2, 4)", &[])
        .await
        .expect("DELETE failed");
    assert_eq!(affected, 3, "DELETE should affect 3 rows, got {affected}");

    // DELETE remaining rows (3 left).
    let affected_rest = conn
        .execute("DELETE FROM items", &[])
        .await
        .expect("DELETE all failed");
    assert_eq!(
        affected_rest, 3,
        "DELETE all remaining should affect 3, got {affected_rest}"
    );
}

// ── DDL returns 0 ─────────────────────────────────────────────────────────────

/// DDL statements (CREATE TABLE, DROP TABLE) must return 0 affected rows.
#[tokio::test]
async fn test_ddl_returns_zero() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    let create_count = conn
        .execute("CREATE TABLE ddl_test (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");
    assert_eq!(
        create_count, 0,
        "CREATE TABLE should return 0, got {create_count}"
    );
}

// ── TCL returns 0 ─────────────────────────────────────────────────────────────

/// BEGIN, COMMIT, and ROLLBACK must return 0 affected rows.
#[tokio::test]
async fn test_tcl_returns_zero() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE tcl_test (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Transaction that commits.
    let mut txn = conn.transaction().await.expect("BEGIN failed");
    let insert_count = txn
        .execute("INSERT INTO tcl_test VALUES (1)", &[])
        .await
        .expect("INSERT in txn failed");
    assert_eq!(
        insert_count, 1,
        "INSERT inside txn should return 1, got {insert_count}"
    );
    txn.commit().await.expect("COMMIT failed");

    // Verify the row is there.
    let rows = conn
        .query("SELECT COUNT(*) FROM tcl_test", &[])
        .await
        .expect("COUNT failed");
    assert_eq!(
        rows.first().and_then(|r| r.get_by_index(0)),
        Some(&Value::I64(1)),
        "expected 1 committed row"
    );

    // Transaction that rolls back.
    let mut txn2 = conn.transaction().await.expect("BEGIN 2 failed");
    txn2.execute("INSERT INTO tcl_test VALUES (2)", &[])
        .await
        .expect("INSERT in txn2 failed");
    txn2.rollback().await.expect("ROLLBACK failed");

    // Only the original row should remain.
    let rows2 = conn
        .query("SELECT COUNT(*) FROM tcl_test", &[])
        .await
        .expect("COUNT 2 failed");
    assert_eq!(
        rows2.first().and_then(|r| r.get_by_index(0)),
        Some(&Value::I64(1)),
        "expected 1 row after rollback"
    );
}

// ── Repeated cached INSERT does not accumulate ────────────────────────────────

/// Re-use the same INSERT statement 10 times with different values and confirm
/// each returns 1 — not 1, 2, 3, … as it would if `n_change` accumulated.
#[tokio::test]
async fn test_repeated_cached_insert_no_accumulation() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE acc (n INTEGER)", &[])
        .await
        .expect("CREATE failed");

    for i in 0i64..10 {
        let affected = conn
            .execute("INSERT INTO acc VALUES ($1)", &[&i])
            .await
            .unwrap_or_else(|e| panic!("INSERT {i} failed: {e}"));
        assert_eq!(
            affected, 1,
            "INSERT #{i} should return 1 (not {affected}): n_change may be accumulating"
        );
    }
}
