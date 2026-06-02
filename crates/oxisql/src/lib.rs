#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql` — the COOLJAPAN Pure-Rust SQL facade.
//!
//! Provides three entry-points:
//! - [`connect`] — returns a single boxed [`Connection`].
//! - [`connect_pooled`] — returns a boxed [`ConnectionPool`] for connection reuse.
//! - `connect_pool` — returns a typed `OxidbPool` for backend-specific pool access.
//!
//! # Supported URI Schemes
//!
//! | Scheme | Feature | Backend | Notes |
//! |--------|---------|---------|-------|
//! | `memory://` | `embedded` | GlueSQL in-memory | Always creates a fresh DB |
//! | `postgres://`, `postgresql://` | `postgres` | tokio-postgres | Pure Rust, no libpq |
//! | `mysql://` | `mysql` | mysql_async | Pure Rust, no libmysqlclient |
//! | `datafusion://` | `datafusion` | DataFusion context | Use `connect_datafusion` — not a `Connection` |
//! | `redb://path/to/file.db` | `redb` | redb persistent storage | Pure Rust file-backed embedded DB |
//! | `fjall://path/to/dir` | `fjall` | fjall LSM-tree storage | Pure Rust persistent embedded DB |
//! | `file:///path`, `file:` | `sled` | sled persistent storage | Pure Rust file-backed embedded DB; errors with a helpful message when `sled` is disabled |
//!
//! # Feature Flags
//!
//! | Feature      | URI scheme              | Backend                                |
//! |--------------|-------------------------|----------------------------------------|
//! | `embedded`   | `memory://`             | GlueSQL `MemoryStorage`                |
//! | `postgres`   | `postgres://` / `postgresql://` | tokio-postgres (Pure Rust, no libpq) |
//! | `mysql`      | `mysql://`              | mysql_async (Pure Rust, no libmysqlclient) |
//! | `datafusion` | `datafusion://` (OLAP layer) | Apache DataFusion query engine — use `connect_datafusion` |
//! | `redb`       | `redb://`               | redb-backed persistent embedded SQL    |
//! | `fjall`      | `fjall://`              | fjall-backed persistent embedded SQL   |
//!
//! # Pooled connections
//!
//! | URI prefix | Feature flag | Pool type |
//! |---|---|---|
//! | `memory://` | `pool-embedded` | `EmbeddedPool` |
//! | `postgres://` / `postgresql://` | `pool-postgres` | `OxidbPgPool` |
//! | `mysql://` | `pool-mysql` | `MysqlPool` |
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql::OxiSqlError> {
//! let conn = oxisql::connect("memory://").await?;
//! conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! ```rust,no_run
//! # #[cfg(feature = "pool-embedded")]
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql::OxiSqlError> {
//! // Boxed pool (erased type):
//! let pool = oxisql::connect_pooled("memory://", 4).await?;
//! let conn = pool.get().await?;
//! conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "pool-embedded"))]
//! # fn main() {}
//! ```
//!
//! ```rust,no_run
//! # #[cfg(feature = "pool-embedded")]
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Typed OxidbPool (backend-specific access):
//! let pool = oxisql::connect_pool("memory://", 4).await?;
//! pool.health_check().await?;
//! # Ok(())
//! # }
//! # #[cfg(not(feature = "pool-embedded"))]
//! # fn main() {}
//! ```
//!
//! ## Feature Combinations
//!
//! The following feature combinations are tested:
//!
//! | Combination | Description |
//! |-------------|-------------|
//! | `embedded` | In-memory only |
//! | `postgres` | PostgreSQL only |
//! | `mysql` | MySQL only |
//! | `embedded,postgres` | Both embedded and PG |
//! | `embedded,mysql` | Both embedded and MySQL |
//! | `all-backends` | All three backends |
//! | `all-backends,datafusion` | Full feature set |
//!
//! Run `cargo check --all-features` to verify all combinations.
//!
//! # Prelude
//!
//! Import the most commonly used types with:
//!
//! ```rust
//! use oxisql::prelude::*;
//! ```

pub use oxisql_core::{
    ColumnInfo, Connection, ConnectionMetrics, ConnectionPool, ForeignKeyInfo, FromValue,
    IndexInfo, LoggingConnection, MetricsConnection, MetricsSnapshot, OxiSqlError, RetryConnection,
    RetryPolicy, RetryPredicate, Row, RowSet, TableInfo, TableType, ToSqlValue, Transaction, Value,
};

// ── MultiConnection ───────────────────────────────────────────────────────────

mod multi;
pub use multi::MultiConnection;

// ── Logging middleware ────────────────────────────────────────────────────────

/// Query-logging middleware for wrapping any boxed [`Connection`].
///
/// The [`logging::LoggingConnection`] type wraps a `Box<dyn Connection>` and
/// logs every SQL operation with timing using the [`log`] crate.  See the
/// module documentation for details and examples.
///
/// Note: [`LoggingConnection`] (from `oxisql_core`) is also available at the
/// crate root as a generic wrapper.  Use `oxisql::logging::LoggingConnection`
/// when you hold a type-erased `Box<dyn Connection>` and need a labelled
/// wrapper; use the core variant when you have the concrete backend type.
pub mod logging;

// ── BackendInfo ───────────────────────────────────────────────────────────────

/// Metadata about a database backend that OxiSQL can connect to.
///
/// Use [`backend_info_for_uri`] to obtain an instance for a given URI.
///
/// # Example
///
/// ```rust
/// if let Some(info) = oxisql::backend_info_for_uri("memory://") {
///     assert_eq!(info.name, "embedded");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct BackendInfo {
    /// Short identifier for the backend (e.g. `"embedded"`, `"postgres"`, `"mysql"`).
    pub name: &'static str,
    /// Version string for this backend, if statically known.
    ///
    /// For the embedded backend this is the OxiSQL crate version.  For network
    /// backends (`postgres`, `mysql`) the server version is not known until
    /// after the connection handshake; `None` is returned here.
    pub version: Option<String>,
    /// Capability tokens supported by this backend.
    pub features: Vec<&'static str>,
}

impl BackendInfo {
    /// Metadata for the embedded (GlueSQL in-memory) backend.
    #[must_use]
    pub fn embedded() -> Self {
        Self {
            name: "embedded",
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            features: vec!["in-memory", "sql", "gluesql"],
        }
    }

    /// Metadata for the PostgreSQL backend.
    #[must_use]
    pub fn postgres() -> Self {
        Self {
            name: "postgres",
            version: None,
            features: vec!["tcp", "tls", "prepared-statements"],
        }
    }

    /// Metadata for the MySQL backend.
    #[must_use]
    pub fn mysql() -> Self {
        Self {
            name: "mysql",
            version: None,
            features: vec!["tcp", "tls", "prepared-statements"],
        }
    }

    /// Metadata for the DataFusion OLAP backend.
    #[must_use]
    pub fn datafusion_backend() -> Self {
        Self {
            name: "datafusion",
            version: None,
            features: vec!["olap", "arrow", "datafusion"],
        }
    }

    /// Metadata for the redb persistent embedded backend.
    #[must_use]
    pub fn redb() -> Self {
        Self {
            name: "redb",
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            features: vec!["persistent", "file-backed", "pure-rust", "redb"],
        }
    }

    /// Metadata for the fjall persistent embedded backend.
    #[must_use]
    pub fn fjall() -> Self {
        Self {
            name: "fjall",
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            features: vec![
                "persistent",
                "file-backed",
                "pure-rust",
                "lsm-tree",
                "fjall",
            ],
        }
    }

    /// Metadata for the Pure-Rust SQLite-compat (Limbo) backend.
    #[must_use]
    pub fn sqlite_compat() -> Self {
        Self {
            name: "sqlite",
            version: Some("limbo-0.0.22".to_string()),
            features: vec!["sqlite-compat", "pure-rust", "limbo", "embedded"],
        }
    }
}

/// Return [`BackendInfo`] for the backend that would handle `uri`, or `None`
/// when the URI scheme is not recognised.
///
/// This function does **not** open a connection; it is a pure dispatch based
/// on the URI prefix.
///
/// # Examples
///
/// ```rust
/// let info = oxisql::backend_info_for_uri("memory://").unwrap();
/// assert_eq!(info.name, "embedded");
///
/// let sqlite_info = oxisql::backend_info_for_uri("sqlite://foo");
/// // Returns Some with name "sqlite" when the sqlite feature is enabled;
/// // returns None when the feature is not enabled.
/// # let _ = sqlite_info;
///
/// let df = oxisql::backend_info_for_uri("datafusion://").unwrap();
/// assert_eq!(df.name, "datafusion");
/// ```
#[must_use]
pub fn backend_info_for_uri(uri: &str) -> Option<BackendInfo> {
    if uri.starts_with("memory://") {
        Some(BackendInfo::embedded())
    } else if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        Some(BackendInfo::postgres())
    } else if uri.starts_with("mysql://") {
        Some(BackendInfo::mysql())
    } else if uri.starts_with("datafusion://") {
        Some(BackendInfo::datafusion_backend())
    } else if uri.starts_with("redb://") {
        Some(BackendInfo::redb())
    } else if uri.starts_with("fjall://") {
        Some(BackendInfo::fjall())
    } else {
        #[cfg(feature = "sqlite")]
        if uri.starts_with("sqlite://") || uri == "sqlite::memory:" {
            return Some(BackendInfo::sqlite_compat());
        }
        None
    }
}

