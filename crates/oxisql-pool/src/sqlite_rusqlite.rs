//! C-backed SQLite connection pool via a custom [`deadpool::managed::Manager`]
//! over `rusqlite::Connection`.
//!
//! This module is a **legacy escape-hatch** for code that explicitly requires
//! the C-backed SQLite driver.  For new code, prefer the `sqlite` feature
//! which uses the Pure-Rust Limbo engine via `oxisql-sqlite-compat`.
//!
//! Requires the **`sqlite-rusqlite`** feature flag.  The `bundled` cargo
//! feature on `rusqlite` compiles SQLite from the vendored C source, providing
//! a hermetic build with no system library requirement.
//!
//! # `:memory:` semantics
//!
//! Each call to `rusqlite::Connection::open(":memory:")` creates a *separate*,
//! independent in-memory database.  This means that in a pool with
//! `max_size > 1`, different checkouts will see different database states.
//! For shared state across checkouts use the
//! `file:mydb?mode=memory&cache=shared&uri=true` URI form, or a temp-file
//! path.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_pool::sqlite_rusqlite::new_sqlite_pool;
//!
//! let pool = new_sqlite_pool(":memory:", 4)?;
//! let conn = pool.get().await?;
//! conn.execute_batch("CREATE TABLE t (id INTEGER PRIMARY KEY)")?;
//! # Ok(())
//! # }
//! ```

use deadpool::managed::{Manager, Metrics, RecycleError, RecycleResult};
use rusqlite::Connection as SqliteConn;

// ── Manager ──────────────────────────────────────────────────────────────────

/// A `deadpool::managed::Manager` that creates and recycles `rusqlite::Connection`
/// objects.
///
/// Construct via [`SqliteManager::new`] and pass to
/// [`deadpool::managed::Pool::builder`].
pub struct SqliteManager {
    /// Database path: `":memory:"` for a per-connection in-memory DB, a file
    /// path for an on-disk DB, or a URI such as
    /// `"file:mydb?mode=memory&cache=shared&uri=true"`.
    path: String,
}

impl SqliteManager {
    /// Build a manager for the given SQLite database path or URI.
    ///
    /// Pass `":memory:"` for an ephemeral in-memory database (note: each
    /// pooled connection gets its own independent copy — see module docs).
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl Manager for SqliteManager {
    type Type = SqliteConn;
    type Error = rusqlite::Error;

    async fn create(&self) -> Result<SqliteConn, rusqlite::Error> {
        // `rusqlite::Connection::open` is a blocking C call; run it on the
        // blocking thread pool so the async executor is not stalled.
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || SqliteConn::open(&path))
            .await
            .map_err(|join_err| {
                rusqlite::Error::InvalidParameterName(format!(
                    "spawn_blocking join error: {join_err}"
                ))
            })?
    }

    async fn recycle(
        &self,
        conn: &mut SqliteConn,
        _metrics: &Metrics,
    ) -> RecycleResult<rusqlite::Error> {
        // A trivial ping to verify the connection is still usable.
        // `execute_batch` is microsecond-scale for `SELECT 1`; running it
        // inline on the async executor is acceptable for the recycle path.
        conn.execute_batch("SELECT 1")
            .map_err(RecycleError::Backend)
    }
}

// ── Pool newtype ──────────────────────────────────────────────────────────────

/// A thin newtype wrapper around the deadpool `Pool` for [`SqliteManager`].
///
/// Construct via [`new_sqlite_pool`], then call [`SqlitePool::get`] to check
/// out a pooled [`rusqlite::Connection`].
pub struct SqlitePool(deadpool::managed::Pool<SqliteManager>);

impl SqlitePool {
    /// Check out a connection from the pool.
    ///
    /// # Errors
    ///
    /// Returns a [`deadpool::managed::PoolError`] if the pool is exhausted,
    /// closed, or the backend connection fails.
    pub async fn get(
        &self,
    ) -> Result<
        deadpool::managed::Object<SqliteManager>,
        deadpool::managed::PoolError<rusqlite::Error>,
    > {
        self.0.get().await
    }

    /// Return the name of the backend powering this pool.
    pub fn backend_name(&self) -> &'static str {
        "sqlite"
    }

    /// Close the pool.
    ///
    /// No new connections will be handed out after this call.  Dropping the
    /// pool also releases all connections; this method is provided for API
    /// symmetry with other backends.
    pub fn close(&self) {
        self.0.close();
    }

    /// Return the maximum number of connections in the pool.
    pub fn max_size(&self) -> usize {
        self.0.status().max_size
    }

    /// Return the number of connections currently available (idle) in the pool.
    pub fn available(&self) -> usize {
        self.0.status().available
    }

    /// Verify the pool is healthy by checking out a connection and running a
    /// lightweight `SELECT 1` query.
    ///
    /// SQLite connections are always healthy once opened, but this checks that
    /// the pool can still hand out connections.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError`] if the pool is closed, exhausted, or the
    /// connection cannot execute the health check query.
    pub async fn health_check(&self) -> Result<(), crate::PoolError> {
        let conn = self
            .0
            .get()
            .await
            .map_err(|e| crate::PoolError::Build(e.to_string()))?;
        conn.execute_batch("SELECT 1")
            .map_err(|e| crate::PoolError::Build(e.to_string()))
    }

    /// Return a snapshot of the pool's current utilisation metrics.
    pub fn metrics(&self) -> crate::PoolMetrics {
        let s = self.0.status();
        let active = s.size.saturating_sub(s.available);
        crate::PoolMetrics {
            max_size: s.max_size,
            active,
            idle: s.available,
            wait_count: s.waiting,
            acquired_total: 0,
            released_total: 0,
            timeout_count: 0,
        }
    }
}

// ── Constructor ───────────────────────────────────────────────────────────────

/// Create a new [`SqlitePool`] backed by the given database path and bounded
/// to `max_size` simultaneous connections.
///
/// Pass `":memory:"` for a per-connection in-memory database.  Note that with
/// `max_size > 1` each connection sees an independent schema; for shared state
/// use a file path or a shared-cache URI (see module docs).
///
/// # Errors
///
/// Returns [`crate::PoolError::Build`] if pool construction fails (only
/// occurs when `max_size` is 0).
pub fn new_sqlite_pool(path: &str, max_size: usize) -> Result<SqlitePool, crate::PoolError> {
    let manager = SqliteManager::new(path);
    deadpool::managed::Pool::builder(manager)
        .max_size(max_size)
        .build()
        .map(SqlitePool)
        .map_err(|e| crate::PoolError::Build(e.to_string()))
}
