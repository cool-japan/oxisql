//! Advisory distributed locking for migration runs.
//!
//! In multi-process deployments (e.g. Kubernetes, horizontal scaling) two
//! processes can simultaneously run migrations and corrupt the tracker table.
//! The `MigrationLock` trait provides an advisory-lock guard that the
//! [`MigrationRunner`](crate::runner::MigrationRunner) acquires before running
//! migrations and releases afterwards.
//!
//! Two implementations are shipped out of the box:
//!
//! * [`NoopMigrationLock`](crate::lock::NoopMigrationLock) — always succeeds.
//!   Use for embedded / in-memory backends where multiple processes cannot share
//!   the same database.
//! * [`PostgresAdvisoryLock`](crate::lock::PostgresAdvisoryLock) — uses
//!   `pg_try_advisory_lock` / `pg_advisory_unlock` for PostgreSQL deployments.

use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::MigrationError;

// ── Lock key ────────────────────────────────────────────────────────────────

/// A stable 64-bit key identifying OxiSQL migration advisory locks.
///
/// The bytes spell `"OxiSQLmi"` in ASCII / UTF-8.
const OXISQL_MIGRATE_LOCK_KEY: i64 = 0x4F786953514C6D69_u64 as i64;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A guard that holds an advisory lock on the database.
///
/// The lock is acquired via [`acquire`](MigrationLock::acquire) and must be
/// explicitly released via [`release`](MigrationLock::release).  The
/// [`MigrationRunner`](crate::runner::MigrationRunner) calls both methods
/// automatically — callers typically do not need to drive the lock directly.
#[async_trait]
pub trait MigrationLock: Send + Sync {
    /// Attempt to acquire the advisory lock, retrying until `timeout_secs`
    /// seconds have elapsed.
    ///
    /// Returns `Err(`[`MigrationError::LockTimeout`]`)` if the lock cannot be
    /// acquired within the deadline.
    async fn acquire(&mut self, timeout_secs: u32) -> Result<(), MigrationError>;

    /// Release the advisory lock.
    ///
    /// This is a no-op when the lock is not currently held.
    async fn release(&mut self) -> Result<(), MigrationError>;

    /// Returns `true` if this instance currently holds the advisory lock.
    fn is_held(&self) -> bool;
}

// ── NoopMigrationLock ────────────────────────────────────────────────────────

/// A no-op advisory lock for backends that do not support distributed locking.
///
/// Use this with embedded in-memory or local file-backed backends where
/// multiple processes never share the same database file.  Every call to
/// [`acquire`](MigrationLock::acquire) immediately succeeds.
pub struct NoopMigrationLock {
    held: bool,
}

impl NoopMigrationLock {
    /// Create a new [`NoopMigrationLock`] in the unheld state.
    pub fn new() -> Self {
        Self { held: false }
    }
}

impl Default for NoopMigrationLock {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MigrationLock for NoopMigrationLock {
    async fn acquire(&mut self, _timeout_secs: u32) -> Result<(), MigrationError> {
        self.held = true;
        Ok(())
    }

    async fn release(&mut self) -> Result<(), MigrationError> {
        self.held = false;
        Ok(())
    }

    fn is_held(&self) -> bool {
        self.held
    }
}

// ── PostgresAdvisoryLock ─────────────────────────────────────────────────────

/// Advisory lock backed by PostgreSQL's session-level `pg_try_advisory_lock` /
/// `pg_advisory_unlock`.
///
/// The lock is keyed on `OXISQL_MIGRATE_LOCK_KEY` (`"OxiSQLmi"` as bytes).
/// When a lock cannot be obtained immediately the implementation retries every
/// 100 ms until `timeout_secs` elapses, then returns
/// [`MigrationError::LockTimeout`].
///
/// # Connection requirements
///
/// The connection must implement [`oxisql_core::Connection`].  Pass any
/// `&dyn Connection` or a concrete type.  The connection must target a
/// PostgreSQL server — the advisory-lock functions do not exist in other
/// backends.
pub struct PostgresAdvisoryLock<'a> {
    conn: &'a dyn oxisql_core::Connection,
    held: bool,
}

impl<'a> PostgresAdvisoryLock<'a> {
    /// Wrap `conn` for use as a PostgreSQL advisory lock.
    ///
    /// The lock is not acquired until [`acquire`](MigrationLock::acquire) is
    /// called.
    pub fn new(conn: &'a dyn oxisql_core::Connection) -> Self {
        Self { conn, held: false }
    }
}

#[async_trait]
impl MigrationLock for PostgresAdvisoryLock<'_> {
    async fn acquire(&mut self, timeout_secs: u32) -> Result<(), MigrationError> {
        let deadline = Instant::now() + Duration::from_secs(u64::from(timeout_secs));

        loop {
            // `pg_try_advisory_lock` returns a single boolean row.
            let rows = self
                .conn
                .query(
                    "SELECT pg_try_advisory_lock($1)",
                    &[&OXISQL_MIGRATE_LOCK_KEY],
                )
                .await
                .map_err(|e| MigrationError::Execution(e.to_string()))?;

            // Extract the boolean result from the first (and only) row.
            let acquired = rows
                .first()
                .and_then(|row| row.try_get_by_index::<bool>(0).ok())
                .unwrap_or(false);

            if acquired {
                self.held = true;
                return Ok(());
            }

            if Instant::now() >= deadline {
                return Err(MigrationError::LockTimeout(format!(
                    "could not acquire advisory lock within {timeout_secs}s \
                     — another migration process may still be running"
                )));
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn release(&mut self) -> Result<(), MigrationError> {
        if self.held {
            self.conn
                .execute("SELECT pg_advisory_unlock($1)", &[&OXISQL_MIGRATE_LOCK_KEY])
                .await
                .map_err(|e| MigrationError::Execution(e.to_string()))?;
            self.held = false;
        }
        Ok(())
    }

    fn is_held(&self) -> bool {
        self.held
    }
}
