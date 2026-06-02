#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxidb-pool` — async connection pooling for OxiDB.
//!
//! Provides three pool variants gated behind feature flags:
//!
//! | Feature     | Type                  | Backend                            |
//! |-------------|----------------------|------------------------------------|
//! | `postgres`  | `postgres::OxidbPgPool`  | `deadpool-postgres` wrapper   |
//! | `mysql`     | `mysql::MysqlPool`       | Custom `deadpool::managed::Manager` over `mysql_async::Conn` |
//! | `embedded`  | `embedded::EmbeddedPool` | `Arc<Mutex<Glue<MemoryStorage>>>` no-op |
//!
//! All features are **opt-in** — `default = []`.
//!
//! # Example — embedded (no external services)
//!
//! ```rust
//! # #[cfg(feature = "embedded")]
//! # {
//! use oxisql_pool::embedded::EmbeddedPool;
//! let pool = EmbeddedPool::new();
//! // In async code: let mut glue = pool.get().await;
//! let _ = pool;
//! # }
//! ```

/// Pool error type covering all backends.
#[derive(Debug, thiserror::Error)]
pub enum PoolError {
    /// A Postgres pool error.
    #[cfg(feature = "postgres")]
    #[error("postgres pool error: {0}")]
    Postgres(#[from] deadpool_postgres::PoolError),
    /// Failure creating a Postgres pool.
    #[cfg(feature = "postgres")]
    #[error("postgres pool creation error: {0}")]
    CreatePool(deadpool_postgres::CreatePoolError),
    /// A MySQL pool error.
    #[cfg(feature = "mysql")]
    #[error("mysql pool error: {0}")]
    Mysql(deadpool::managed::PoolError<mysql_async::Error>),
    /// A MySQL URL parse error.
    #[cfg(feature = "mysql")]
    #[error("mysql url error: {0}")]
    MysqlUrl(mysql_async::UrlError),
    /// A SQLite pool error (Pure-Rust Limbo backend).
    #[cfg(feature = "sqlite")]
    #[error("sqlite pool error: {0}")]
    Sqlite(deadpool::managed::PoolError<oxisql_core::OxiSqlError>),
    /// A generic build or configuration error (message string).
    #[error("pool build error: {0}")]
    Build(String),
    /// No pool backend enabled.
    #[allow(dead_code)]
    #[error("no pool backend is enabled")]
    NoBackend,
}

// ── Pool metrics ─────────────────────────────────────────────────────────────

/// A snapshot of current pool utilisation metrics.
///
/// Obtain via [`OxidbPool::metrics`].
#[derive(Debug, Clone, Default)]
pub struct PoolMetrics {
    /// Maximum number of connections the pool will create.
    pub max_size: usize,
    /// Number of connections currently checked out (in use).
    pub active: usize,
    /// Number of connections currently idle (available for checkout).
    pub idle: usize,
    /// Number of waiters blocked waiting for a connection.
    pub wait_count: usize,
    /// Total number of successful connection checkouts since pool creation.
    pub acquired_total: u64,
    /// Total number of connections returned to the pool since creation.
    ///
    /// For the embedded backend this is incremented on every `on_checkin` event;
    /// for Postgres and MySQL backends it is `0` (deadpool does not expose a
    /// cumulative checkin counter).
    pub released_total: u64,
    /// Total number of checkout attempts that timed out since pool creation.
    ///
    /// Currently `0` for all backends — timeout tracking is a future enhancement.
    pub timeout_count: u64,
}

// ── Pool configuration builder ───────────────────────────────────────────────

/// Configuration parameters for a connection pool.
///
/// Create an instance via [`PoolConfigBuilder`] and pass it to pool
/// constructors that accept it.
///
/// # Example
///
/// ```rust
/// use oxisql_pool::{PoolConfig, PoolConfigBuilder};
///
/// let config = PoolConfigBuilder::new()
///     .max_size(20)
///     .min_idle(2)
///     .connect_timeout_ms(5_000)
///     .idle_timeout_ms(300_000)
///     .build();
///
/// assert_eq!(config.max_size, 20);
/// ```
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Maximum number of connections in the pool.
    pub max_size: usize,
    /// Minimum number of idle connections to keep alive, if set.
    pub min_idle: Option<usize>,
    /// Timeout (milliseconds) waiting for a connection checkout, if set.
    pub connect_timeout_ms: Option<u64>,
    /// Time (milliseconds) after which idle connections are closed, if set.
    pub idle_timeout_ms: Option<u64>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_size: 10,
            min_idle: None,
            connect_timeout_ms: Some(30_000),
            idle_timeout_ms: Some(600_000),
        }
    }
}

