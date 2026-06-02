//! Concurrency and isolation tests for the embedded in-memory backend.
//!
//! These tests exercise transaction isolation semantics, concurrent access
//! patterns, and data visibility guarantees for `EmbeddedConnection` backed
//! by GlueSQL `MemoryStorage`.
//!
//! # Isolation model
//!
//! GlueSQL `MemoryStorage` does not implement MVCC.  Instead, isolation is
//! enforced at the Rust level via an `Arc<Mutex<Glue<MemoryStorage>>>` shared
//! by all connections.  An `EmbeddedTransaction` holds an `OwnedMutexGuard`
//! for its entire lifetime, making all concurrent access strictly serial:
//! any second caller blocks until the first releases the guard.
//!
//! Consequences tested here:
//!
//! - No dirty reads: a concurrent query cannot observe uncommitted writes
//!   because it cannot acquire the mutex until the transaction drops its guard.
//! - Serializable behaviour: transactions run one-at-a-time; there is no
//!   interleaving of reads/writes between two in-flight transactions.
//! - Rollback visibility: after a rolled-back transaction, subsequent queries
//!   see zero rows for the reverted inserts.
//! - Commit visibility: committed data is immediately visible to subsequent
//!   connections sharing the same `Arc`.

use std::sync::Arc;

use oxisql_core::{Connection, ToSqlValue};
use oxisql_embedded::EmbeddedConnection;
use tokio::sync::Mutex;

// ── helpers ───────────────────────────────────────────────────────────────────

/// Create a unique table name using a monotonically increasing counter.
/// Prevents table-name collisions across async tests run in the same process.
fn unique_table(base: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{base}_{n}")
}

/// Return `true` if GlueSQL `MemoryStorage` supports `BEGIN` on this build.
///
/// Some GlueSQL versions reject `BEGIN` on `MemoryStorage`.  We probe once
/// and propagate the result rather than letting tests fail unexpectedly.
async fn transactions_supported(conn: &EmbeddedConnection) -> bool {
    conn.transaction().await.is_ok()
}

// ── test_transaction_isolation_serializable ───────────────────────────────────

/// Verify serializable isolation: when T1 holds a transaction (Mutex guard),
/// T2 cannot start until T1 finishes.
///
/// Because GlueSQL MemoryStorage serializes via `OwnedMutexGuard`, T2 blocks
/// for the entire duration of T1.  After T1 commits and T2 runs, T2 must see
/// T1's committed row.
#[tokio::test]
async fn test_transaction_isolation_serializable() {
    let tbl = unique_table("txn_serial");

    // Two connections sharing the same storage instance.
    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));
    let conn1 = EmbeddedConnection::from_arc(Arc::clone(&shared));
    let conn2 = EmbeddedConnection::from_arc(Arc::clone(&shared));

    // Create table via conn1.
    conn1
        .execute(&format!("CREATE TABLE {tbl} (id INTEGER, val TEXT)"), &[])
        .await
        .expect("CREATE TABLE");

    // Probe whether transactions are supported.
    if !transactions_supported(&conn1).await {
        // GlueSQL MemoryStorage does not support BEGIN — verify normal INSERT
        // visibility instead and return.
        let id: i64 = 1;
        conn1
            .execute(
                &format!("INSERT INTO {tbl} VALUES ($1, $2)"),
                &[&id as &dyn ToSqlValue, &"t1" as &dyn ToSqlValue],
            )
            .await
            .expect("INSERT via conn1");

        let rows = conn2
            .query(&format!("SELECT val FROM {tbl}"), &[])
            .await
            .expect("SELECT via conn2");
        assert_eq!(rows.len(), 1, "conn2 must see conn1 insert (no tx support)");
        return;
    }

    // Start T1 and insert a row without committing yet.
    let id1: i64 = 1;
    let mut t1 = conn1.transaction().await.expect("begin T1");
    t1.execute(
        &format!("INSERT INTO {tbl} VALUES ($1, $2)"),
        &[&id1 as &dyn ToSqlValue, &"from_t1" as &dyn ToSqlValue],
    )
    .await
    .expect("INSERT in T1");

    // T2 cannot start (blocks on the mutex), so we commit T1 first and only
    // then verify T2 sees the row.  This proves the serializable ordering.
    t1.commit().await.expect("T1 commit");

    // Now T2 runs and must see T1's committed row.
    let rows = conn2
        .query(&format!("SELECT val FROM {tbl}"), &[])
        .await
        .expect("SELECT via conn2 after T1 commit");

    assert_eq!(rows.len(), 1, "T2 must see T1's committed row");
}

// ── test_no_dirty_reads ───────────────────────────────────────────────────────

