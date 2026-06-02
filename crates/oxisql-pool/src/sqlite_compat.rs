//! Pure-Rust SQLite connection pool via a custom [`deadpool::managed::Manager`]
//! over `SqliteConnection` from `oxisql-sqlite-compat` (Limbo backend).
//!
//! This module provides an alternative to the `sqlite` (rusqlite, C-FFI) pool.
//! Unlike the C-backed pool, this one has zero C dependencies — all storage
//! logic runs through the [Limbo](https://github.com/tursodatabase/limbo) pure-Rust
//! SQLite engine.
//!
//! # Feature gate
//!
//! Enable with `oxisql-pool = { features = ["sqlite-compat"] }` in your
//! `Cargo.toml`.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_pool::sqlite_compat::new_sqlite_compat_pool;
//!
//! // File-backed pool
//! let pool = new_sqlite_compat_pool("/tmp/mydb.sqlite3", 4).await?;
//! let conn = pool.get().await?;
//! // use conn as a &SqliteConnection (via Deref)
//! # Ok(())
//! # }
//! ```
//!
//! # Connection lifecycle
//!
//! Each pool slot holds a single `SqliteConnection`.  When recycled, the
//! manager issues `SELECT 1` to verify the connection is still functional.
//! For in-memory databases (`:memory:`) each pool slot is independent — there
//! is no shared state between pool slots, which matches SQLite's behaviour for
//! separate in-memory databases.

use deadpool::managed::{Manager, Metrics, RecycleError, RecycleResult};
use oxisql_core::Connection as OxiConnection;
use oxisql_sqlite_compat::SqliteConnection;

/// A `deadpool::managed::Manager` that creates and recycles [`SqliteConnection`]
/// objects from the Limbo pure-Rust SQLite backend.
pub struct SqliteCompatManager {
    path: String,
}

impl SqliteCompatManager {
    /// Create a new manager that opens SQLite databases at `path`.
    ///
    /// Pass `":memory:"` for in-memory databases.  For file-backed databases
    /// each pool slot opens the same file — concurrent writes from multiple
    /// slots are serialised by SQLite's WAL mode.
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }
}

impl Manager for SqliteCompatManager {
    type Type = SqliteConnection;
    type Error = oxisql_core::OxiSqlError;

    async fn create(&self) -> Result<SqliteConnection, oxisql_core::OxiSqlError> {
        SqliteConnection::open(&self.path).await
    }

    async fn recycle(
        &self,
        conn: &mut SqliteConnection,
        _: &Metrics,
    ) -> RecycleResult<oxisql_core::OxiSqlError> {
        conn.ping().await.map_err(RecycleError::Backend)
    }
}

/// A thin newtype wrapper around the deadpool [`Pool`][deadpool::managed::Pool]
/// for [`SqliteCompatManager`].
///
/// Construct via [`new_sqlite_compat_pool`], then call [`SqliteCompatPool::get`]
/// to check out a pooled [`SqliteConnection`].
pub struct SqliteCompatPool(deadpool::managed::Pool<SqliteCompatManager>);

impl SqliteCompatPool {
    /// Check out a connection from the pool.
    ///
    /// # Errors
    ///
    /// Returns a [`deadpool::managed::PoolError`] if the pool is exhausted,
    /// closed, or the backend connection fails.
    pub async fn get(
        &self,
    ) -> Result<
        deadpool::managed::Object<SqliteCompatManager>,
        deadpool::managed::PoolError<oxisql_core::OxiSqlError>,
    > {
        self.0.get().await
    }

    /// Return the name of the backend powering this pool.
    pub fn backend_name(&self) -> &'static str {
        "sqlite"
    }

    /// Close the pool.
    ///
    /// No new connections will be handed out after this call.
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

    /// Verify the pool is healthy by checking out a connection and running
    /// `SELECT 1`.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError`] if the connection cannot be checked out or
    /// the query fails.
    pub async fn health_check(&self) -> Result<(), crate::PoolError> {
        let conn = self.0.get().await.map_err(crate::PoolError::Sqlite)?;
        conn.ping()
            .await
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
            wait_count: 0,
            acquired_total: 0,
            released_total: 0,
            timeout_count: 0,
        }
    }
}

/// Create a new [`SqliteCompatPool`] with the given database path and
/// `max_size`.
///
/// # Parameters
///
/// - `path` — File path for the SQLite database, or `":memory:"` for in-memory.
/// - `max_size` — Maximum number of simultaneous connections in the pool.
///
/// # Errors
///
/// Returns [`crate::PoolError::Build`] if pool construction fails (e.g. when
/// `max_size` is 0) or if the initial connection attempt fails.
pub async fn new_sqlite_compat_pool(
    path: impl Into<String>,
    max_size: usize,
) -> Result<SqliteCompatPool, crate::PoolError> {
    let manager = SqliteCompatManager::new(path);
    deadpool::managed::Pool::builder(manager)
        .max_size(max_size)
        .build()
        .map(SqliteCompatPool)
        .map_err(|e| crate::PoolError::Build(e.to_string()))
}

/// Canonical async constructor — equivalent to [`new_sqlite_compat_pool`].
///
/// This name matches the old `sqlite.rs` API so that code importing
/// `oxisql_pool::sqlite::new_sqlite_pool` continues to compile after the
/// feature was migrated to the Limbo (Pure-Rust) backend.
///
/// # Errors
///
/// Returns [`crate::PoolError::Build`] if pool construction fails.
pub async fn new_sqlite_pool(
    path: impl Into<String>,
    max_size: usize,
) -> Result<SqliteCompatPool, crate::PoolError> {
    new_sqlite_compat_pool(path, max_size).await
}

/// Type alias so that `SqlitePool` resolves to [`SqliteCompatPool`].
///
/// Downstream code that stored a `SqlitePool` value will continue to compile
/// without changes after the `sqlite` feature was migrated to the Pure-Rust
/// Limbo backend.
pub type SqlitePool = SqliteCompatPool;
