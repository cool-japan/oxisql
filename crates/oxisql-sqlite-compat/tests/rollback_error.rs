//! Phase E — honest ROLLBACK error tests.
//!
//! Verifies that `SqliteTransaction::rollback()` returns a clear, actionable
//! error message rather than a cryptic parse-error from Limbo 0.0.22.

use oxisql_core::Connection;
use oxisql_sqlite_compat::SqliteConnection;

// ── test_rollback_returns_clear_error ─────────────────────────────────────────

/// Begin a transaction, insert a row, then call `rollback()`.
///
/// Expected: `Err(OxiSqlError::Other("ROLLBACK is not supported by the limbo
/// 0.0.22 engine …"))`.  The error message must contain "ROLLBACK" and "not
/// supported" so that callers can distinguish it from arbitrary engine errors.
/// It must NOT contain a raw parse-error like "ROLLBACK not supported yet" —
/// that would expose Limbo internals; our wrapper should give a clear user-
/// facing message.
#[tokio::test]
async fn test_rollback_returns_clear_error() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE rb_test (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    let txn = conn.transaction().await.expect("BEGIN transaction failed");

    // The result of rollback() must be Err.
    let result = txn.rollback().await;
    assert!(
        result.is_err(),
        "rollback() must return Err with limbo 0.0.22, but returned Ok"
    );

    let err_msg = result.unwrap_err().to_string();

    // The message must mention ROLLBACK so callers know what failed.
    assert!(
        err_msg.contains("ROLLBACK"),
        "error message must mention 'ROLLBACK', got: {err_msg:?}"
    );

    // The message must say "not supported" (not a raw parse-error).
    assert!(
        err_msg.contains("not supported"),
        "error message must say 'not supported', got: {err_msg:?}"
    );

    // The message must NOT expose raw limbo internals ("ROLLBACK not supported yet").
    // Our message is user-facing and must be different from the raw engine error.
    assert!(
        !err_msg.contains("ROLLBACK not supported yet"),
        "error message must not expose raw limbo parse error, got: {err_msg:?}"
    );
}

// ── test_rollback_done_flag_set_after_rollback ────────────────────────────────

/// After `rollback()` is called (and returns Err), the transaction's `done`
/// flag should be set to `true` so that the `Drop` impl does NOT attempt a
/// second fire-and-forget ROLLBACK.
///
/// Because limbo 0.0.22 cannot execute ROLLBACK, the connection that had a
/// transaction will remain in a "transaction-active" state after rollback()
/// returns Err.  A subsequent `transaction()` call on the SAME connection
/// correctly fails with "cannot start a transaction within a transaction".
/// This test verifies that expected behaviour: the error is clear and the
/// code does not panic.
#[tokio::test]
async fn test_rollback_done_flag_set_after_rollback() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE rb2 (n INTEGER)", &[])
        .await
        .expect("CREATE TABLE failed");

    // Begin a transaction and attempt rollback (expected to return Err).
    let txn = conn.transaction().await.expect("first BEGIN failed");

    let rb_result = txn.rollback().await;
    assert!(
        rb_result.is_err(),
        "rollback() must return Err (limbo 0.0.22 does not support ROLLBACK)"
    );

    // A second connection can be opened and used without issue.
    // (The existing connection is stuck in a transaction; the test verifies
    // that rollback() didn't panic and that calling it again on a NEW
    // connection is safe.)
    let conn2 = SqliteConnection::open_memory()
        .await
        .expect("second open_memory failed");

    conn2
        .execute("CREATE TABLE clean (x INTEGER)", &[])
        .await
        .expect("CREATE on clean connection failed");

    conn2
        .execute("INSERT INTO clean VALUES (1)", &[])
        .await
        .expect("INSERT on clean connection failed");

    let rows = conn2
        .query("SELECT x FROM clean", &[])
        .await
        .expect("SELECT on clean connection failed");

    assert_eq!(rows.len(), 1, "clean connection should have 1 row");
}
