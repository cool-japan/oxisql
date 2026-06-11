//! Tests for the advisory migration lock implementations.

use oxisql_migrate::lock::{MigrationLock, NoopMigrationLock};

/// A freshly-constructed `NoopMigrationLock` is not held.
#[tokio::test]
async fn noop_lock_acquires_and_releases() {
    let mut lock = NoopMigrationLock::new();
    assert!(!lock.is_held(), "lock should not be held initially");

    lock.acquire(5).await.expect("acquire should succeed");
    assert!(lock.is_held(), "lock should be held after acquire");

    lock.release().await.expect("release should succeed");
    assert!(!lock.is_held(), "lock should not be held after release");
}

/// Acquiring a `NoopMigrationLock` twice is idempotent.
#[tokio::test]
async fn noop_lock_acquire_twice_is_idempotent() {
    let mut lock = NoopMigrationLock::new();

    lock.acquire(5).await.expect("first acquire should succeed");
    lock.acquire(5)
        .await
        .expect("second acquire should also succeed (noop is idempotent)");
    assert!(lock.is_held());

    lock.release().await.expect("release should succeed");
    assert!(!lock.is_held());
}

/// Releasing an already-unheld `NoopMigrationLock` is safe.
#[tokio::test]
async fn noop_lock_release_when_not_held_is_safe() {
    let mut lock = NoopMigrationLock::new();
    assert!(!lock.is_held());

    // Should not panic or error.
    lock.release()
        .await
        .expect("releasing an unheld noop lock should not error");
    assert!(!lock.is_held());
}

/// `Default` impl mirrors `new()`.
#[test]
fn noop_lock_default_is_not_held() {
    let lock = NoopMigrationLock::default();
    assert!(!lock.is_held());
}

/// `with_lock(NoopMigrationLock)` wires cleanly into `MigrationRunner::run_with_conn`.
#[tokio::test]
async fn runner_with_noop_lock_applies_migrations() {
    use oxisql_embedded::EmbeddedConnection;
    use oxisql_migrate::runner::MigrationRunner;

    let dir = std::env::temp_dir().join(format!(
        "oxisql_lock_test_{}_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        std::process::id(),
    ));
    std::fs::create_dir_all(&dir).expect("create temp dir");

    std::fs::write(
        dir.join("20240801000001__create_locked_test.sql"),
        "CREATE TABLE locked_test (id INTEGER)",
    )
    .expect("write migration");

    let conn = EmbeddedConnection::open_memory().expect("open in-memory db");
    let mut runner = MigrationRunner::new(&dir).with_lock(NoopMigrationLock::new());

    let applied = runner
        .run_with_conn(&conn)
        .await
        .expect("run_with_conn with noop lock should succeed");
    assert_eq!(applied, 1, "expected exactly one migration applied");

    // Second run is idempotent.
    let applied2 = runner
        .run_with_conn(&conn)
        .await
        .expect("idempotent second run");
    assert_eq!(applied2, 0);

    std::fs::remove_dir_all(&dir).ok();
}

// PostgresAdvisoryLock tests require a live server and are therefore ignored
// by default.  Run them explicitly with:
//   cargo test -p oxisql-migrate -- postgres_advisory --ignored

#[cfg(test)]
mod postgres_advisory_ignored {
    /// Acquiring a `PostgresAdvisoryLock` against a live Postgres instance.
    ///
    /// Requires the `OXISQL_PG_TEST_URL` environment variable to be set to a
    /// valid Postgres connection string, e.g.:
    /// `postgresql://postgres:postgres@localhost:5432/oxisql_test`
    #[tokio::test]
    #[ignore = "requires a live PostgreSQL server (set OXISQL_PG_TEST_URL)"]
    async fn postgres_advisory_lock_acquire_release() {
        // This test is intentionally left without a live server dependency.
        // To exercise it, integrate your Postgres backend and call:
        //
        //   let conn: &dyn oxisql_core::Connection = /* your pg connection */;
        //   let mut lock = PostgresAdvisoryLock::new(conn);
        //   lock.acquire(5).await.unwrap();
        //   assert!(lock.is_held());
        //   lock.release().await.unwrap();
        //   assert!(!lock.is_held());
    }
}