/// Builder for [`PoolConfig`].
///
/// Start with [`PoolConfigBuilder::new`] and chain setter methods, then call
/// [`PoolConfigBuilder::build`] to produce a [`PoolConfig`].
#[derive(Debug, Clone, Default)]
pub struct PoolConfigBuilder {
    config: PoolConfig,
}

impl PoolConfigBuilder {
    /// Create a new builder with the [`PoolConfig`] default values.
    pub fn new() -> Self {
        Self {
            config: PoolConfig::default(),
        }
    }

    /// Set the maximum number of connections.
    pub fn max_size(mut self, n: usize) -> Self {
        self.config.max_size = n;
        self
    }

    /// Set the minimum number of idle connections.
    pub fn min_idle(mut self, n: usize) -> Self {
        self.config.min_idle = Some(n);
        self
    }

    /// Set the connection checkout timeout in milliseconds.
    pub fn connect_timeout_ms(mut self, ms: u64) -> Self {
        self.config.connect_timeout_ms = Some(ms);
        self
    }

    /// Set the idle connection expiry timeout in milliseconds.
    pub fn idle_timeout_ms(mut self, ms: u64) -> Self {
        self.config.idle_timeout_ms = Some(ms);
        self
    }

    /// Consume the builder and return the [`PoolConfig`].
    pub fn build(self) -> PoolConfig {
        self.config
    }
}

// ── Connection lifecycle hooks ────────────────────────────────────────────────

/// Callbacks invoked at connection lifecycle events.
///
/// Each hook is optional; `None` means no-op.
///
/// # Example
///
/// ```rust
/// use oxisql_pool::PoolHooks;
///
/// let hooks = PoolHooks::new()
///     .on_create(|| println!("connection created"))
///     .on_checkout(|| println!("connection checked out"));
/// ```
#[derive(Clone, Default)]
pub struct PoolHooks {
    /// Called when a new connection is created (pool startup or expansion).
    pub on_create: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// Called when a connection is checked out of the pool.
    pub on_checkout: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    /// Called when a connection is returned (checked in) to the pool.
    pub on_checkin: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for PoolHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolHooks")
            .field("on_create", &self.on_create.is_some())
            .field("on_checkout", &self.on_checkout.is_some())
            .field("on_checkin", &self.on_checkin.is_some())
            .finish()
    }
}

impl PoolHooks {
    /// Create a new `PoolHooks` with no callbacks set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the `on_create` callback, fired once when hooks are attached.
    pub fn on_create(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_create = Some(std::sync::Arc::new(f));
        self
    }

    /// Set the `on_checkout` callback, fired on each `EmbeddedPool::get` call.
    pub fn on_checkout(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_checkout = Some(std::sync::Arc::new(f));
        self
    }

    /// Set the `on_checkin` callback.
    ///
    /// NOTE: for the embedded pool, `on_checkin` is registered but not yet
    /// automatically fired on lock release.  This is a future enhancement.
    pub fn on_checkin(mut self, f: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_checkin = Some(std::sync::Arc::new(f));
        self
    }

    /// Fire the `on_create` hook if one is registered.
    #[cfg(feature = "embedded")]
    pub(crate) fn fire_create(&self) {
        if let Some(f) = &self.on_create {
            f();
        }
    }

    /// Fire the `on_checkout` hook if one is registered.
    #[cfg(feature = "embedded")]
    pub(crate) fn fire_checkout(&self) {
        if let Some(f) = &self.on_checkout {
            f();
        }
    }

    /// Fire the `on_checkin` hook if one is registered.
    #[allow(dead_code)]
    pub(crate) fn fire_checkin(&self) {
        if let Some(f) = &self.on_checkin {
            f();
        }
    }
}