/// Direct re-export of the embedded connection type for callers that need
/// embedded-specific APIs.
#[cfg(feature = "embedded")]
pub use oxisql_embedded::EmbeddedConnection;

/// Direct re-export of the redb persistent connection type.
///
/// Requires the `redb` feature.
#[cfg(feature = "redb")]
pub use oxisql_embedded::RedbEmbeddedConnection;

/// Direct re-export of the fjall persistent connection type.
///
/// Requires the `fjall` feature.
#[cfg(feature = "fjall")]
pub use oxisql_embedded::FjallEmbeddedConnection;

/// Direct re-export of the sled persistent connection type.
///
/// Requires the `sled` feature.
#[cfg(feature = "sled")]
pub use oxisql_embedded::SledEmbeddedConnection;

/// Direct re-export of the Pure-Rust SQLite-compat connection type.
///
/// Requires the `sqlite` feature.
#[cfg(feature = "sqlite")]
pub use oxisql_sqlite_compat::SqliteConnection;

/// Options for establishing a database connection.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Connection timeout in milliseconds. None = no timeout.
    pub connect_timeout_ms: Option<u64>,
    /// Maximum pool size (used by `connect_pooled`).
    pub pool_size: Option<usize>,
    /// TLS mode — true to require TLS, false to disable.
    pub require_tls: bool,
}

