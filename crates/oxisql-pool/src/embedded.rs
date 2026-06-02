//! Embedded in-memory pool — a thin `Arc<Mutex<Glue<MemoryStorage>>>` wrapper.
//!
//! There is no real "pooling" here: GlueSQL's in-memory engine is single-instance
//! and single-threaded (`&mut self`), so the pool is just a shareable handle
//! protected by a `tokio::sync::Mutex`.
//!
//! # Example
//!
//! ```rust
//! use oxisql_pool::embedded::EmbeddedPool;
//!
//! let pool = EmbeddedPool::new();
//! // Acquire the lock in async context: let mut glue = pool.get().await;
//! let _ = pool;
//! ```

use async_trait::async_trait;
use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_core::{
    ColumnInfo, Connection, ConnectionPool, ForeignKeyInfo, IndexInfo, OxiSqlError,
    PreparedStatement, Row, TableInfo, ToSqlValue,
};
use oxisql_embedded::EmbeddedConnection;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use tokio::sync::Mutex;

use crate::{PoolError, PoolHooks};

/// RAII wrapper that increments the `released` counter and fires the checkin hook on drop.
///
/// Returned by [`ConnectionPool::get`] for [`EmbeddedPool`] so that every checkout
/// obtained through the [`ConnectionPool`] trait path is automatically tracked when
/// the caller drops the connection handle.
struct CheckinOnDrop {
    conn: Box<dyn Connection + Send>,
    released: Arc<AtomicU64>,
    hooks: Arc<PoolHooks>,
}

impl Drop for CheckinOnDrop {
    fn drop(&mut self) {
        self.released.fetch_add(1, Ordering::Relaxed);
        self.hooks.fire_checkin();
    }
}

#[async_trait]
impl Connection for CheckinOnDrop {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        self.conn.execute(sql, params).await
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        self.conn.query(sql, params).await
    }

    async fn transaction(&self) -> Result<Box<dyn oxisql_core::Transaction + '_>, OxiSqlError> {
        self.conn.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        self.conn.execute_batch(sql).await
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        self.conn.ping().await
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        self.conn.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.conn.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.conn.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.conn.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.conn.foreign_keys(table).await
    }
}

/// A shared, cloneable handle to a single GlueSQL in-memory storage instance.
///
/// All access is serialised through the inner `tokio::sync::Mutex`.  Clone the
/// pool to share it across async tasks.  Call [`EmbeddedPool::close`] to
/// prevent future checkouts; subsequent calls to [`EmbeddedPool::get`] will
/// return `Err(PoolError::Build(…))`.
///
/// Optional lifecycle hooks can be registered via [`EmbeddedPool::with_hooks`].
/// The `on_checkout` hook fires on every [`EmbeddedPool::get`] call.
///
/// The pool tracks cumulative checkout and checkin counts via atomic counters,
/// accessible through [`EmbeddedPool::metrics`].
#[derive(Clone, Debug)]
pub struct EmbeddedPool {
    inner: Arc<Mutex<Glue<MemoryStorage>>>,
    /// Set to `true` once [`close`](EmbeddedPool::close) is called.
    closed: Arc<AtomicBool>,
    /// Lifecycle hooks (on_create / on_checkout / on_checkin).
    hooks: Arc<PoolHooks>,
    /// Total successful checkouts since pool creation (both `get` paths).
    acquired: Arc<AtomicU64>,
    /// Total connections returned (checkins) since pool creation.
    released: Arc<AtomicU64>,
}