// ── Backend modules ──────────────────────────────────────────────────────────

/// PostgreSQL pool (requires the `postgres` feature).
#[cfg(feature = "postgres")]
pub mod postgres;

/// MySQL pool via custom deadpool Manager (requires the `mysql` feature).
#[cfg(feature = "mysql")]
pub mod mysql;

/// Embedded in-memory pool wrapper (requires the `embedded` feature).
#[cfg(feature = "embedded")]
pub mod embedded;

/// Pure-Rust SQLite pool via Limbo (requires the `sqlite` or `sqlite-compat` feature).
///
/// This module exposes `SqlitePool`, `SqliteManager`, and `new_sqlite_pool`
/// backed by the Limbo engine (`oxisql-sqlite-compat`).  There are no C
/// dependencies.
#[cfg(feature = "sqlite")]
pub mod sqlite_compat;

/// Convenience re-export: `oxisql_pool::sqlite` is an alias for
/// `oxisql_pool::sqlite_compat` when the `sqlite` feature is active.
///
/// This allows existing code that imports `oxisql_pool::sqlite::SqlitePool`
/// to continue working without changes.
#[cfg(feature = "sqlite")]
pub use sqlite_compat as sqlite;

/// C-backed SQLite pool via rusqlite (requires the `sqlite-rusqlite` feature).
///
/// This is a legacy escape-hatch for code that explicitly needs the C-backed
/// SQLite driver.  Prefer the `sqlite` feature (Limbo, Pure Rust) instead.
#[cfg(feature = "sqlite-rusqlite")]
pub mod sqlite_rusqlite;

/// SQL-backed key-value store (see `kv_store::EmbeddedKvStore` and
/// `kv_store::OxidbKvStore`).
pub mod kv_store;

// ── Unified pool enum ────────────────────────────────────────────────────────

/// Unified pool enum spanning all enabled backends.
///
/// Construct the appropriate variant and store it in application state.
pub enum OxidbPool {
    /// A PostgreSQL connection pool.
    #[cfg(feature = "postgres")]
    Postgres(postgres::OxidbPgPool),
    /// A MySQL connection pool.
    #[cfg(feature = "mysql")]
    Mysql(mysql::MysqlPool),
    /// An embedded in-memory pool.
    #[cfg(feature = "embedded")]
    Embedded(embedded::EmbeddedPool),
    /// A Pure-Rust SQLite connection pool (Limbo backend).
    #[cfg(feature = "sqlite")]
    Sqlite(sqlite_compat::SqliteCompatPool),
}

impl OxidbPool {
    /// Ping the backing pool to verify it is healthy.
    ///
    /// For Postgres and MySQL backends this issues a lightweight query against
    /// a live connection.  For the embedded backend it checks that the pool
    /// has not been closed.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] if the pool is closed, exhausted, or the ping
    /// query fails.
    pub async fn health_check(&self) -> Result<(), PoolError> {
        match self {
            #[cfg(feature = "postgres")]
            OxidbPool::Postgres(p) => p.health_check().await,
            #[cfg(feature = "mysql")]
            OxidbPool::Mysql(p) => p.health_check().await,
            #[cfg(feature = "embedded")]
            OxidbPool::Embedded(p) => p.pool_health_check().await,
            #[cfg(feature = "sqlite")]
            OxidbPool::Sqlite(p) => p.health_check().await,
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => Err(PoolError::NoBackend),
        }
    }

    /// Return a snapshot of the pool's current utilisation metrics.
    pub fn metrics(&self) -> PoolMetrics {
        match self {
            #[cfg(feature = "postgres")]
            OxidbPool::Postgres(p) => p.metrics(),
            #[cfg(feature = "mysql")]
            OxidbPool::Mysql(p) => p.metrics(),
            #[cfg(feature = "embedded")]
            OxidbPool::Embedded(p) => p.metrics(),
            #[cfg(feature = "sqlite")]
            OxidbPool::Sqlite(p) => p.metrics(),
            #[cfg(not(any(
                feature = "postgres",
                feature = "mysql",
                feature = "embedded",
                feature = "sqlite"
            )))]
            _ => PoolMetrics::default(),
        }
    }
}