impl ConnectOptions {
    /// Create a `ConnectOptions` with all defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the connection timeout.
    pub fn timeout_ms(mut self, ms: u64) -> Self {
        self.connect_timeout_ms = Some(ms);
        self
    }

    /// Set the pool size.
    pub fn pool_size(mut self, n: usize) -> Self {
        self.pool_size = Some(n);
        self
    }

    /// Require TLS.
    pub fn require_tls(mut self, require: bool) -> Self {
        self.require_tls = require;
        self
    }
}

/// Prelude module — import all commonly used types with `use oxisql::prelude::*`.
pub mod prelude {
    pub use oxisql_core::{
        ColumnInfo, Connection, ConnectionMetrics, ConnectionPool, ForeignKeyInfo, FromValue,
        IndexInfo, LoggingConnection, MetricsConnection, MetricsSnapshot, OxiSqlError,
        RetryConnection, RetryPolicy, RetryPredicate, Row, RowSet, TableInfo, TableType,
        ToSqlValue, Transaction, Value,
    };
}

/// Return the crate version string.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Re-exports from the PostgreSQL backend (requires the `postgres` feature).
#[cfg(feature = "postgres")]
pub mod postgres {
    pub use oxisql_postgres::{PgConnection, PgError, PgTransaction, TlsMode};
}

/// Re-exports from the MySQL backend (requires the `mysql` feature).
#[cfg(feature = "mysql")]
pub mod mysql {
    pub use oxisql_mysql::*;
}

