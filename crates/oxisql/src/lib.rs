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

use std::time::Duration;

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

    /// Metadata for the PostgreSQL backend, with `version` statically `None`.
    ///
    /// This is a **static, connectionless** constructor — like
    /// [`backend_info_for_uri`], it performs no I/O, so the server version
    /// cannot be known here. Once a `postgres` connection is established, use
    /// `BackendInfo::from_postgres_connection` (requires the `postgres`
    /// feature) instead to populate `version` from the live server.
    #[must_use]
    pub fn postgres() -> Self {
        Self {
            name: "postgres",
            version: None, // static dispatch — no connection to query yet
            features: vec!["tcp", "tls", "prepared-statements"],
        }
    }

    /// Metadata for the MySQL backend, with `version` statically `None`.
    ///
    /// This is a **static, connectionless** constructor — like
    /// [`backend_info_for_uri`], it performs no I/O, so the server version
    /// cannot be known here. Once a `mysql` connection is established, use
    /// `BackendInfo::from_mysql_connection` (requires the `mysql` feature)
    /// instead to populate `version` from the live server.
    #[must_use]
    pub fn mysql() -> Self {
        Self {
            name: "mysql",
            version: None, // static dispatch — no connection to query yet
            features: vec!["tcp", "tls", "prepared-statements"],
        }
    }

    /// Build [`BackendInfo`] for a **live** PostgreSQL connection, populating
    /// `version` from the server's handshake parameters instead of leaving it
    /// `None`.
    ///
    /// Unlike [`BackendInfo::postgres`], which is a static, connectionless
    /// dispatcher, this reads `PgConnection::server_version` — a value
    /// `PgConnection::connect` captures from the `server_version`
    /// `ParameterStatus` message during the connection handshake — so the
    /// returned `version` reflects the actual connected server.
    ///
    /// `version` is `None` only when `conn` was built via
    /// `PgConnection::from_client` (no handshake parameters were available to
    /// capture) or in the practically-impossible case that the server did not
    /// report `server_version` at all.
    ///
    /// Requires the `postgres` feature.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use oxisql::postgres::{PgConnection, TlsMode};
    ///
    /// let conn = PgConnection::connect("host=localhost user=postgres", TlsMode::Disabled).await?;
    /// let info = oxisql::BackendInfo::from_postgres_connection(&conn);
    /// println!("server version: {:?}", info.version);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "postgres")]
    #[must_use]
    pub fn from_postgres_connection(conn: &oxisql_postgres::PgConnection) -> Self {
        Self {
            name: "postgres",
            version: conn.server_version().map(str::to_string),
            features: vec!["tcp", "tls", "prepared-statements"],
        }
    }

    /// Build [`BackendInfo`] for a **live** MySQL connection, populating
    /// `version` from the server's reported version instead of leaving it
    /// `None`.
    ///
    /// Unlike [`BackendInfo::mysql`], which is a static, connectionless
    /// dispatcher, this calls `MyConnection::server_version`, which reports
    /// the actual connected server's version, formatted as
    /// `"{major}.{minor}.{patch}"`. Because `mysql_async` checks connections
    /// out of its pool per call rather than holding one persistently, this
    /// performs a pool round-trip. On failure (e.g. the pool cannot reach the
    /// server), `version` is left `None` rather than propagating the error,
    /// since `BackendInfo` is best-effort metadata, not a health check.
    ///
    /// Requires the `mysql` feature.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use oxisql::mysql::{MyConnection, TlsMode};
    ///
    /// let conn = MyConnection::connect("mysql://root:@localhost/mydb", TlsMode::Disabled).await?;
    /// let info = oxisql::BackendInfo::from_mysql_connection(&conn).await;
    /// println!("server version: {:?}", info.version);
    /// # Ok(())
    /// # }
    /// ```
    #[cfg(feature = "mysql")]
    #[must_use]
    pub async fn from_mysql_connection(conn: &oxisql_mysql::MyConnection) -> Self {
        Self {
            name: "mysql",
            version: conn.server_version().await.ok(),
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

    /// Metadata for the Pure-Rust SQLite-compat backend (OxiSQLite — C-free fork).
    ///
    /// The version reflects the statically-known OxiSQLite engine version.  For
    /// network backends (Postgres, MySQL), the *static* dispatchers
    /// ([`BackendInfo::postgres`], [`BackendInfo::mysql`]) leave `version:
    /// None` since the server version is not known until after the
    /// connection handshake; use `BackendInfo::from_postgres_connection` /
    /// `BackendInfo::from_mysql_connection` on an established connection to
    /// populate it instead.
    #[must_use]
    pub fn sqlite_compat() -> Self {
        Self {
            name: "sqlite",
            // `oxisqlite` is workspace-version-locked to this crate (both
            // `crates/oxisql/Cargo.toml` and `crates/oxisqlite/Cargo.toml` inherit
            // `version.workspace = true` from the same `[workspace.package]`), so
            // `CARGO_PKG_VERSION` here is guaranteed to equal the actual OxiSQLite
            // engine version shipped alongside this release (C-free fork of limbo
            // 0.0.22) — unlike a hand-typed literal, it can never go stale.
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            features: vec!["sqlite-compat", "pure-rust", "oxisqlite", "embedded"],
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
///
/// Can be constructed manually via the builder methods, or automatically
/// populated from a URI query string using [`ConnectOptions::from_uri`].
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// Connection timeout in milliseconds, applied by
    /// [`connect_with_options`] to network-backed drivers (PostgreSQL; see
    /// that function's doc comment for why MySQL is unaffected in
    /// practice). `None` falls back to [`DEFAULT_CONNECT_TIMEOUT`] rather
    /// than waiting indefinitely.
    pub connect_timeout_ms: Option<u64>,
    /// Maximum pool size (used by `connect_pooled`).
    pub pool_size: Option<usize>,
    /// TLS mode — true to require TLS, false to disable.
    pub require_tls: bool,
    /// Application name hint forwarded to the server where supported.
    pub application_name: Option<String>,
    /// SSL mode string (e.g. `"require"`, `"prefer"`, `"disable"`).
    pub sslmode: Option<String>,
    /// Unknown query-string keys collected verbatim.
    pub extra: std::collections::HashMap<String, String>,
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

    /// Set the application name hint.
    pub fn application_name(mut self, name: impl Into<String>) -> Self {
        self.application_name = Some(name.into());
        self
    }

    /// Parse the query string portion of `uri` (everything after `?`) and
    /// populate the recognised fields on `self`.  Unknown keys are inserted
    /// into [`ConnectOptions::extra`].
    ///
    /// Recognised keys:
    ///
    /// | Key               | Maps to                             |
    /// |-------------------|-------------------------------------|
    /// | `pool_max`        | `pool_size`                         |
    /// | `connect_timeout` | `connect_timeout_ms` (seconds → ms) |
    /// | `sslmode`         | `sslmode`                           |
    /// | `application_name`| `application_name`                  |
    ///
    /// # Example
    ///
    /// ```rust
    /// let opts = oxisql::ConnectOptions::new()
    ///     .with_uri_params("sqlite://path.db?pool_max=8&connect_timeout=5");
    /// assert_eq!(opts.pool_size, Some(8));
    /// assert_eq!(opts.connect_timeout_ms, Some(5_000));
    /// ```
    #[must_use]
    pub fn with_uri_params(mut self, uri: &str) -> Self {
        let query = match uri.split_once('?') {
            Some((_, q)) if !q.is_empty() => q,
            _ => return self,
        };
        for pair in query.split('&') {
            let (key, val) = match pair.split_once('=') {
                Some(kv) => kv,
                None => {
                    // bare key with no value — skip silently
                    continue;
                }
            };
            let key = key.trim();
            let val = val.trim();
            match key {
                "pool_max" => {
                    if let Ok(n) = val.parse::<usize>() {
                        self.pool_size = Some(n);
                    }
                }
                "connect_timeout" => {
                    // Value is in seconds; store as milliseconds
                    if let Ok(secs) = val.parse::<u64>() {
                        self.connect_timeout_ms = Some(secs.saturating_mul(1_000));
                    }
                }
                "sslmode" => {
                    self.sslmode = Some(val.to_string());
                    if matches!(val, "require" | "verify-ca" | "verify-full") {
                        self.require_tls = true;
                    }
                }
                "application_name" => {
                    self.application_name = Some(val.to_string());
                }
                _ => {
                    self.extra.insert(key.to_string(), val.to_string());
                }
            }
        }
        self
    }

    /// Convenience constructor: parse a URI and build `ConnectOptions` from
    /// any recognized query parameters.
    ///
    /// # Example
    ///
    /// ```rust
    /// let opts = oxisql::ConnectOptions::from_uri("memory://?pool_max=4");
    /// assert_eq!(opts.pool_size, Some(4));
    /// ```
    #[must_use]
    pub fn from_uri(uri: &str) -> Self {
        Self::new().with_uri_params(uri)
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
///
/// Logical-replication types are additionally re-exported when the
/// `postgres-replication` feature is enabled (which also implies `postgres`).
#[cfg(feature = "postgres")]
pub mod postgres {
    pub use oxisql_postgres::{PgConnection, PgError, PgTransaction, TlsMode};

    #[cfg(feature = "postgres-replication")]
    pub use oxisql_postgres::{
        pg_micros_to_unix_micros, unix_micros_to_pg_micros, CellValue, ColumnSpec, CreatedSlot,
        IdentifySystem, LogicalReplicationMessage, Lsn, PgReplicationConnection, RelationBody,
        ReplicaIdentity, ReplicationEvent, ReplicationStream, TupleColumn, TupleData,
    };
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

/// Re-exports from the SQL query-result / prepared-plan cache (requires the
/// `cache` feature).
///
/// Provides `SqlQueryCache` (query-result LRU + TTL cache), `SqlPlanCache`
/// (prepared-statement plan cache), `CachedQueryRunner` (a read-through
/// caching adapter over any executor closure), and `QueryCacheStats`.
#[cfg(feature = "cache")]
pub mod cache {
    pub use oxisql_cache::{CachedQueryRunner, QueryCacheStats, SqlPlanCache, SqlQueryCache};
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

/// Default upper bound on how long [`connect`] will wait for a
/// network-backed driver (PostgreSQL, MySQL) to establish a connection
/// before giving up.
///
/// Without a bound, a firewalled or "black-holed" host (one that silently
/// drops packets instead of refusing the connection) leaves the connect
/// call hanging for however long the OS TCP stack takes to give up on its
/// own — often 60-130+ seconds, and sometimes effectively unbounded. This
/// constant exists so [`connect`] fails cleanly well before that.
///
/// 10 seconds was chosen to:
/// - Sit at the low (fast-feedback) end of the commonly-recommended 10-30s
///   range for interactive/CLI database clients — [`connect`] is most
///   visibly used from `oxisql-repl`, where a human is waiting on the
///   result, so failing fast on an unreachable host matters more than it
///   would for a long-lived background service. It is still generous
///   relative to typical connection-establishment latency: even a slow or
///   congested *working* network rarely takes more than a couple of
///   seconds to complete a TCP handshake plus the PostgreSQL auth exchange,
///   so legitimate slow connections are not expected to be falsely flagged.
/// - Contrast deliberately with [`oxisql_pool::PoolConfig`]'s default
///   (`connect_timeout_ms: Some(30_000)`): pool connections are typically
///   established once, at service startup or idle-recovery time, where a
///   longer wait is far more tolerable than in this facade's synchronous,
///   one-shot `connect`.
///
/// Neither `tokio_postgres::Config` nor `mysql_async::Opts` apply a default
/// connect timeout of their own (both leave it unbounded unless the caller
/// opts in), so there was no existing driver default to match instead.
///
/// Only PostgreSQL is actually bounded by this value in practice — see the
/// `mysql://` branch in this module's `connect_inner` for why MySQL's
/// `connect` call performs no blocking network I/O to bound in the first
/// place. Embedded/local backends (`memory://`, `redb://`, `fjall://`,
/// `sled://`, `sqlite://`) are unaffected for the same reason.
///
/// Use [`connect_with_options`] with [`ConnectOptions::timeout_ms`] (or a
/// `?connect_timeout=<secs>` query parameter parsed by
/// [`ConnectOptions::from_uri`]) to override this default.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
/// # Connection timeout
///
/// Network-backed drivers are bounded by [`DEFAULT_CONNECT_TIMEOUT`] (10
/// seconds) so a firewalled or black-holed host fails cleanly instead of
/// hanging indefinitely. Use [`connect_with_options`] to override the
/// default.
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, [`OxiSqlError::Timeout`] when a network-backed
/// driver does not finish connecting within the timeout, or a
/// backend-specific error on connection failure.
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect(uri: &str) -> Result<Box<dyn Connection>, OxiSqlError> {
    connect_inner(uri, DEFAULT_CONNECT_TIMEOUT).await
}

/// Shared dispatch logic behind [`connect`] and [`connect_with_options`].
///
/// `timeout` bounds only the PostgreSQL driver, via
/// `oxisql_postgres::PgConnection::connect_with_timeout`. Embedded/local
/// backends perform no blocking network I/O and ignore it entirely.
///
/// MySQL is a deliberate exception, left unwrapped: `mysql_async::Pool::new`
/// (invoked by `oxisql_mysql::MyConnection::connect`) constructs the pool
/// object synchronously with no network I/O at all — connections are dialed
/// lazily, per checkout, on the first `execute`/`query` call — so there is
/// nothing for `timeout` to bound at this stage, and `mysql_async` exposes
/// no client-side connect-timeout mechanism (neither an `OptsBuilder`
/// method nor a recognised URL query parameter) to wire in even for
/// defensive symmetry with PostgreSQL.
async fn connect_inner(uri: &str, timeout: Duration) -> Result<Box<dyn Connection>, OxiSqlError> {
    #[cfg(feature = "embedded")]
    if uri.starts_with("memory://") {
        return oxisql_embedded::EmbeddedConnection::open_memory()
            .map(|c| Box::new(c) as Box<dyn Connection>);
    }

    #[cfg(feature = "postgres")]
    if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
        let conn = oxisql_postgres::PgConnection::connect_with_timeout(
            uri,
            oxisql_postgres::TlsMode::Disabled,
            timeout,
        )
        .await
        .map_err(OxiSqlError::from)?;
        return Ok(Box::new(conn) as Box<dyn Connection>);
    }

    // See this function's doc comment: `MyConnection::connect` only builds a
    // `mysql_async::Pool` (synchronous, no network I/O), so `timeout` has
    // nothing to bound here and is intentionally not applied.
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

    // Suppress an unused-variable warning when `postgres` — the only branch
    // that reads `timeout` — is not compiled in. `Duration` is `Copy`, so
    // this discard has no effect on the real use above when it is.
    let _ = timeout;

    Err(OxiSqlError::UnsupportedUri(uri.to_string()))
}

/// The wire-protocol family of a connection URI that supports server-side
/// database creation (`CREATE DATABASE`).
///
/// Embedded backends (`memory://`, `redb://`, `fjall://`, `sled://`,
/// `sqlite://`) create their storage on open and are not represented here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreateScheme {
    /// PostgreSQL (`postgres://` / `postgresql://`).
    Postgres,
    /// MySQL (`mysql://`).
    Mysql,
}

impl CreateScheme {
    /// Classify `uri` into a [`CreateScheme`], or `None` for embedded / unknown
    /// schemes that do not support `CREATE DATABASE`.
    fn from_uri(uri: &str) -> Option<Self> {
        if uri.starts_with("postgres://") || uri.starts_with("postgresql://") {
            Some(CreateScheme::Postgres)
        } else if uri.starts_with("mysql://") {
            Some(CreateScheme::Mysql)
        } else {
            None
        }
    }
}

/// Split a wire-protocol URI into its authority part (scheme + credentials +
/// host[:port], up to but excluding the leading `/` of the path) and the
/// target database name (the first path segment, percent-decoding left to the
/// backend).
///
/// The returned tuple is `(authority, db_name)` where `authority` retains the
/// scheme and any `?query`/`#fragment` is **not** part of `db_name`.  The query
/// string (if any) is preserved on the authority half so callers can rebuild a
/// maintenance URI without losing connection parameters.
///
/// # Errors
///
/// Returns [`OxiSqlError::UnsupportedUri`] when the URI has no `scheme://`
/// prefix, no path component, or an empty database name.
fn split_db_name(uri: &str) -> Result<(String, String), OxiSqlError> {
    let scheme_end = uri
        .find("://")
        .ok_or_else(|| OxiSqlError::UnsupportedUri(format!("URI is missing a scheme://: {uri}")))?;
    // Position just after "://".
    let after_scheme = scheme_end + 3;
    let rest = &uri[after_scheme..];

    // The authority ends at the first '/' (start of the path).  A '?' or '#'
    // before any '/' means there is no path → no database name.
    let path_slash = rest.find('/');
    let query_or_frag = rest.find(['?', '#']);

    let slash_idx = match (path_slash, query_or_frag) {
        (Some(s), Some(q)) if s < q => s,
        (Some(s), None) => s,
        _ => {
            return Err(OxiSqlError::UnsupportedUri(format!(
                "URI has no database path segment: {uri}"
            )));
        }
    };

    // authority = scheme + everything up to (not including) the path slash.
    let authority = &uri[..after_scheme + slash_idx];

    // The path begins right after the slash.  The database name is the first
    // path segment, terminated by the next '/', '?' or '#'.
    let path_and_after = &rest[slash_idx + 1..];
    let db_end = path_and_after
        .find(['/', '?', '#'])
        .unwrap_or(path_and_after.len());
    let db_name = &path_and_after[..db_end];

    if db_name.is_empty() {
        return Err(OxiSqlError::UnsupportedUri(format!(
            "URI has an empty database name: {uri}"
        )));
    }

    // Preserve any query string on the authority half so the maintenance URI
    // keeps connection parameters (sslmode, application_name, …).
    let query = match uri.find('?') {
        Some(q) => &uri[q..],
        None => "",
    };

    Ok((format!("{authority}{query}"), db_name.to_string()))
}

/// Build a *maintenance* connection URI from `uri` — one that connects to a
/// database that is guaranteed to exist so a `CREATE DATABASE` for the real
/// target can be issued.
///
/// - **PostgreSQL** connects to the built-in `postgres` maintenance database,
///   reusing the same authority and query string.
/// - **MySQL** connects with **no** database selected (no path), reusing the
///   same authority and query string.
///
/// # Errors
///
/// Propagates [`OxiSqlError::UnsupportedUri`] from [`split_db_name`].
fn maintenance_uri(uri: &str, scheme: CreateScheme) -> Result<String, OxiSqlError> {
    // `split_db_name` returns the authority with any query string already
    // appended (e.g. "postgres://user@host:5432?sslmode=require").
    let (authority_with_query, _db) = split_db_name(uri)?;

    match scheme {
        CreateScheme::Postgres => {
            // Insert the `/postgres` path *before* any query string.
            match authority_with_query.split_once('?') {
                Some((auth, query)) => Ok(format!("{auth}/postgres?{query}")),
                None => Ok(format!("{authority_with_query}/postgres")),
            }
        }
        // MySQL: no database path — the authority (with query) is exactly the
        // maintenance URI.
        CreateScheme::Mysql => Ok(authority_with_query),
    }
}

/// Quote a SQL identifier for the given [`CreateScheme`], escaping the quote
/// character per that dialect, and build a `CREATE DATABASE <quoted>` statement.
///
/// - **PostgreSQL** double-quotes the identifier, doubling embedded `"`.
/// - **MySQL** backtick-quotes the identifier, doubling embedded `` ` ``.
///
/// # Errors
///
/// Returns [`OxiSqlError::UnsupportedUri`] when `db_name` is empty or contains a
/// NUL byte (`\0`), which cannot be safely represented in a quoted identifier.
fn create_database_stmt(scheme: CreateScheme, db_name: &str) -> Result<String, OxiSqlError> {
    if db_name.is_empty() {
        return Err(OxiSqlError::UnsupportedUri(
            "cannot CREATE DATABASE with an empty name".to_string(),
        ));
    }
    if db_name.contains('\0') {
        return Err(OxiSqlError::UnsupportedUri(format!(
            "database name contains a NUL byte and cannot be safely quoted: {db_name:?}"
        )));
    }

    let quoted = match scheme {
        // Postgres: "name", escape embedded '"' as '""'.
        CreateScheme::Postgres => format!("\"{}\"", db_name.replace('"', "\"\"")),
        // MySQL: `name`, escape embedded '`' as '``'.
        CreateScheme::Mysql => format!("`{}`", db_name.replace('`', "``")),
    };

    Ok(format!("CREATE DATABASE {quoted}"))
}

/// Classify an [`OxiSqlError`] produced while connecting (or probing) a
/// wire-protocol backend as a "target database does not exist" error.
///
/// - **PostgreSQL** reports SQLSTATE `3D000` (`invalid_catalog_name` /
///   `undefined_database`).  The probe ([`pg_probe`]) embeds the SQLSTATE code
///   in the message when the server supplies one (the facade's normal mapping
///   discards it); the canonical-message substring `does not exist` is matched
///   as a fallback for builds that cannot read the structured code.
/// - **MySQL** reports server error code `1049` (`ER_BAD_DB_ERROR`,
///   "Unknown database").  The numeric code survives in the backend error's
///   `Display` (`ERROR 1049 (…): Unknown database …`), so it is matched
///   directly.
///
/// The predicate is pure and operates only on the rendered error string, which
/// makes it unit-testable against synthetic [`OxiSqlError`] values without a
/// live server.
fn is_database_missing_error(scheme: CreateScheme, err: &OxiSqlError) -> bool {
    let msg = err.to_string();
    match scheme {
        // SQLSTATE 3D000 is the only state for a missing catalog.  Match the
        // structured code first; fall back to the canonical server phrasing.
        CreateScheme::Postgres => msg.contains("3D000") || msg.contains("does not exist"),
        // MySQL error 1049 — match the numeric code, which `mysql_async`
        // renders verbatim in the server-error Display.
        CreateScheme::Mysql => msg.contains("1049"),
    }
}

/// Connect to a database, creating it first if it does not already exist.
///
/// # Per-backend behaviour
///
/// | URI prefix | Behaviour |
/// |---|---|
/// | `memory://` | Identical to [`connect`]: always succeeds (no persistent storage to create). |
/// | `postgres://` / `postgresql://` | Attempts [`connect`]; on SQLSTATE `3D000` (database does not exist) connects to the `postgres` maintenance database, issues `CREATE DATABASE "<name>"`, then reconnects to the original URI. |
/// | `mysql://` | Attempts [`connect`] and probes the connection; on error `1049` (unknown database) connects with no database selected, issues `` CREATE DATABASE `<name>` ``, then reconnects to the original URI. |
/// | `redb://`, `fjall://`, `sled://`, `sqlite://`, `file://` | Identical to [`connect`]: these embedded backends create their storage on open. |
/// | Unknown schemes | Returns the same error as [`connect`]. |
///
/// The database identifier is always safely quoted (PostgreSQL double-quotes,
/// MySQL backticks, with embedded quote characters doubled) and a name that
/// cannot be quoted (e.g. one containing a NUL byte) is rejected — the
/// `CREATE DATABASE` statement is never built by raw interpolation.
///
/// `CREATE DATABASE` is issued on a freshly-opened maintenance connection and
/// therefore runs in autocommit mode; it is **not** wrapped in a transaction
/// (PostgreSQL forbids `CREATE DATABASE` inside one).
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, [`OxiSqlError::UnsupportedUri`] when a `postgres://` /
/// `mysql://` URI has no database name (so there is nothing to create), or a
/// backend-specific error on connection / creation failure.
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
    // Embedded backends (memory/redb/fjall/sled/sqlite) and unknown schemes
    // create-on-open or have no create semantics — delegate unchanged.
    let scheme = match CreateScheme::from_uri(uri) {
        Some(s) => s,
        None => return connect(uri).await,
    };

    // Probe whether the target database already exists.  For PostgreSQL the
    // missing-database error surfaces eagerly during `connect`; for MySQL the
    // pool connects lazily, so an extra round-trip is needed to force the
    // server handshake (where the unknown-database error is raised).
    let probe = match scheme {
        CreateScheme::Postgres => pg_probe(uri).await,
        CreateScheme::Mysql => mysql_probe(uri).await,
    };

    match probe {
        // Database exists (or some other, non-fatal-to-detection state): hand
        // back a normal facade connection.
        Ok(()) => connect(uri).await,
        Err(err) if is_database_missing_error(scheme, &err) => {
            // Create the database via a maintenance connection, then reconnect.
            let (_authority, db_name) = split_db_name(uri)?;
            let stmt = create_database_stmt(scheme, &db_name)?;
            let maint = maintenance_uri(uri, scheme)?;

            // Fresh connection ⇒ autocommit; CREATE DATABASE is a simple
            // statement and is NOT wrapped in a transaction.
            let maint_conn = connect(&maint).await?;
            maint_conn.execute(&stmt, &[]).await?;
            drop(maint_conn);

            connect(uri).await
        }
        // Any other error (auth failure, host unreachable, …) is returned as-is.
        Err(err) => Err(err),
    }
}

/// Render a backend error together with its full `source()` chain into a
/// single string, so discriminating detail carried by a *source* error (e.g.
/// PostgreSQL's `DbError` message, MySQL's numeric server code) survives even
/// when the top-level `Display` is opaque (e.g. tokio-postgres renders database
/// errors as just `"db error"`).
///
/// This avoids a direct dependency on the backend driver crates
/// (`tokio_postgres` / `mysql_async`) — it relies only on the standard
/// [`std::error::Error`] source chain.
#[cfg(any(feature = "postgres", feature = "mysql"))]
fn render_error_chain(err: &(dyn std::error::Error + 'static)) -> String {
    let mut parts = vec![err.to_string()];
    let mut source = err.source();
    // Bound the walk defensively against any pathological cyclic chain.
    let mut guard = 0u8;
    while let Some(inner) = source {
        parts.push(inner.to_string());
        source = inner.source();
        guard = guard.saturating_add(1);
        if guard >= 16 {
            break;
        }
    }
    parts.join(": ")
}

/// Attempt to open `uri` as PostgreSQL and confirm the target database exists.
///
/// Returns `Ok(())` when the connection (which selects the database during the
/// startup handshake) succeeds, or the connection error — rendered with its
/// full source chain so the canonical `database "…" does not exist` message is
/// preserved — on failure.  This bypasses the facade's lossy error mapping so
/// [`is_database_missing_error`] can classify a missing database.
#[cfg(feature = "postgres")]
async fn pg_probe(uri: &str) -> Result<(), OxiSqlError> {
    match oxisql_postgres::PgConnection::connect_with_timeout(
        uri,
        oxisql_postgres::TlsMode::Disabled,
        DEFAULT_CONNECT_TIMEOUT,
    )
    .await
    {
        Ok(_conn) => Ok(()),
        Err(e) => Err(OxiSqlError::Other(format!(
            "postgres connect error: {}",
            render_error_chain(&e)
        ))),
    }
}

/// PostgreSQL backend not compiled in — fall back to the facade `connect`
/// (which yields a clear "unsupported scheme" error).
#[cfg(not(feature = "postgres"))]
async fn pg_probe(uri: &str) -> Result<(), OxiSqlError> {
    connect(uri).await.map(|_| ())
}

/// Attempt to open `uri` as MySQL and confirm the target database exists.
///
/// MySQL's pool connects lazily, so a trivial `SELECT 1` probe is issued to
/// force the server handshake where an unknown-database error (code `1049`)
/// would be raised.  Returns `Ok(())` on success, or the error — rendered with
/// its full source chain so the numeric `ERROR 1049` code is preserved — on
/// failure.
#[cfg(feature = "mysql")]
async fn mysql_probe(uri: &str) -> Result<(), OxiSqlError> {
    let conn = match oxisql_mysql::MyConnection::connect(uri, oxisql_mysql::TlsMode::Disabled).await
    {
        Ok(c) => c,
        Err(e) => {
            return Err(OxiSqlError::Other(format!(
                "mysql connect error: {}",
                render_error_chain(&e)
            )))
        }
    };
    // Force the lazy pool to actually connect (and select the database).
    match conn.query_binary("SELECT 1", &[]).await {
        Ok(_rows) => Ok(()),
        Err(e) => Err(OxiSqlError::Other(format!(
            "mysql probe error: {}",
            render_error_chain(&e)
        ))),
    }
}

/// MySQL backend not compiled in — fall back to the facade `connect`.
#[cfg(not(feature = "mysql"))]
async fn mysql_probe(uri: &str) -> Result<(), OxiSqlError> {
    connect(uri).await.map(|_| ())
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
/// This is the same as [`connect`], except `opts.connect_timeout_ms` — when
/// set — overrides [`DEFAULT_CONNECT_TIMEOUT`] for the PostgreSQL driver
/// (MySQL's `connect` performs no blocking network I/O regardless; see
/// [`connect`]'s doc comment). `require_tls` is **not yet applied** — it
/// requires backend-specific support (use [`connect_with_tls`] for an
/// explicit TLS config in the meantime). `pool_size` is ignored (use
/// [`connect_pooled`] for pooling).
///
/// # Errors
///
/// Returns [`OxiSqlError::NotConnected`] when no backend is compiled in for
/// the requested scheme, [`OxiSqlError::Timeout`] when the PostgreSQL
/// driver does not finish connecting within the (possibly overridden)
/// timeout, or a backend-specific error on connection failure.
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// // Override the default 10-second timeout with a short one for a
/// // fast-fail health check against a possibly-unreachable host.
/// let opts = oxisql::ConnectOptions::new().timeout_ms(2_000);
/// let _conn = oxisql::connect_with_options("postgres://localhost/mydb", opts).await?;
/// # Ok(())
/// # }
/// ```
#[must_use = "the returned Connection should be used for database operations"]
pub async fn connect_with_options(
    uri: &str,
    opts: ConnectOptions,
) -> Result<Box<dyn Connection>, OxiSqlError> {
    let timeout = opts
        .connect_timeout_ms
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_CONNECT_TIMEOUT);
    connect_inner(uri, timeout).await
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
            let conn = oxisql_postgres::PgConnection::connect_with_timeout(
                uri,
                oxisql_postgres::TlsMode::Rustls(tls_cfg),
                DEFAULT_CONNECT_TIMEOUT,
            )
            .await
            .map_err(OxiSqlError::from)?;
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

// ── Clean error display ─────────────────────────────────────────────────────
//
// Implementation lives in `error_display.rs` (kept out of this file to stay
// under the workspace's 2000-line guideline); `display_error` is
// re-exported here so `oxisql::display_error` keeps working unchanged.

mod error_display;
pub use error_display::display_error;

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
            // `from_postgres_connection` takes a live `&PgConnection` — no
            // connection is created here, this only proves the signature
            // type-checks (`fn(&PgConnection) -> BackendInfo`).
            let _: fn(&oxisql_postgres::PgConnection) -> crate::BackendInfo =
                crate::BackendInfo::from_postgres_connection;
        }
        #[cfg(feature = "mysql")]
        {
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::mysql;
            // `from_mysql_connection` is `async fn(&MyConnection) -> BackendInfo`.
            // Its opaque `impl Future` return type captures the input
            // reference's lifetime, which makes a plain `fn`-pointer
            // coercion fail to type-check (a known limitation with async fns
            // taking borrowed parameters) — so, mirroring
            // `connect_with_timeout_signature_compiles` in
            // `oxisql-postgres/tests/connect.rs`, a thin wrapper function
            // whose body calls it is used instead to prove the signature
            // type-checks, without ever running it.
            #[allow(dead_code)]
            async fn _check_from_mysql_connection(
                conn: &oxisql_mysql::MyConnection,
            ) -> crate::BackendInfo {
                crate::BackendInfo::from_mysql_connection(conn).await
            }
            let _ = std::mem::size_of_val(&_check_from_mysql_connection);
        }
        #[cfg(feature = "datafusion")]
        {
            let _: fn() -> crate::BackendInfo = crate::BackendInfo::datafusion_backend;
        }
        // If we reach here, all enabled feature combinations compiled
    }
}

#[cfg(test)]
mod auto_create_helpers_tests {
    use super::{
        create_database_stmt, is_database_missing_error, maintenance_uri, split_db_name,
        CreateScheme, OxiSqlError,
    };

    // ── split_db_name ──────────────────────────────────────────────────────────

    #[test]
    fn split_db_name_basic() {
        let (authority, db) =
            split_db_name("postgres://user:pw@localhost:5432/mydb").expect("splits");
        assert_eq!(authority, "postgres://user:pw@localhost:5432");
        assert_eq!(db, "mydb");
    }

    #[test]
    fn split_db_name_with_params() {
        // The query string must be preserved on the authority half so the
        // maintenance URI keeps connection parameters.
        let (authority, db) =
            split_db_name("postgres://user@host:5432/appdb?sslmode=require&application_name=x")
                .expect("splits");
        assert_eq!(
            authority,
            "postgres://user@host:5432?sslmode=require&application_name=x"
        );
        assert_eq!(db, "appdb");
    }

    #[test]
    fn split_db_name_mysql() {
        let (authority, db) = split_db_name("mysql://root@127.0.0.1:3306/shop").expect("splits");
        assert_eq!(authority, "mysql://root@127.0.0.1:3306");
        assert_eq!(db, "shop");
    }

    #[test]
    fn split_db_name_rejects_no_scheme() {
        assert!(matches!(
            split_db_name("localhost/db"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
    }

    #[test]
    fn split_db_name_rejects_no_path() {
        // Authority only, no '/' before the (optional) query → no database.
        assert!(matches!(
            split_db_name("postgres://localhost"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
        assert!(matches!(
            split_db_name("postgres://localhost?sslmode=require"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
    }

    #[test]
    fn split_db_name_rejects_empty_db() {
        assert!(matches!(
            split_db_name("postgres://localhost/"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
        assert!(matches!(
            split_db_name("postgres://localhost/?x=1"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
    }

    // ── maintenance_uri ──────────────────────────────────────────────────────────

    #[test]
    fn pg_maintenance_uri() {
        let m = maintenance_uri(
            "postgres://user:pw@localhost:5432/mydb",
            CreateScheme::Postgres,
        )
        .expect("maintenance uri");
        assert_eq!(m, "postgres://user:pw@localhost:5432/postgres");
    }

    #[test]
    fn pg_maintenance_uri_preserves_query() {
        let m = maintenance_uri(
            "postgresql://u@h:5432/appdb?sslmode=require&connect_timeout=5",
            CreateScheme::Postgres,
        )
        .expect("maintenance uri");
        assert_eq!(
            m,
            "postgresql://u@h:5432/postgres?sslmode=require&connect_timeout=5"
        );
    }

    #[test]
    fn mysql_maintenance_uri() {
        let m = maintenance_uri("mysql://root@127.0.0.1:3306/shop", CreateScheme::Mysql)
            .expect("maintenance uri");
        // No database path on the maintenance URI for MySQL.
        assert_eq!(m, "mysql://root@127.0.0.1:3306");
    }

    #[test]
    fn mysql_maintenance_uri_preserves_query() {
        let m = maintenance_uri(
            "mysql://root@127.0.0.1:3306/shop?pool_max=4",
            CreateScheme::Mysql,
        )
        .expect("maintenance uri");
        assert_eq!(m, "mysql://root@127.0.0.1:3306?pool_max=4");
    }

    // ── create_database_stmt ──────────────────────────────────────────────────────

    #[test]
    fn create_database_stmt_quotes_identifier() {
        // PostgreSQL → double quotes.
        assert_eq!(
            create_database_stmt(CreateScheme::Postgres, "mydb").expect("stmt"),
            r#"CREATE DATABASE "mydb""#
        );
        // MySQL → backticks.
        assert_eq!(
            create_database_stmt(CreateScheme::Mysql, "mydb").expect("stmt"),
            "CREATE DATABASE `mydb`"
        );
    }

    #[test]
    fn create_database_stmt_escapes_embedded_quote() {
        // PostgreSQL doubles an embedded double-quote.
        assert_eq!(
            create_database_stmt(CreateScheme::Postgres, r#"we"ird"#).expect("stmt"),
            r#"CREATE DATABASE "we""ird""#
        );
        // MySQL doubles an embedded backtick.
        assert_eq!(
            create_database_stmt(CreateScheme::Mysql, "we`ird").expect("stmt"),
            "CREATE DATABASE `we``ird`"
        );
    }

    #[test]
    fn create_database_stmt_rejects_bad_identifier() {
        // A NUL byte cannot be safely represented in a quoted identifier.
        assert!(matches!(
            create_database_stmt(CreateScheme::Postgres, "bad\0name"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
        assert!(matches!(
            create_database_stmt(CreateScheme::Mysql, "bad\0name"),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
        // An empty name is rejected too.
        assert!(matches!(
            create_database_stmt(CreateScheme::Postgres, ""),
            Err(OxiSqlError::UnsupportedUri(_))
        ));
    }

    // ── is_database_missing_error ──────────────────────────────────────────────────

    #[test]
    fn classifier_matches_pg_missing_database() {
        // Real-world rendering embeds SQLSTATE 3D000 and/or the canonical phrase.
        let sqlstate_err = OxiSqlError::Other(
            "postgres connect error: db error: FATAL: database \"mydb\" does not exist [SQLSTATE 3D000]"
                .to_string(),
        );
        assert!(is_database_missing_error(
            CreateScheme::Postgres,
            &sqlstate_err
        ));

        let phrase_err = OxiSqlError::Other(
            "postgres connect error: db error: FATAL: database \"mydb\" does not exist".to_string(),
        );
        assert!(is_database_missing_error(
            CreateScheme::Postgres,
            &phrase_err
        ));
    }

    #[test]
    fn classifier_rejects_unrelated_pg_error() {
        // Authentication failure (SQLSTATE 28P01) must NOT be treated as missing-db.
        let auth_err = OxiSqlError::Other(
            "postgres connect error: db error: FATAL: password authentication failed [SQLSTATE 28P01]"
                .to_string(),
        );
        assert!(!is_database_missing_error(
            CreateScheme::Postgres,
            &auth_err
        ));

        // A connection-refused error is unrelated.
        let conn_err = OxiSqlError::Other(
            "postgres connect error: error connecting to server: Connection refused".to_string(),
        );
        assert!(!is_database_missing_error(
            CreateScheme::Postgres,
            &conn_err
        ));
    }

    #[test]
    fn classifier_matches_mysql_missing_database() {
        // mysql_async renders the numeric code in the server-error Display.
        let err = OxiSqlError::Other(
            "mysql probe error: mysql connection error: Server error: `ERROR 1049 (42000): \
             Unknown database 'shop''"
                .to_string(),
        );
        assert!(is_database_missing_error(CreateScheme::Mysql, &err));
    }

    #[test]
    fn classifier_rejects_unrelated_mysql_error() {
        // Access denied (error 1045) must NOT be treated as missing-db.
        let access_err = OxiSqlError::Other(
            "mysql connect error: mysql connection error: Server error: `ERROR 1045 (28000): \
             Access denied for user 'root'@'localhost''"
                .to_string(),
        );
        assert!(!is_database_missing_error(CreateScheme::Mysql, &access_err));

        // An I/O / connection-refused error is unrelated.
        let io_err = OxiSqlError::Other(
            "mysql connect error: mysql connection error: Input/output error: Connection refused"
                .to_string(),
        );
        assert!(!is_database_missing_error(CreateScheme::Mysql, &io_err));
    }

    #[test]
    fn create_scheme_classifies_uris() {
        assert_eq!(
            CreateScheme::from_uri("postgres://h/db"),
            Some(CreateScheme::Postgres)
        );
        assert_eq!(
            CreateScheme::from_uri("postgresql://h/db"),
            Some(CreateScheme::Postgres)
        );
        assert_eq!(
            CreateScheme::from_uri("mysql://h/db"),
            Some(CreateScheme::Mysql)
        );
        assert_eq!(CreateScheme::from_uri("memory://"), None);
        assert_eq!(CreateScheme::from_uri("sqlite://x.db"), None);
        assert_eq!(CreateScheme::from_uri("redb:///x.db"), None);
    }
}