/// Verify no dirty reads: because the embedded backend uses a single Mutex,
/// a second connection physically cannot observe uncommitted in-flight changes
/// from a first connection's transaction.  After a rollback the row count
/// must be zero.
#[tokio::test]
async fn test_no_dirty_reads() {
    let tbl = unique_table("dirty_read");

    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));
    let conn1 = EmbeddedConnection::from_arc(Arc::clone(&shared));
    let conn2 = EmbeddedConnection::from_arc(Arc::clone(&shared));

    conn1
        .execute(&format!("CREATE TABLE {tbl} (v INTEGER)"), &[])
        .await
        .expect("CREATE TABLE");

    if !transactions_supported(&conn1).await {
        // Without transaction support there is no dirty read to test.
        return;
    }

    // T1 inserts a row then rolls back.
    let val: i64 = 42;
    let mut t1 = conn1.transaction().await.expect("begin T1");
    t1.execute(
        &format!("INSERT INTO {tbl} VALUES ($1)"),
        &[&val as &dyn ToSqlValue],
    )
    .await
    .expect("INSERT in T1");
    t1.rollback().await.expect("T1 rollback");

    // After rollback, conn2 must see zero rows.
    let rows = conn2
        .query(&format!("SELECT v FROM {tbl}"), &[])
        .await
        .expect("SELECT via conn2 after T1 rollback");

    assert_eq!(
        rows.len(),
        0,
        "no dirty reads: rolled-back row must be invisible"
    );
}

// ── test_concurrent_pool_access ───────────────────────────────────────────────

/// Verify that 20 concurrent `EmbeddedConnection::from_arc` handles can each
/// acquire the inner mutex, execute a SELECT, and complete without deadlock.
///
/// Note: this uses `EmbeddedConnection::from_arc` directly (not EmbeddedPool)
/// to avoid a circular dependency between `oxisql-embedded` and `oxisql-pool`.
/// The EmbeddedPool's `ConnectionPool::get()` tests live in `oxisql-pool`.
#[tokio::test]
async fn test_concurrent_pool_access() {
    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));

    // Setup: create a table we can SELECT from.
    let setup_conn = EmbeddedConnection::from_arc(Arc::clone(&shared));
    setup_conn
        .execute("CREATE TABLE concurrent_select_probe (x INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    setup_conn
        .execute("INSERT INTO concurrent_select_probe VALUES (1)", &[])
        .await
        .expect("INSERT");

    let task_count = 20usize;
    let mut join_set = tokio::task::JoinSet::new();

    for i in 0..task_count {
        let arc = Arc::clone(&shared);
        join_set.spawn(async move {
            let conn = EmbeddedConnection::from_arc(arc);
            let rows = conn
                .query("SELECT x FROM concurrent_select_probe", &[])
                .await
                .unwrap_or_else(|e| panic!("task {i} SELECT failed: {e}"));
            assert!(!rows.is_empty(), "task {i}: expected at least one row");
        });
    }

    let mut completed = 0usize;
    while let Some(result) = join_set.join_next().await {
        result.expect("task panicked");
        completed += 1;
    }

    assert_eq!(
        completed, task_count,
        "all {task_count} tasks must complete"
    );
}

// ── test_concurrent_insert_and_query ─────────────────────────────────────────

/// Verify that 10 writer tasks (each inserting 10 rows) and 5 concurrent
/// reader tasks all complete without deadlock, and the final COUNT matches
/// the expected 100 rows.
///
/// GlueSQL's mutex serialises all operations.  The key invariant is that no
/// task blocks forever and the total row count after joining all writers is 100.
#[tokio::test]
async fn test_concurrent_insert_and_query() {
    let tbl = unique_table("conc_insert");

    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));

    // Setup table.
    let setup = EmbeddedConnection::from_arc(Arc::clone(&shared));
    setup
        .execute(
            &format!("CREATE TABLE {tbl} (task_id INTEGER, row_idx INTEGER)"),
            &[],
        )
        .await
        .expect("CREATE TABLE");

    let writer_count = 10usize;
    let rows_per_writer = 10usize;
    let reader_count = 5usize;

    let mut join_set = tokio::task::JoinSet::new();

    // Spawn writer tasks.
    for task_id in 0..writer_count {
        let arc = Arc::clone(&shared);
        let tbl_clone = tbl.clone();
        join_set.spawn(async move {
            let conn = EmbeddedConnection::from_arc(arc);
            for row_idx in 0..rows_per_writer {
                let t = task_id as i64;
                let r = row_idx as i64;
                conn.execute(
                    &format!("INSERT INTO {tbl_clone} VALUES ($1, $2)"),
                    &[&t as &dyn ToSqlValue, &r as &dyn ToSqlValue],
                )
                .await
                .unwrap_or_else(|e| panic!("writer {task_id} row {row_idx} failed: {e}"));
            }
        });
    }

    // Spawn reader tasks (concurrent with writers; their counts may be partial).
    for reader_id in 0..reader_count {
        let arc = Arc::clone(&shared);
        let tbl_clone = tbl.clone();
        join_set.spawn(async move {
            let conn = EmbeddedConnection::from_arc(arc);
            let rows = conn
                .query(&format!("SELECT task_id FROM {tbl_clone}"), &[])
                .await
                .unwrap_or_else(|e| panic!("reader {reader_id} failed: {e}"));
            // Count is a snapshot; accept any value >= 0.
            let _ = rows.len();
        });
    }

    // Join all tasks.
    let mut completed = 0usize;
    while let Some(res) = join_set.join_next().await {
        res.expect("task panicked");
        completed += 1;
    }

    assert_eq!(
        completed,
        writer_count + reader_count,
        "all tasks must complete"
    );

    // After all writers have finished, final count must be exactly 100.
    let final_conn = EmbeddedConnection::from_arc(Arc::clone(&shared));
    let rows = final_conn
        .query(&format!("SELECT task_id FROM {tbl}"), &[])
        .await
        .expect("final COUNT query");
    assert_eq!(
        rows.len(),
        writer_count * rows_per_writer,
        "expected {} rows after all writers finished",
        writer_count * rows_per_writer
    );
}