/// Connect to DataFusion and return an `OxiSqlContext`.
///
/// Unlike [`connect`], which returns a boxed [`Connection`], DataFusion is an
/// OLAP engine — not a single-connection backend.  This function returns an
/// `OxiSqlContext` that can have oxisql-backed tables registered into it via
/// `datafusion::register_table`.
///
/// # URI schemes
///
/// - `datafusion://` — create a new, empty DataFusion context.
/// - `memory://` — alias for `datafusion://`; same result.
///
/// # Errors
///
/// Returns [`OxiSqlError::UnsupportedUri`] when the URI does not match
/// `datafusion://` or `memory://`.
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "datafusion")]
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// let ctx = oxisql::connect_datafusion("datafusion://").await?;
/// # Ok(())
/// # }
/// ```
#[cfg(feature = "datafusion")]
pub async fn connect_datafusion(uri: &str) -> Result<datafusion::OxiSqlContext, OxiSqlError> {
    if uri.starts_with("datafusion://") || uri == "memory://" {
        Ok(datafusion::context())
    } else {
        Err(OxiSqlError::UnsupportedUri(uri.to_string()))
    }
}

/// Re-exports and convenience helpers for the DataFusion bridge.
///
/// Requires the `datafusion` feature.
#[cfg(feature = "datafusion")]
pub mod datafusion {
    pub use oxisql_datafusion::{OxiSqlContext, OxiSqlFusionError, OxiSqlTableProvider};

    use arrow::datatypes::SchemaRef;
    use oxisql_core::Connection;
    use std::sync::Arc;

    /// Register an OxiSQL connection's table in a DataFusion context.
    ///
    /// After registration the table is queryable via DataFusion SQL.  The
    /// connection is kept alive (wrapped in `Arc`) so that DataFusion can
    /// issue queries against it at scan time.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] when registration fails (e.g. a table
    /// with the same name was already registered).
    pub fn register_table(
        ctx: &OxiSqlContext,
        name: &str,
        conn: Arc<dyn Connection>,
        schema: SchemaRef,
    ) -> Result<(), OxiSqlFusionError> {
        ctx.register_table(name, conn, schema)
    }

