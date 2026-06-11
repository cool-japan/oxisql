//! ROLLBACK integration tests for `oxisql-sqlite-compat`.
//!
//! Verifies that `SqliteTransaction::rollback()` correctly discards uncommitted
//! changes, that committed data is not affected, and that the connection remains
//! usable after a rollback.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── test_basic_rollback ───────────────────────────────────────────────────────

/// `BEGIN; INSERT INTO t VALUES(1); ROLLBACK;` → SELECT COUNT(*) must be 0.
#[tokio::test]
async fn test_basic_rollback() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    let mut txn = conn.transaction().await.expect("BEGIN failed");
    txn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT failed");
    txn.rollback().await.expect("ROLLBACK failed");

    let rows = conn
        .query("SELECT COUNT(*) FROM t", &[])
        .await
        .expect("SELECT failed");
    let count = match rows.first().and_then(|r| r.get_by_index(0)) {
        Some(Value::I64(n)) => *n,
        other => panic!("unexpected COUNT(*) result: {other:?}"),
    };
    assert_eq!(
        count, 0,
        "rolled-back INSERT must leave 0 rows, got {count}"
    );
}

// ── test_commit_not_rolled_back ───────────────────────────────────────────────

/// `BEGIN; INSERT INTO t VALUES(1); COMMIT;` then verify the row persists.
#[tokio::test]
async fn test_commit_not_rolled_back() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    let mut txn = conn.transaction().await.expect("BEGIN failed");
    txn.execute("INSERT INTO t VALUES (42)", &[])
        .await
        .expect("INSERT failed");
    txn.commit().await.expect("COMMIT failed");

    let rows = conn
        .query("SELECT id FROM t", &[])
        .await
        .expect("SELECT failed");
    assert_eq!(rows.len(), 1, "committed row must persist after COMMIT");
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::I64(42)),
        "committed value must be 42"
    );
}

// ── test_rollback_after_no_txn ────────────────────────────────────────────────

/// Bare `ROLLBACK` with no active transaction.
///
/// SQLite returns "cannot rollback — no transaction is active" in this case.
/// The compat layer should propagate an error (not panic).
#[tokio::test]
async fn test_rollback_after_no_txn() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    // Execute ROLLBACK directly without a preceding BEGIN.
    let result = conn.execute("ROLLBACK", &[]).await;

    // SQLite signals an error when there is no active transaction to roll back.
    // The exact message is implementation-defined; we just verify it is an Err.
    assert!(
        result.is_err(),
        "ROLLBACK outside a transaction must return Err, but got Ok"
    );
}

// ── test_multi_row_rollback ───────────────────────────────────────────────────

/// Insert 10 rows inside a transaction, rollback, verify 0 rows remain.
#[tokio::test]
async fn test_multi_row_rollback() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE nums (n INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    let mut txn = conn.transaction().await.expect("BEGIN failed");
    for i in 0i64..10 {
        txn.execute("INSERT INTO nums VALUES ($1)", &[&i])
            .await
            .expect("INSERT failed");
    }
    txn.rollback().await.expect("ROLLBACK failed");

    let rows = conn
        .query("SELECT COUNT(*) FROM nums", &[])
        .await
        .expect("SELECT failed");
    let count = match rows.first().and_then(|r| r.get_by_index(0)) {
        Some(Value::I64(n)) => *n,
        other => panic!("unexpected COUNT(*) result: {other:?}"),
    };
    assert_eq!(
        count, 0,
        "all 10 rows must be gone after rollback, got {count}"
    );
}

// ── test_read_after_rollback ──────────────────────────────────────────────────

/// Tests engine-state integrity: the connection remains fully usable for reads
/// after a rollback.  Verifies that:
///   - pre-existing committed data is still readable after a rollback,
///   - the engine can open new transactions without errors, and
///   - the rolled-back data is invisible to subsequent reads.
///
/// Uses an in-memory database to isolate transaction-state correctness from
/// file-I/O concerns.
#[tokio::test]
async fn test_read_after_rollback() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE items (val INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Commit several sentinel rows outside any explicit transaction.
    for v in [10i64, 20, 30] {
        conn.execute("INSERT INTO items VALUES ($1)", &[&v])
            .await
            .expect("sentinel INSERT failed");
    }

    // Begin a transaction, insert an extra row, then roll it back.
    let mut txn1 = conn.transaction().await.expect("BEGIN txn1 failed");
    txn1.execute("INSERT INTO items VALUES (999)", &[])
        .await
        .expect("INSERT in txn1 failed");
    txn1.rollback().await.expect("ROLLBACK txn1 failed");

    // The engine must accept a new transaction after rollback (no panic, no
    // "transaction already active" error).
    let mut txn2 = conn
        .transaction()
        .await
        .expect("BEGIN txn2 after rollback failed");

    // All three sentinel rows must still be visible; the rolled-back row must not.
    let rows = txn2
        .query("SELECT val FROM items ORDER BY val", &[])
        .await
        .expect("SELECT within txn2 failed");
    txn2.commit().await.expect("COMMIT txn2 failed");

    assert_eq!(
        rows.len(),
        3,
        "exactly 3 sentinel rows must remain after rollback, got {}",
        rows.len()
    );
    let vals: Vec<i64> = rows
        .iter()
        .filter_map(|r| {
            if let Some(Value::I64(n)) = r.get_by_index(0) {
                Some(*n)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        vals,
        vec![10, 20, 30],
        "sentinel values must be intact: {vals:?}"
    );
    assert!(
        !vals.contains(&999),
        "rolled-back value 999 must not appear: {vals:?}"
    );
}