impl EmbeddedPool {
    /// Create a new, empty embedded pool.
    pub fn new() -> Self {
        let storage = MemoryStorage::default();
        let glue = Glue::new(storage);
        Self {
            inner: Arc::new(Mutex::new(glue)),
            closed: Arc::new(AtomicBool::new(false)),
            hooks: Arc::new(PoolHooks::default()),
            acquired: Arc::new(AtomicU64::new(0)),
            released: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Attach lifecycle hooks to this pool.
    ///
    /// The `on_create` hook fires immediately when this method is called,
    /// treating hook attachment as a "pool creation" event.
    /// The `on_checkout` hook fires on every subsequent [`get`](EmbeddedPool::get).
    ///
    /// This method returns `Self` so it can be chained after [`EmbeddedPool::new`]:
    ///
    /// ```rust
    /// use oxisql_pool::{PoolHooks, embedded::EmbeddedPool};
    ///
    /// let pool = EmbeddedPool::new().with_hooks(PoolHooks::new().on_checkout(|| {}));
    /// let _ = pool;
    /// ```
    pub fn with_hooks(mut self, hooks: PoolHooks) -> Self {
        hooks.fire_create();
        self.hooks = Arc::new(hooks);
        self
    }

    /// Acquire an async lock on the inner [`Glue`] instance.
    ///
    /// Returns `Err(PoolError::Build(…))` if the pool has been closed via
    /// [`close`](EmbeddedPool::close).  Otherwise waits for the mutex and
    /// returns the guard.
    ///
    /// The `on_checkout` hook registered via [`with_hooks`](EmbeddedPool::with_hooks)
    /// fires before the mutex is acquired, each time this method is called.
    ///
    /// # Errors
    ///
    /// Returns `PoolError::Build` when the pool is closed.
    pub async fn get(&self) -> Result<tokio::sync::MutexGuard<'_, Glue<MemoryStorage>>, PoolError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(PoolError::Build("pool is closed".into()));
        }
        self.hooks.fire_checkout();
        self.acquired.fetch_add(1, Ordering::Relaxed);
        Ok(self.inner.lock().await)
    }

    /// Record a connection checkin (connection returned to the pool).
    ///
    /// Increments the `released_total` counter and fires the `on_checkin` hook
    /// if one is registered.  Because the embedded pool uses a `MutexGuard`
    /// rather than an explicit "return" call, callers must invoke this method
    /// manually after dropping the guard when accurate release tracking is needed.
    ///
    /// Connections obtained via [`ConnectionPool::get`] are automatically tracked
    /// through the internal checkin wrapper and do not require a manual call.
    pub fn checkin(&self) {
        self.released.fetch_add(1, Ordering::Relaxed);
        self.hooks.fire_checkin();
    }

    /// Mark the pool as closed.
    ///
    /// After this call every subsequent [`get`](EmbeddedPool::get) will return
    /// an error.  Clones of this pool share the same closed flag, so closing
    /// one handle closes all of them.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Return the name of the backend powering this pool.
    pub fn backend_name(&self) -> &'static str {
        "embedded"
    }

    /// Execute a raw SQL statement through the pool and return the number of
    /// affected rows.
    ///
    /// This is a convenience wrapper that acquires the lock, runs the
    /// statement, and releases it in one call.
    ///
    /// # Errors
    ///
    /// Returns `PoolError::Build` if the pool is closed or if the SQL
    /// execution fails.
    pub async fn execute(&self, sql: &str) -> Result<u64, PoolError> {
        let mut glue = self.get().await?;
        glue.execute(sql)
            .await
            .map_err(|e| PoolError::Build(e.to_string()))?;
        // GlueSQL Payload does not expose affected row count in a unified way;
        // return 0 as a sentinel — callers that need exact counts should use
        // `get()` directly.
        Ok(0)
    }

    /// Return a reference to the inner `Arc<Mutex<Glue<MemoryStorage>>>` for
    /// advanced use cases (e.g. passing to `oxidb-embedded` code).
    pub fn inner(&self) -> &Arc<Mutex<Glue<MemoryStorage>>> {
        &self.inner
    }

    /// Check pool health using the pool-level error type.
    ///
    /// Returns [`crate::PoolError::Build`] if the pool has been closed.
    /// This inherent method exists so that [`crate::OxidbPool::health_check`]
    /// can dispatch to it without ambiguity with the trait method which
    /// returns [`oxisql_core::OxiSqlError`].
    ///
    /// # Errors
    ///
    /// Returns `PoolError::Build("pool is closed")` when the pool is closed.
    pub async fn pool_health_check(&self) -> Result<(), crate::PoolError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(crate::PoolError::Build("pool is closed".into()));
        }
        Ok(())
    }

    /// Return a snapshot of the pool's current utilisation metrics.
    ///
    /// The embedded pool is a single shared instance, so `max_size` is always
    /// `1`.  `idle` is `1` when open, `0` when closed.  `acquired_total` is the
    /// cumulative number of successful checkouts, and `released_total` is the
    /// cumulative number of explicit [`checkin`](EmbeddedPool::checkin) calls,
    /// since pool creation.  `timeout_count` is always `0` (the embedded pool
    /// never times out).
    pub fn metrics(&self) -> crate::PoolMetrics {
        let closed = self.closed.load(Ordering::Acquire);
        crate::PoolMetrics {
            max_size: 1,
            active: 0,
            idle: if closed { 0 } else { 1 },
            wait_count: 0,
            acquired_total: self.acquired.load(Ordering::Relaxed),
            released_total: self.released.load(Ordering::Relaxed),
            timeout_count: 0,
        }
    }
}