    /// Register a snapshot of an OxiSQL table into a DataFusion `SessionContext`.
    ///
    /// Executes `SELECT * FROM {table_name}` on `conn`, infers an Arrow schema
    /// from the first returned row, and registers the result as a DataFusion
    /// [`OxiSqlTableProvider`].  If the table is empty the function succeeds
    /// without registering anything (schema cannot be inferred from zero rows).
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlFusionError`] when the query fails or DataFusion rejects
    /// the registration.
    pub async fn register_embedded_table(
        ctx: &OxiSqlContext,
        conn: &dyn Connection,
        table_name: &str,
    ) -> Result<(), OxiSqlFusionError> {
        ctx.register_embedded_table(conn, table_name).await
    }

    /// Create a new DataFusion context pre-configured for OxiSQL.
    pub fn context() -> OxiSqlContext {
        OxiSqlContext::new()
    }
}

/// Re-exports from the connection pool crate (requires a `pool-*` feature).
///
/// | Feature          | Pool type                          |
/// |------------------|-----------------------------------|
/// | `pool-postgres`  | [`oxisql_pool::postgres::OxidbPgPool`] |
/// | `pool-mysql`     | [`oxisql_pool::mysql::MysqlPool`]       |
/// | `pool-embedded`  | [`oxisql_pool::embedded::EmbeddedPool`] |
#[cfg(feature = "pool-postgres")]
pub mod pool_postgres {
    pub use oxisql_pool::postgres::OxidbPgPool;
}

/// Re-exports from the MySQL connection pool (requires the `pool-mysql` feature).
///
/// Provides `MysqlPool`, `MysqlManager`, and the `new_mysql_pool` helper.
#[cfg(feature = "pool-mysql")]
pub mod pool_mysql {
    pub use oxisql_pool::mysql::{new_mysql_pool, MysqlManager, MysqlPool};
}

/// Re-exports from the embedded connection pool (requires the `pool-embedded` feature).
///
/// Provides `EmbeddedPool`, an `Arc<Mutex<Glue<MemoryStorage>>>` wrapper.
#[cfg(feature = "pool-embedded")]
pub mod pool_embedded {
    pub use oxisql_pool::embedded::EmbeddedPool;
}

/// Re-exports from the migration runner (requires the `migrate` feature).
///
/// Provides the `MigrationRunner` and supporting types for applying
/// directory-based SQL migrations.
#[cfg(feature = "migrate")]
pub mod migrate {
    pub use oxisql_migrate::{
        runner::{MigrationRunner, MigrationState},
        scanner::{scan_migrations, MigrationFile},
        MigrateOptions, MigrationError,
    };
}

/// Pool-related re-exports (feature-gated).
///
/// Provides the unified `OxidbPool` enum, `PoolConfig`, `PoolConfigBuilder`,
/// `PoolError`, and `PoolMetrics`.  Individual pool types are re-exported
/// behind their respective feature gates.
#[cfg(any(
    feature = "pool-postgres",
    feature = "pool-mysql",
    feature = "pool-embedded"
))]
pub mod pool {
    #[cfg(feature = "pool-embedded")]
    pub use oxisql_pool::embedded::EmbeddedPool;
    #[cfg(feature = "pool-mysql")]
    pub use oxisql_pool::mysql::{new_mysql_pool, MysqlPool};
    #[cfg(feature = "pool-postgres")]
    pub use oxisql_pool::postgres::OxidbPgPool;
    pub use oxisql_pool::{OxidbPool, PoolConfig, PoolConfigBuilder, PoolError, PoolMetrics};
}

/// Connect to a database identified by `uri`.
///
/// # URI schemes
///
/// - `memory://` — in-memory database (requires the `embedded` feature).
/// - `postgres://...` / `postgresql://...` — PostgreSQL (requires the `postgres`
///   feature; connects with no TLS / plain-text; for TLS use
///   `postgres::PgConnection::connect` directly).
/// - `mysql://...` — MySQL (requires the `mysql` feature; connects with no TLS /
///   plain-text; for TLS use `mysql::MyConnection::connect` directly).
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, or a backend-specific error on connection failure.
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect(uri: &str) -> Result<Box<dyn Connection>, OxiSqlError> {
    #[cfg(feature = "embedded")]
    if uri.starts_with("memory://") {
        return oxisql_embedded::EmbeddedConnection::open_memory()
            .map(|c| Box::new(c) as Box<dyn Connection>);
    }

    #[cfg(feature = "postgres")]
    if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        let conn = oxisql_postgres::PgConnection::connect(uri, oxisql_postgres::TlsMode::Disabled)
            .await
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    #[cfg(feature = "mysql")]
    if uri.starts_with("mysql://") {
        let conn = oxisql_mysql::MyConnection::connect(uri, oxisql_mysql::TlsMode::Disabled)
            .await
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // Persistent embedded backend: redb
    #[cfg(feature = "redb")]
    if uri.starts_with("redb://") {
        let path = uri.strip_prefix("redb://").unwrap_or("");
        if path.is_empty() {
            return Err(OxiSqlError::UnsupportedUri(
                "redb:// requires a file path — e.g. redb:///path/to/my.db".into(),
            ));
        }
        let conn = oxisql_embedded::RedbEmbeddedConnection::open(path)
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // Persistent embedded backend: fjall
    #[cfg(feature = "fjall")]
    if uri.starts_with("fjall://") {
        let path = uri.strip_prefix("fjall://").unwrap_or("");
        if path.is_empty() {
            return Err(OxiSqlError::UnsupportedUri(
                "fjall:// requires a directory path — e.g. fjall:///path/to/my_db_dir".into(),
            ));
        }
        let conn = oxisql_embedded::FjallEmbeddedConnection::open(path)
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // Persistent embedded backend: sled
    #[cfg(feature = "sled")]
    if uri.starts_with("sled://") {
        let path = uri.strip_prefix("sled://").unwrap_or("");
        if path.is_empty() {
            return Err(OxiSqlError::UnsupportedUri(
                "sled:// requires a directory path — e.g. sled:///path/to/my_db_dir".into(),
            ));
        }
        let conn = oxisql_embedded::SledEmbeddedConnection::open(path)
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // Pure-Rust SQLite-compat backend via Limbo.
    // Accepts:
    //   sqlite://path/to/file.db  — file-backed SQLite database
    //   sqlite::memory:           — in-memory database (destroyed on drop)
    #[cfg(feature = "sqlite")]
    if uri.starts_with("sqlite://") || uri == "sqlite::memory:" {
        let path = if uri == "sqlite::memory:" {
            ":memory:".to_string()
        } else {
            uri.strip_prefix("sqlite://").unwrap_or("").to_string()
        };
        if path.is_empty() {
            return Err(OxiSqlError::UnsupportedUri(
                "sqlite:// requires a file path or use sqlite::memory: for in-memory".into(),
            ));
        }
        let conn = oxisql_sqlite_compat::SqliteConnection::open(&path)
            .await
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // DataFusion is an OLAP context, not a Connection.  Provide a clear error
    // pointing callers at the correct API entry-point.
    if uri.starts_with("datafusion://") {
        return Err(OxiSqlError::UnsupportedUri(
            "datafusion:// is not a Connection backend — use oxisql::connect_datafusion() instead"
                .to_string(),
        ));
    }

    // file:// — route to sled when the feature is enabled, otherwise error.
    #[cfg(feature = "sled")]
    if uri.starts_with("file://") || uri.starts_with("file:") {
        let path = uri
            .strip_prefix("file://")
            .or_else(|| uri.strip_prefix("file:"))
            .unwrap_or("");
        if path.is_empty() {
            return Err(OxiSqlError::UnsupportedUri(
                "file:// requires a path — e.g. file:///path/to/my_db_dir".into(),
            ));
        }
        let conn = oxisql_embedded::SledEmbeddedConnection::open(path)
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    #[cfg(not(feature = "sled"))]
    if uri.starts_with("file://") || uri.starts_with("file:") {
        return Err(OxiSqlError::UnsupportedUri(format!(
            "file:// URIs require the 'sled' feature to be enabled. \
             Use 'memory://' for in-memory storage or enable the sled feature. \
             URI was: {uri}"
        )));
    }

    Err(OxiSqlError::UnsupportedUri(uri.to_string()))
}

/// Connect to a database, creating it first if it does not already exist.
///
/// # Per-backend behaviour
///
/// | URI prefix | Behaviour |
/// |---|---|
/// | `memory://` | Identical to [`connect`]: always succeeds (no persistent storage to create). |
/// | `postgres://` / `postgresql://` | Delegates to [`connect`].  Full auto-create (connecting to the `postgres` maintenance database and issuing `CREATE DATABASE`) is planned for a future release. |
/// | `mysql://` | Delegates to [`connect`].  Full auto-create (connecting without a database and issuing `CREATE DATABASE IF NOT EXISTS`) is planned for a future release. |
/// | Unknown schemes | Returns the same error as [`connect`]. |
///
/// For the `memory://` scheme this function is the most useful: it always
/// produces a fresh in-memory database regardless of whether one previously
/// existed.  Callers that want to be forward-compatible with persistent
/// backends (once auto-create is implemented) should prefer this function
/// over [`connect`] wherever database-creation semantics are desired.
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, or a backend-specific error on connection failure.
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// use oxisql::Connection;
/// let conn = oxisql::connect_or_create("memory://").await?;
/// conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;
/// # Ok(())
/// # }
/// ```
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect_or_create(uri: &str) -> Result<Box<dyn Connection>, OxiSqlError> {
    // For all current backends, `connect` already handles creation semantics
    // as appropriate (embedded is always created fresh; network backends do
    // not yet implement DDL-level auto-create).  This function provides the
    // stable API surface for callers that want create-if-absent semantics,
    // and will be enhanced in future releases to support PostgreSQL and MySQL
    // database creation.
    connect(uri).await
}

/// Connect to a database via a connection pool.
///
/// Returns a [`ConnectionPool`] whose [`get`][ConnectionPool::get] method
/// checks out a connection from the pool.  When the returned
/// `Box<dyn Connection + Send>` is dropped, the connection is returned
/// to the pool automatically.
///
/// # URI schemes
///
/// Same as [`connect`]: `memory://`, `postgres://`, `mysql://`.
///
/// # Arguments
///
/// * `uri` — connection URI
/// * `max_size` — maximum number of connections in the pool (ignored for
///   `memory://` which is a single-instance in-memory engine)
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled for
/// the given scheme.  Returns [`OxiSqlError::ConnectionPool`] on pool
/// creation failure.
#[must_use = "the returned ConnectionPool should be used to acquire connections"]
pub async fn connect_pooled(
    uri: &str,
    max_size: usize,
) -> Result<Box<dyn ConnectionPool>, OxiSqlError> {
    #[cfg(feature = "pool-embedded")]
    if uri.starts_with("memory://") {
        use oxisql_pool::embedded::EmbeddedPool;
        return Ok(Box::new(EmbeddedPool::new()));
    }

    #[cfg(feature = "pool-postgres")]
    if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        let _ = max_size;
        let pg_pool = oxisql_pool::postgres::OxidbPgPool::try_from_url(uri)
            .map_err(|e| OxiSqlError::ConnectionPool(e.to_string()))?;
        return Ok(Box::new(pg_pool));
    }

    #[cfg(feature = "pool-mysql")]
    if uri.starts_with("mysql://") || uri.starts_with("mysql2://") {
        let normalised = if uri.starts_with("mysql2://") {
            uri.replacen("mysql2://", "mysql://", 1)
        } else {
            uri.to_string()
        };
        let mysql_pool = oxisql_pool::mysql::new_mysql_pool(&normalised, max_size)
            .map_err(|e| OxiSqlError::ConnectionPool(e.to_string()))?;
        return Ok(Box::new(mysql_pool));
    }

    // Suppress "unused variable" warnings when no pool feature is active.
    let _ = (uri, max_size);
    Err(OxiSqlError::NotConnected)
}

/// Create a typed connection pool for the given URI, returning an `OxidbPool`.
///
/// Unlike [`connect_pooled`] (which returns a trait object), this function
/// returns the concrete `oxisql_pool::OxidbPool` enum, giving access to
/// backend-specific APIs (e.g. `OxidbPgPool::get()` for direct `deadpool`
/// client handles).
///
/// # URI dispatch
///
/// | URI prefix | Feature | Pool variant |
/// |---|---|---|
/// | `memory://` | `pool-embedded` | `OxidbPool::Embedded` |
/// | `postgres://` / `postgresql://` | `pool-postgres` | `OxidbPool::Postgres` |
/// | `mysql://` | `pool-mysql` | `OxidbPool::Mysql` |
///
/// # Arguments
///
/// * `uri` — connection URI (same schemes as [`connect`])
/// * `max_size` — maximum number of connections (ignored for `memory://`)
///
/// # Errors
///
/// Returns [`OxiSqlError::UnsupportedUri`] if the URI scheme is not recognised
/// or the required backend feature is not compiled in.  Returns
/// [`OxiSqlError::ConnectionPool`] on pool construction failure.
#[cfg(any(
    feature = "pool-postgres",
    feature = "pool-mysql",
    feature = "pool-embedded"
))]
#[must_use = "the returned OxidbPool should be used to acquire connections"]
pub async fn connect_pool(
    uri: &str,
    max_size: usize,
) -> Result<oxisql_pool::OxidbPool, OxiSqlError> {
    #[cfg(feature = "pool-embedded")]
    if uri == "memory://" || uri.starts_with("memory://") {
        return Ok(oxisql_pool::OxidbPool::Embedded(
            oxisql_pool::embedded::EmbeddedPool::new(),
        ));
    }

    #[cfg(feature = "pool-postgres")]
    if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        let pg_pool = oxisql_pool::postgres::OxidbPgPool::try_from_url(uri)
            .map_err(|e| OxiSqlError::ConnectionPool(e.to_string()))?;
        return Ok(oxisql_pool::OxidbPool::Postgres(pg_pool));
    }

    #[cfg(feature = "pool-mysql")]
    if uri.starts_with("mysql://") {
        let pool = oxisql_pool::mysql::new_mysql_pool(uri, max_size)
            .map_err(|e| OxiSqlError::ConnectionPool(e.to_string()))?;
        return Ok(oxisql_pool::OxidbPool::Mysql(pool));
    }

    // Suppress unused-variable warnings when only a subset of pool features is active.
    let _ = (uri, max_size);
    Err(OxiSqlError::UnsupportedUri(uri.to_string()))
}

/// Connect to a database with explicit [`ConnectOptions`].
///
/// This is the same as [`connect`] but accepts additional options.
/// Currently `connect_timeout_ms` and `require_tls` options are **not yet
/// applied** — they require backend-specific support.  `pool_size` is
/// ignored (use [`connect_pooled`] for pooling).
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, or a backend-specific error on connection failure.
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect_with_options(
    uri: &str,
    _opts: ConnectOptions,
) -> Result<Box<dyn Connection>, OxiSqlError> {
    connect(uri).await
}

/// Connect to a database with an explicit TLS configuration.
///
/// For **PostgreSQL** (`postgres://` / `postgresql://`) the provided
/// `tls_config` is forwarded to the backend via `TlsMode::Rustls`.
/// For all other backends (including the embedded in-memory backend) TLS is
/// not applicable and the call is identical to [`connect`]; the `tls_config`
/// argument is silently ignored.
///
/// # Arguments
///
/// * `uri` — connection URI (same schemes as [`connect`]).
/// * `tls_config` — an optional pre-built `rustls::ClientConfig` wrapped in
///   an `Arc`.  When `None` the call behaves exactly like [`connect`].
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled for
/// the requested scheme, or a backend-specific error on connection failure.
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "embedded")]
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// // Embedded backend ignores TLS config.
/// let conn = oxisql::connect_with_tls("memory://", None).await?;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "embedded"))]
/// # fn main() {}
/// ```
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect_with_tls(
    uri: &str,
    _tls_config: Option<std::sync::Arc<rustls::ClientConfig>>,
) -> Result<Box<dyn Connection>, OxiSqlError> {
    #[cfg(feature = "postgres")]
    if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        if let Some(tls_cfg) = _tls_config {
            let conn = oxisql_postgres::PgConnection::connect(
                uri,
                oxisql_postgres::TlsMode::Rustls(tls_cfg),
            )
            .await
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
            return Ok(Box::new(conn) as Box<dyn Connection>);
        }
    }

    // For all other backends (or postgres with no TLS config) fall back to
    // the plain-text connect path.
    connect(uri).await
}