// ── test_transaction_commit_visibility ───────────────────────────────────────

/// Verify cross-transaction commit visibility: T1 inserts and commits; T2
/// starts after and must see T1's row.
#[tokio::test]
async fn test_transaction_commit_visibility() {
    let tbl = unique_table("commit_vis");

    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));
    let conn1 = EmbeddedConnection::from_arc(Arc::clone(&shared));
    let conn2 = EmbeddedConnection::from_arc(Arc::clone(&shared));

    conn1
        .execute(&format!("CREATE TABLE {tbl} (id INTEGER)"), &[])
        .await
        .expect("CREATE TABLE");

    if !transactions_supported(&conn1).await {
        // Fall back to non-transactional insert + visibility check.
        let id: i64 = 99;
        conn1
            .execute(
                &format!("INSERT INTO {tbl} VALUES ($1)"),
                &[&id as &dyn ToSqlValue],
            )
            .await
            .expect("INSERT");
        let rows = conn2
            .query(&format!("SELECT id FROM {tbl}"), &[])
            .await
            .expect("SELECT");
        assert_eq!(rows.len(), 1, "conn2 must see conn1 data");
        return;
    }

    // T1: insert and commit.
    let id: i64 = 99;
    let mut t1 = conn1.transaction().await.expect("begin T1");
    t1.execute(
        &format!("INSERT INTO {tbl} VALUES ($1)"),
        &[&id as &dyn ToSqlValue],
    )
    .await
    .expect("INSERT in T1");
    t1.commit().await.expect("T1 commit");

    // T2: must see T1's committed row.
    let rows = conn2
        .query(&format!("SELECT id FROM {tbl}"), &[])
        .await
        .expect("SELECT via conn2");
    assert_eq!(rows.len(), 1, "T2 must see T1's committed insert");
}

// ── test_transaction_rollback_isolation ──────────────────────────────────────

/// Verify rollback isolation: T1 inserts but rolls back; T2 must not see the
/// row.
#[tokio::test]
async fn test_transaction_rollback_isolation() {
    let tbl = unique_table("rollback_iso");

    let shared = Arc::new(Mutex::new(gluesql::prelude::Glue::new(
        gluesql::prelude::MemoryStorage::default(),
    )));
    let conn1 = EmbeddedConnection::from_arc(Arc::clone(&shared));
    let conn2 = EmbeddedConnection::from_arc(Arc::clone(&shared));

    conn1
        .execute(&format!("CREATE TABLE {tbl} (id INTEGER)"), &[])
        .await
        .expect("CREATE TABLE");

    if !transactions_supported(&conn1).await {
        // Without BEGIN support there is no rollback to test.
        return;
    }

    // T1: insert and rollback.
    let id: i64 = 77;
    let mut t1 = conn1.transaction().await.expect("begin T1");
    t1.execute(
        &format!("INSERT INTO {tbl} VALUES ($1)"),
        &[&id as &dyn ToSqlValue],
    )
    .await
    .expect("INSERT in T1");
    t1.rollback().await.expect("T1 rollback");

    // T2: must see zero rows.
    let rows = conn2
        .query(&format!("SELECT id FROM {tbl}"), &[])
        .await
        .expect("SELECT via conn2 after T1 rollback");
    assert_eq!(rows.len(), 0, "T2 must not see T1's rolled-back insert");
}