impl Default for EmbeddedPool {
    fn default() -> Self {
        Self::new()
    }
}

// ── QueryBuilder integration (requires the `query-builder` feature) ───────────

#[cfg(feature = "query-builder")]
impl EmbeddedPool {
    /// Execute a [`oxisql_parse::QueryBuilder`] query against this pool,
    /// returning the result rows.
    ///
    /// This is a convenience method that builds the SQL from `qb`, acquires a
    /// pooled connection, and runs the query.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError`] when the pool is closed, the builder has no
    /// `FROM` table set, or query execution fails.
    pub async fn execute_query_builder(
        &self,
        qb: &oxisql_parse::QueryBuilder,
    ) -> Result<Vec<oxisql_core::Row>, crate::PoolError> {
        use oxisql_core::ConnectionPool;

        let sql = qb.build_ref().map_err(crate::PoolError::Build)?;

        let conn = <EmbeddedPool as ConnectionPool>::get(self)
            .await
            .map_err(|e| crate::PoolError::Build(e.to_string()))?;

        conn.query(&sql, &[])
            .await
            .map_err(|e| crate::PoolError::Build(e.to_string()))
    }
}

#[async_trait]
impl ConnectionPool for EmbeddedPool {
    /// Check out a connection from the embedded pool.
    ///
    /// Returns an [`EmbeddedConnection`] backed by the shared
    /// `Arc<Mutex<Glue<MemoryStorage>>>`, giving all "connections" a
    /// consistent view of the same in-memory storage.  The wrapper automatically
    /// increments `released_total` and fires the checkin hook when the returned
    /// connection is dropped, enabling accurate [`EmbeddedPool::metrics`] tracking
    /// through the [`ConnectionPool`] trait path.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::ConnectionPool`] when the pool has been closed.
    async fn get(&self) -> Result<Box<dyn Connection + Send>, OxiSqlError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(OxiSqlError::ConnectionPool("pool is closed".into()));
        }
        self.hooks.fire_checkout();
        self.acquired.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(CheckinOnDrop {
            conn: Box::new(EmbeddedConnection::from_arc(self.inner.clone())),
            released: Arc::clone(&self.released),
            hooks: Arc::clone(&self.hooks),
        }))
    }

    /// The embedded pool is a single shared instance; always returns `1`.
    fn pool_size(&self) -> usize {
        1
    }

    /// Returns `1` when the pool is open (ready to hand out a connection),
    /// `0` when it has been closed.
    fn idle_count(&self) -> usize {
        if self.closed.load(Ordering::Acquire) {
            0
        } else {
            1
        }
    }

    /// The embedded pool does not track active connections; always returns `0`.
    fn active_count(&self) -> usize {
        0
    }

    /// Verify the pool is healthy (checks that it has not been closed).
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::ConnectionPool`] when the pool is closed.
    async fn health_check(&self) -> Result<(), OxiSqlError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(OxiSqlError::ConnectionPool("pool is closed".into()));
        }
        Ok(())
    }

    /// Close the pool, preventing future checkouts.
    async fn close(&self) {
        EmbeddedPool::close(self);
    }
}