/// Check connectivity of an established connection.
///
/// Delegates to [`Connection::ping`].
///
/// # Errors
///
/// Returns the backend-specific error if the ping fails.
pub async fn ping(conn: &dyn Connection) -> Result<(), OxiSqlError> {
    conn.ping().await
}

/// Explicitly close/drop a connection.
///
/// This is equivalent to simply dropping `conn`; provided for code clarity.
pub fn close(_conn: Box<dyn Connection>) {
    // Drop conn here — the connection is closed by the Drop impl.
}

/// Return all tables visible to the connection.
///
/// Delegates to [`Connection::tables`].  Returns an empty `Vec` when the
/// backend does not support schema introspection (e.g. an unconnected stub).
///
/// # Example
///
/// ```rust,no_run
/// # #[cfg(feature = "embedded")]
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// let conn = oxisql::connect("memory://").await?;
/// let tables = oxisql::introspect(conn.as_ref()).await;
/// # Ok(())
/// # }
/// # #[cfg(not(feature = "embedded"))]
/// # fn main() {}
/// ```
pub async fn introspect(conn: &dyn Connection) -> Vec<TableInfo> {
    conn.tables().await.unwrap_or_default()
}

#[cfg(test)]
mod feature_flag_tests {
    /// Verify that the feature-gated code compiles for all enabled features.
    ///
    /// This test validates that the feature-gated code compiles when features
    /// are enabled.  If we reach the end, all enabled feature combinations
    /// compiled successfully.
    #[test]
    fn test_feature_flags_compile() {
        #[cfg(feature = "embedded")]
        {
            // Ensure connect and EmbeddedConnection are accessible
            let _ = crate::connect;
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::embedded;
        }
        #[cfg(feature = "postgres")]
        {
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::postgres;
        }
        #[cfg(feature = "mysql")]
        {
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::mysql;
        }
        #[cfg(feature = "datafusion")]
        {
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::datafusion_backend;
        }
        // If we reach here, all enabled feature combinations compiled
    }
}
