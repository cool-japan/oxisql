//! MySQL connection implementing [`oxisql_core::Connection`].
//!
//! # Pool-based concurrency
//!
//! `mysql_async::Pool` is already internally synchronized and `Send + Sync`.
//! Each `execute` / `query` call acquires a connection from the pool, uses it,
//! and returns it automatically when it is dropped.  There is no need for an
//! `Arc<Mutex<Client>>` wrapper.
//!
//! # TLS
//!
//! `mysql_async` 0.36 builds its own `rustls::ClientConfig` internally via
//! `SslOpts::build_tls_connector`, which calls `ClientConfig::builder()`.
//! That call requires a process-global `CryptoProvider` to be installed.
//! When [`TlsMode::Rustls`] is requested we install the `rustls-rustcrypto`
//! provider guarded by `CryptoProvider::get_default().is_none()` so that
//! multiple calls in the same test binary do not panic.
//!
//! The `Arc<rustls::ClientConfig>` carried in `TlsMode::Rustls` is used only to
//! extract the `CryptoProvider` for the guarded install; `mysql_async` builds
//! its own config from `SslOpts`.  This API symmetry matches `PgConnection` and
//! is intended to support a direct-config path if a future mysql_async release
//! exposes one.
//!
//! # Transaction semantics
//!
//! `MyTransaction` holds an owned `mysql_async::Transaction<'static>` obtained
//! from `Pool::start_transaction`.  Callers **must** call
//! [`Transaction::commit`] or [`Transaction::rollback`] explicitly.  Dropping
//! without an explicit terminal action rolls back implicitly (mysql_async's
//! internal behaviour).
//!
//! # Warning surfacing
//!
//! MySQL reports a warning count in every `OkPacket`.  After each `execute` or
//! `query` call, if the count is `> 0`, a follow-up `SHOW WARNINGS` query is
//! issued on the same connection to retrieve the full list.  The results are
//! stored in `last_warnings` (via an `Arc<Mutex<...>>` for interior mutability
//! across `Clone` instances sharing the same pool) and accessible via
//! [`Connection::last_warnings`].  When the server reports `0` warnings the
//! extra round-trip is skipped entirely.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use mysql_async::prelude::Queryable;
use mysql_async::{Opts, OptsBuilder, Pool, PoolConstraints, PoolOpts, SslOpts, TxOpts};

use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    SqlWarning, TableInfo, TableType, ToSqlValue, Transaction, Value,
};

use crate::error::{classify_mysql_error, MysqlError};
use crate::prepared::MySqlPrepared;
use crate::types::{mysql_row_to_core, mysql_value_to_core};

// ── warning helpers ───────────────────────────────────────────────────────────

/// Fetch the current warning list from the server via `SHOW WARNINGS`.
///
/// Called only when the preceding statement's `OkPacket` reported
/// `warnings_count > 0` — no extra round-trip on the common no-warning path.
///
/// Each row in the `SHOW WARNINGS` response has three columns:
/// `Level VARCHAR`, `Code SMALLINT UNSIGNED`, `Message TEXT`.
async fn fetch_show_warnings(conn: &mut mysql_async::Conn) -> Result<Vec<SqlWarning>, MysqlError> {
    use oxisql_core::parse_warning_level;

    let rows: Vec<mysql_async::Row> = conn
        .query("SHOW WARNINGS")
        .await
        .map_err(MysqlError::Query)?;

    let mut warnings = Vec::with_capacity(rows.len());
    for mut row in rows {
        // Level column (VARCHAR)
        let level_str: Option<mysql_async::Value> = row.take(0);
        let level = match level_str {
            Some(mysql_async::Value::Bytes(b)) => {
                let s = String::from_utf8(b).unwrap_or_default();
                parse_warning_level(&s)
            }
            _ => oxisql_core::SqlWarningLevel::Warning,
        };

        // Code column (SMALLINT UNSIGNED → UInt or Int in mysql_async)
        let code_val: Option<mysql_async::Value> = row.take(1);
        let code: u16 = match code_val {
            Some(mysql_async::Value::UInt(n)) => u16::try_from(n).unwrap_or(u16::MAX),
            Some(mysql_async::Value::Int(n)) => u16::try_from(n.max(0)).unwrap_or(u16::MAX),
            _ => 0,
        };

        // Message column (TEXT)
        let msg_val: Option<mysql_async::Value> = row.take(2);
        let message = match msg_val {
            Some(mysql_async::Value::Bytes(b)) => String::from_utf8(b).unwrap_or_default(),
            _ => String::new(),
        };

        warnings.push(SqlWarning {
            code,
            level,
            message,
        });
    }
    Ok(warnings)
}

// ── TLS mode ──────────────────────────────────────────────────────────────────

/// TLS mode to use when connecting to MySQL.
#[derive(Clone, Debug, Default)]
pub enum TlsMode {
    /// Plain-text connection — no encryption.
    #[default]
    Disabled,
    /// TLS via Pure-Rust rustls with the RustCrypto provider.
    ///
    /// The `Arc<rustls::ClientConfig>` is used only to install the
    /// `CryptoProvider` into the process default (guarded).  `mysql_async` 0.36
    /// builds its own `ClientConfig` from `SslOpts` and does not accept a
    /// pre-built config directly.
    ///
    /// Note: to construct a suitable `ClientConfig` use
    /// `oxitls_adapter_rustls_rustcrypto::client_config(root_store)` or
    /// `rustls_rustcrypto::provider()` directly.
    #[allow(dead_code)]
    Rustls(Arc<rustls::ClientConfig>),
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Install the rustls-rustcrypto provider as the process default, guarded so
/// a second call in the same process does not panic.
fn ensure_crypto_provider() {
    use rustls::crypto::CryptoProvider;
    if CryptoProvider::get_default().is_none() {
        // install_default returns Err if another thread won the race; either
        // way the default is set after this point.
        let _ = rustls_rustcrypto::provider().install_default();
    }
}

/// Build a `Pool` from a URL string, an optional `SslOpts`, and optional
/// `PoolOpts` for min/max connections and idle/absolute TTL settings.
fn build_pool(
    url: &str,
    ssl: Option<SslOpts>,
    pool_opts: Option<PoolOpts>,
) -> Result<Pool, MysqlError> {
    let base_opts =
        Opts::from_url(url).map_err(|e| MysqlError::Connection(mysql_async::Error::from(e)))?;

    let mut builder = OptsBuilder::from_opts(base_opts);
    if let Some(ssl_opts) = ssl {
        builder = builder.ssl_opts(ssl_opts);
    }
    if let Some(p) = pool_opts {
        builder = builder.pool_opts(p);
    }
    let opts: Opts = builder.into();
    Ok(Pool::new(opts))
}

/// Convert `oxisql_core` positional params to `mysql_async::Params`.
pub fn core_params_to_mysql(params: &[&dyn ToSqlValue]) -> mysql_async::Params {
    if params.is_empty() {
        mysql_async::Params::Empty
    } else {
        let vals: Vec<mysql_async::Value> = params
            .iter()
            .map(|p| core_value_to_mysql(&p.to_value()))
            .collect();
        mysql_async::Params::Positional(vals)
    }
}

/// Convert a single `oxisql_core::Value` to a `mysql_async::Value`.
///
/// Extended types (Timestamp, Date, Time, Uuid, Json, Decimal, Array) are
/// converted to their string (Bytes) representation.  The MySQL server
/// handles implicit casts for typed columns.
pub fn core_value_to_mysql(v: &Value) -> mysql_async::Value {
    match v {
        Value::Null => mysql_async::Value::NULL,
        Value::Bool(b) => mysql_async::Value::Int(i64::from(*b)),
        Value::I64(n) => mysql_async::Value::Int(*n),
        Value::F64(f) => mysql_async::Value::Double(*f),
        Value::Text(s) => mysql_async::Value::Bytes(s.as_bytes().to_vec()),
        Value::Blob(b) => mysql_async::Value::Bytes(b.clone()),
        // Extended types: send as text bytes
        Value::Timestamp(us) => {
            let display = format!("{}", Value::Timestamp(*us));
            mysql_async::Value::Bytes(display.into_bytes())
        }
        Value::Date(days) => mysql_async::Value::Bytes(format!("{days}").into_bytes()),
        Value::Time(us) => {
            let display = format!("{}", Value::Time(*us));
            mysql_async::Value::Bytes(display.into_bytes())
        }
        Value::Uuid(u) => {
            let display = format!("{}", Value::Uuid(*u));
            mysql_async::Value::Bytes(display.into_bytes())
        }
        Value::Json(s) => mysql_async::Value::Bytes(s.as_bytes().to_vec()),
        Value::Decimal(s) => mysql_async::Value::Bytes(s.as_bytes().to_vec()),
        Value::Array(vals) => {
            // MySQL doesn't have native arrays; send as JSON array
            let items: Vec<String> = vals.iter().map(|v| format!("{v}")).collect();
            let json = format!("[{}]", items.join(","));
            mysql_async::Value::Bytes(json.into_bytes())
        }
        Value::TypedArray { values, .. } => {
            // MySQL doesn't have native typed arrays; send as JSON array
            let items: Vec<String> = values.iter().map(|v| format!("{v}")).collect();
            let json = format!("[{}]", items.join(","));
            mysql_async::Value::Bytes(json.into_bytes())
        }
    }
}

// ── MyConnection ──────────────────────────────────────────────────────────────

/// An asynchronous MySQL connection backed by a `mysql_async::Pool` (Pure Rust,
/// no `libmysqlclient` or `mysqlclient-sys`).
///
/// The `Pool` is internally synchronized; callers can share a `MyConnection`
/// across async tasks without additional locking.
///
/// `Clone` is cheap: both `mysql_async::Pool` and the `Arc`-wrapped warnings
/// store are internally reference-counted.  Clones share the same warning store,
/// so `last_warnings()` always reflects the most recent call made on *any*
/// clone.
#[derive(Clone)]
pub struct MyConnection {
    pool: Pool,
    /// Warnings from the most recently completed `execute` or `query` call.
    ///
    /// Wrapped in `Arc<Mutex<…>>` so that `Clone` instances share one store
    /// and interior mutability is safe across the `&self` execute/query API.
    last_warnings: Arc<Mutex<Vec<SqlWarning>>>,
}

impl MyConnection {
    /// Connect to a MySQL server identified by `url`.
    ///
    /// `url` follows the `mysql_async` format:
    /// `"mysql://user:password@host:port/database"`.
    ///
    /// # TLS
    ///
    /// - [`TlsMode::Disabled`] — plain-text connection.
    /// - [`TlsMode::Rustls`] — TLS via the process-default `CryptoProvider`
    ///   (installed lazily from `rustls-rustcrypto` on first use).
    pub async fn connect(url: &str, tls: TlsMode) -> Result<Self, MysqlError> {
        let ssl_opts = match tls {
            TlsMode::Disabled => None,
            TlsMode::Rustls(_cfg) => {
                // mysql_async builds its own ClientConfig from SslOpts using
                // ClientConfig::builder() which requires a process-global
                // CryptoProvider.  Install it guarded so repeated calls are safe.
                ensure_crypto_provider();
                Some(SslOpts::default())
            }
        };

        let pool = build_pool(url, ssl_opts, None)?;
        Ok(Self {
            pool,
            last_warnings: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Construct a `MyConnection` from a pre-existing `mysql_async::Pool`.
    ///
    /// This is useful when you need to share or reuse a pool that was created
    /// externally (e.g. from `mysql_async::Pool::new` with custom `Opts`).
    ///
    /// `Clone` is cheap: `mysql_async::Pool` is internally reference-counted.
    pub fn from_pool(pool: Pool) -> Self {
        Self {
            pool,
            last_warnings: Arc::new(Mutex::new(Vec::new())),
        }
    }

    // ── query analysis helpers ────────────────────────────────────────────────

    /// Return `true` if the SQL is a read-only query (SELECT, EXPLAIN, SHOW, …).
    ///
    /// Uses `oxisql_parse::parse_one` to parse the statement and
    /// `oxisql_parse::is_read_only` for AST-level classification.  This can
    /// be used to route read queries to a replica and write queries to a primary.
    ///
    /// Returns `false` when the SQL cannot be parsed or when the statement
    /// modifies data or schema.
    pub fn is_read_only_query(sql: &str) -> bool {
        match oxisql_parse::parse_one(sql) {
            Ok(stmt) => oxisql_parse::is_read_only(&stmt),
            Err(_) => false,
        }
    }

    /// Return the normalized canonical form of a SQL query for use as a cache key.
    ///
    /// Uses `oxisql_parse::normalize` to collapse whitespace, strip comments,
    /// and upper-case keywords.  The result is stable across cosmetically
    /// different but semantically equivalent SQL strings, making it suitable
    /// as a prepared-statement cache key.
    pub fn normalize_query(sql: &str) -> String {
        oxisql_parse::normalize(sql)
    }
}

impl Drop for MyConnection {
    fn drop(&mut self) {
        // Pool::disconnect() is async; we cannot await here.  Dropping the Pool
        // signals all in-flight connections to close when they are returned.
        // For graceful shutdown callers should use Pool::disconnect() explicitly.
    }
}

// ── Connection impl ───────────────────────────────────────────────────────────

#[async_trait]
impl Connection for MyConnection {
    /// Execute a DML/DDL statement and return the number of rows affected.
    ///
    /// MySQL uses `?` as the positional placeholder for parameters,
    /// unlike PostgreSQL which uses `$1`, `$2`, etc.
    /// Callers must use `?`-style placeholders in their SQL for MySQL.
    ///
    /// After execution, any server warnings are fetched via `SHOW WARNINGS`
    /// (only when `warnings_count > 0` in the `OkPacket`; no extra round-trip
    /// on the common no-warning path) and stored in `last_warnings`.
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        // Clear warnings before starting.
        {
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            store.clear();
        }

        let mut conn = self
            .pool
            .get_conn()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))?;

        let mysql_params = core_params_to_mysql(params);
        let result = conn
            .exec_iter(sql, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;

        let affected = result.affected_rows();
        // Read warning count before consuming the result set.
        let warnings_count = result.warnings();
        result
            .drop_result()
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;

        // Conditional SHOW WARNINGS — skips extra round-trip when count is 0.
        if warnings_count > 0 {
            let fetched = fetch_show_warnings(&mut conn)
                .await
                .map_err(OxiSqlError::from)?;
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            *store = fetched;
        }

        Ok(affected)
    }

    /// Execute a `SELECT` statement and return the result rows.
    ///
    /// MySQL uses `?` as the positional placeholder; callers must use `?` in their SQL.
    ///
    /// Internally uses explicit `prep()` + `exec()` (binary protocol) so the server
    /// can cache the prepared-statement plan.  This is equivalent to calling
    /// [`MyConnection::query_binary`] but returns `OxiSqlError` for trait uniformity.
    ///
    /// After execution, any server warnings are fetched via `SHOW WARNINGS`
    /// (only when the warning count from the last `OkPacket` is `> 0`) and
    /// stored in `last_warnings`.
    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        self.query_internal(sql, params)
            .await
            .map_err(OxiSqlError::from)
    }

    /// Return any SQL warnings generated by the most recently completed
    /// `execute` or `query` call on this connection.
    ///
    /// The list is cleared before each statement and repopulated from
    /// `SHOW WARNINGS` only when the server reports `warnings_count > 0`.
    fn last_warnings(&self) -> Vec<SqlWarning> {
        self.last_warnings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Begin a transaction.
    ///
    /// The returned [`MyTransaction`] holds an owned connection from the pool.
    /// You **must** call [`Transaction::commit`] or [`Transaction::rollback`]
    /// before dropping; otherwise `mysql_async` rolls back implicitly.
    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        let tx = self
            .pool
            .start_transaction(TxOpts::default())
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))?;

        Ok(Box::new(MyTransaction { tx: Some(tx) }))
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        // Clear warnings at the start of a batch; warnings from the last
        // statement in the batch are captured at the end.
        {
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            store.clear();
        }

        let mut conn = self
            .pool
            .get_conn()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))?;

        let mut total = 0u64;
        let mut last_warnings_count: u16 = 0;
        for stmt in sql.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                let result = conn
                    .exec_iter(trimmed, mysql_async::Params::Empty)
                    .await
                    .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
                total += result.affected_rows();
                last_warnings_count = result.warnings();
                result
                    .drop_result()
                    .await
                    .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
            }
        }

        // Capture warnings from the last statement in the batch.
        if last_warnings_count > 0 {
            let fetched = fetch_show_warnings(&mut conn)
                .await
                .map_err(OxiSqlError::from)?;
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            *store = fetched;
        }

        Ok(total)
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        let mut conn = self
            .pool
            .get_conn()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))?;
        conn.ping()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT TABLE_NAME, TABLE_SCHEMA, TABLE_TYPE \
                 FROM INFORMATION_SCHEMA.TABLES \
                 WHERE TABLE_SCHEMA = DATABASE() \
                 ORDER BY TABLE_NAME",
                &[],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("TABLE_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let schema = row.try_get::<String>("TABLE_SCHEMA").ok();
                let type_str = row
                    .try_get::<String>("TABLE_TYPE")
                    .unwrap_or_else(|_| "BASE TABLE".to_string());
                Ok(TableInfo {
                    name,
                    schema,
                    table_type: TableType::from(type_str.as_str()),
                })
            })
            .collect()
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT COLUMN_NAME, ORDINAL_POSITION, DATA_TYPE, IS_NULLABLE, COLUMN_DEFAULT, \
                        CHARACTER_MAXIMUM_LENGTH, NUMERIC_PRECISION, NUMERIC_SCALE \
                 FROM INFORMATION_SCHEMA.COLUMNS \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
                 ORDER BY ORDINAL_POSITION",
                &[&table],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("COLUMN_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let ordinal: i64 = row
                    .try_get::<i64>("ORDINAL_POSITION")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let data_type = row
                    .try_get::<String>("DATA_TYPE")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let nullable_str = row
                    .try_get::<String>("IS_NULLABLE")
                    .unwrap_or_else(|_| "YES".to_string());
                let default = row.try_get::<String>("COLUMN_DEFAULT").ok();
                let max_len: Option<i64> = row.try_get::<i64>("CHARACTER_MAXIMUM_LENGTH").ok();
                let num_prec: Option<i64> = row.try_get::<i64>("NUMERIC_PRECISION").ok();
                let num_scale: Option<i64> = row.try_get::<i64>("NUMERIC_SCALE").ok();
                Ok(ColumnInfo {
                    name,
                    ordinal_position: u32::try_from(ordinal).unwrap_or(0),
                    data_type,
                    nullable: !nullable_str.eq_ignore_ascii_case("NO"),
                    default,
                    max_length: max_len.and_then(|n| u64::try_from(n).ok()),
                    numeric_precision: num_prec.and_then(|n| u32::try_from(n).ok()),
                    numeric_scale: num_scale.and_then(|n| u32::try_from(n).ok()),
                })
            })
            .collect()
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT INDEX_NAME, COLUMN_NAME, NON_UNIQUE, SEQ_IN_INDEX \
                 FROM INFORMATION_SCHEMA.STATISTICS \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
                 ORDER BY INDEX_NAME, SEQ_IN_INDEX",
                &[&table],
            )
            .await?;
        // Group by index name, accumulating columns in order.
        let mut index_map: std::collections::HashMap<String, (bool, bool, Vec<String>)> =
            std::collections::HashMap::new();
        // Preserve insertion order for deterministic results.
        let mut order: Vec<String> = Vec::new();
        for row in rows {
            let idx_name = row
                .try_get::<String>("INDEX_NAME")
                .map_err(|e| OxiSqlError::Other(e.to_string()))?;
            let col_name = row
                .try_get::<String>("COLUMN_NAME")
                .map_err(|e| OxiSqlError::Other(e.to_string()))?;
            let non_unique: i64 = row.try_get::<i64>("NON_UNIQUE").unwrap_or(1);
            let is_primary = idx_name == "PRIMARY";
            let is_unique = non_unique == 0;
            if !index_map.contains_key(&idx_name) {
                order.push(idx_name.clone());
            }
            let entry = index_map
                .entry(idx_name)
                .or_insert((is_unique, is_primary, Vec::new()));
            entry.2.push(col_name);
        }
        Ok(order
            .into_iter()
            .filter_map(|name| {
                index_map
                    .remove(&name)
                    .map(|(unique, primary, columns)| IndexInfo {
                        name,
                        columns,
                        unique,
                        primary,
                    })
            })
            .collect())
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT CONSTRAINT_NAME, COLUMN_NAME, \
                        REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
                 FROM INFORMATION_SCHEMA.KEY_COLUMN_USAGE \
                 WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? \
                       AND REFERENCED_TABLE_NAME IS NOT NULL \
                 ORDER BY CONSTRAINT_NAME, ORDINAL_POSITION",
                &[&table],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let constraint_name = row
                    .try_get::<String>("CONSTRAINT_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let column = row
                    .try_get::<String>("COLUMN_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let foreign_table = row
                    .try_get::<String>("REFERENCED_TABLE_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let foreign_column = row
                    .try_get::<String>("REFERENCED_COLUMN_NAME")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                Ok(ForeignKeyInfo {
                    constraint_name,
                    column,
                    foreign_table,
                    foreign_column,
                    ..Default::default()
                })
            })
            .collect()
    }

    /// Stream query results row-by-row from MySQL.
    ///
    /// Acquires a pool connection, runs `exec()` (which fetches all rows), then
    /// yields each converted row individually.  This is equivalent to the
    /// default trait implementation but avoids going through `async_trait`'s
    /// boxing overhead.
    fn query_stream<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a dyn oxisql_core::ToSqlValue],
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = Result<oxisql_core::Row, oxisql_core::OxiSqlError>>
                + Send
                + 'a,
        >,
    > {
        use futures::StreamExt;
        let fut = self.query(sql, params);
        Box::pin(futures::stream::once(fut).flat_map(|result| match result {
            Ok(rows) => futures::stream::iter(rows.into_iter().map(Ok)).left_stream(),
            Err(e) => futures::stream::once(async move { Err(e) }).right_stream(),
        }))
    }

    /// Compile a SQL statement for repeated execution with different parameters.
    ///
    /// Acquires an exclusive `Conn` from the pool and prepares the statement
    /// server-side.  The connection is held by the returned [`MySqlPrepared`]
    /// and returned to the pool when it is dropped.
    ///
    /// MySQL uses `?` as the positional placeholder (not `$1`).
    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        let mut conn = self
            .pool
            .get_conn()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Connection(e)))?;
        let stmt = conn
            .prep(sql)
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))?;
        Ok(Box::new(MySqlPrepared::new(conn, stmt, sql.to_string())))
    }
}

// ── savepoint validation ──────────────────────────────────────────────────────

/// Validate that a savepoint name contains only ASCII alphanumeric characters
/// and underscores, and is non-empty.  Returns `Err(OxiSqlError::Parse(…))`
/// if the name is invalid.
fn validate_savepoint_name(name: &str) -> Result<(), OxiSqlError> {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(OxiSqlError::Parse(format!(
            "invalid savepoint name: '{name}'"
        )));
    }
    Ok(())
}

// ── MyTransaction ─────────────────────────────────────────────────────────────

/// A MySQL transaction holding an owned `mysql_async::Transaction<'static>`.
///
/// Call [`Transaction::commit`] or [`Transaction::rollback`] explicitly.
/// Dropping without doing so causes `mysql_async` to roll back the transaction
/// when the inner connection is returned to the pool.
pub struct MyTransaction {
    /// `None` after `commit` or `rollback` has consumed the transaction.
    tx: Option<mysql_async::Transaction<'static>>,
}

impl MyTransaction {
    /// Returns the last auto-increment ID generated by the most recent INSERT,
    /// or `None` if no INSERT has been executed in this transaction yet.
    pub fn last_insert_id(&self) -> Option<u64> {
        // `mysql_async::Transaction` derefs to `Conn`, which exposes
        // `last_insert_id() -> Option<u64>`.
        self.tx.as_ref().and_then(|t| t.last_insert_id())
    }
}

#[async_trait]
impl Transaction for MyTransaction {
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let tx = self.tx.as_mut().ok_or(OxiSqlError::NotConnected)?;

        let mysql_params = core_params_to_mysql(params);
        let result = tx
            .exec_iter(sql, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;

        let affected = result.affected_rows();
        result
            .drop_result()
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;

        Ok(affected)
    }

    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let tx = self.tx.as_mut().ok_or(OxiSqlError::NotConnected)?;

        let mysql_params = core_params_to_mysql(params);
        let mysql_rows: Vec<mysql_async::Row> = tx
            .exec(sql, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;

        mysql_rows
            .into_iter()
            .map(|r| mysql_row_to_core(r).map_err(OxiSqlError::from))
            .collect()
    }

    async fn savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        self.execute(&format!("SAVEPOINT {name}"), &[]).await?;
        Ok(())
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        self.execute(&format!("RELEASE SAVEPOINT {name}"), &[])
            .await?;
        Ok(())
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        self.execute(&format!("ROLLBACK TO SAVEPOINT {name}"), &[])
            .await?;
        Ok(())
    }

    async fn commit(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        let tx = self.tx.take().ok_or(OxiSqlError::NotConnected)?;

        tx.commit()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        let tx = self.tx.take().ok_or(OxiSqlError::NotConnected)?;

        tx.rollback()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))
    }
}

// ── type-mapping helper: re-export for tests ──────────────────────────────────

/// Convert a `mysql_async::Value` to an `oxisql_core::Value`.
///
/// Exposed for testing and advanced use cases.
pub fn map_value(v: mysql_async::Value) -> Result<Value, MysqlError> {
    mysql_value_to_core(v)
}

// ── Binary protocol and multi-result-set extensions ──────────────────────────

impl MyConnection {
    /// Core binary-protocol query implementation shared by both the
    /// [`Connection::query`] trait method and the public [`Self::query_binary`]
    /// convenience method.
    ///
    /// Uses explicit `prep()` so the server can cache the statement plan, then
    /// `exec()` to send the actual values over the binary wire protocol.
    /// Errors that represent well-known MySQL conditions (e.g. constraint
    /// violations, connection loss) are run through [`classify_mysql_error`] before
    /// being returned as [`MysqlError`] variants.
    ///
    /// Clears `last_warnings` before the call and captures any server-reported
    /// warnings into it afterwards (conditional `SHOW WARNINGS` — only when the
    /// server reports `warnings_count > 0`).
    async fn query_internal(
        &self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, MysqlError> {
        // Clear warnings before starting.
        {
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            store.clear();
        }

        let mut conn = self.pool.get_conn().await.map_err(MysqlError::Connection)?;
        let stmt = conn.prep(sql).await.map_err(classify_mysql_error)?;
        let mysql_params = core_params_to_mysql(params);
        let mysql_rows: Vec<mysql_async::Row> = conn
            .exec(&stmt, mysql_params)
            .await
            .map_err(classify_mysql_error)?;

        // After exec(), the warning count is available on the connection.
        let warnings_count = conn.get_warnings();

        let rows = mysql_rows
            .into_iter()
            .map(mysql_row_to_core)
            .collect::<Result<Vec<Row>, _>>()?;

        // Conditional SHOW WARNINGS — no extra round-trip when count is 0.
        if warnings_count > 0 {
            let fetched = fetch_show_warnings(&mut conn).await?;
            let mut store = self.last_warnings.lock().unwrap_or_else(|e| e.into_inner());
            *store = fetched;
        }

        Ok(rows)
    }

    /// Execute a query using an explicit server-side prepared statement (binary
    /// protocol).
    ///
    /// This method calls `conn.prep(sql)` explicitly, giving the server a chance to
    /// cache the statement plan across calls on the same underlying connection.
    /// For one-shot queries the difference is negligible; for hot-path repeated
    /// queries prefer [`Connection::prepare`] to reuse the same statement handle.
    ///
    /// MySQL uses `?` as the positional placeholder (not `$1`).
    ///
    /// This is an alias for the default [`Connection::query`] path, which also uses
    /// binary protocol.  Kept for API compatibility.
    pub async fn query_binary(
        &self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, MysqlError> {
        self.query_internal(sql, params).await
    }

    /// Execute a stored procedure and collect all result sets it produces.
    ///
    /// MySQL stored procedures may emit zero or more `SELECT` result sets before
    /// returning.  This method drives `QueryResult::next()` until the current set
    /// is exhausted, then checks `is_empty()` to discover additional sets,
    /// repeating until all sets have been consumed.
    ///
    /// Parameters are positional (`?` placeholders) and passed as [`Value`]s.
    /// They are sent via `exec_iter` (binary protocol prepared statement), which
    /// is the recommended path for parameterised `CALL` on MySQL 5.7+/8.x.
    ///
    /// Returns a `Vec` of result sets, where each inner `Vec<Row>` corresponds to
    /// one `SELECT` emitted by the procedure.
    pub async fn call_procedure_multi(
        &self,
        name: &str,
        params: Vec<Value>,
    ) -> Result<Vec<Vec<Row>>, MysqlError> {
        let mut conn = self.pool.get_conn().await.map_err(MysqlError::Connection)?;

        // Build `CALL name(?, ?, ...)` with one placeholder per parameter.
        let placeholders: String = std::iter::repeat_n("?", params.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!("CALL {name}({placeholders})");

        // Convert core Values to mysql_async positional params.
        let mysql_vals: Vec<mysql_async::Value> = params.iter().map(core_value_to_mysql).collect();
        let mysql_params = if mysql_vals.is_empty() {
            mysql_async::Params::Empty
        } else {
            mysql_async::Params::Positional(mysql_vals)
        };

        // exec_iter (binary protocol) preserves multi-result-set boundaries;
        // QueryResult::next() advances within and across result sets.
        let mut query_result = conn
            .exec_iter(sql.as_str(), mysql_params)
            .await
            .map_err(MysqlError::Query)?;

        let mut all_result_sets: Vec<Vec<Row>> = Vec::new();

        loop {
            // Collect all rows from the current result set.
            // QueryResult::next() returns None at the result-set boundary,
            // which also advances the internal cursor past the OK packet.
            let mut current_set: Vec<Row> = Vec::new();
            while let Some(raw_row) = query_result.next().await.map_err(MysqlError::Query)? {
                current_set.push(mysql_row_to_core(raw_row)?);
            }
            all_result_sets.push(current_set);

            // is_empty() returns true when no more rows AND no more result sets.
            if query_result.is_empty() {
                break;
            }
        }

        Ok(all_result_sets)
    }

    /// Gracefully disconnect all connections in the pool and shut it down.
    ///
    /// This sends a `COM_QUIT` packet on every active connection and waits
    /// until they are all closed.  Prefer this over simply dropping the
    /// `MyConnection` when you need a clean shutdown (e.g. in integration
    /// tests or process-exit paths).
    ///
    /// Consumes `self` because the pool is unusable after disconnection.
    pub async fn disconnect(self) -> Result<(), MysqlError> {
        // Pool is internally reference-counted; clone is cheap and lets us
        // disconnect without moving out of a type that implements Drop.
        let pool = self.pool.clone();
        drop(self);
        pool.disconnect().await.map_err(MysqlError::Connection)
    }

    /// Bulk-insert rows into a table using batched `INSERT` statements.
    ///
    /// This is a portable alternative to `LOAD DATA LOCAL INFILE` that works
    /// without requiring the `LOCAL INFILE` MySQL server permission or any
    /// client-side file access.  Rows are sent over the standard binary
    /// protocol in groups of `batch_size` rows per statement, which
    /// dramatically reduces round-trips compared to individual INSERTs while
    /// keeping memory use bounded.
    ///
    /// # Arguments
    ///
    /// * `table`      — target table name (must be a valid MySQL identifier)
    /// * `columns`    — column names in order, matching each row's value order
    /// * `rows` — row data as `Vec<Vec<Value>>`; each inner `Vec` must have exactly `columns.len()` elements
    /// * `batch_size` — rows per `INSERT` statement (clamped to a minimum of 1)
    ///
    /// # Returns
    ///
    /// The total number of rows inserted across all batches.
    ///
    /// # Errors
    ///
    /// Returns [`MysqlError`] on connection failure, statement preparation
    /// error, or execution error (e.g. a unique constraint violation).
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use oxisql_mysql::{MyConnection, TlsMode};
    /// use oxisql_core::Value;
    ///
    /// let conn = MyConnection::connect("mysql://root:@localhost/mydb", TlsMode::Disabled).await?;
    ///
    /// let rows = vec![
    ///     vec![Value::I64(1), Value::Text("alice".into())],
    ///     vec![Value::I64(2), Value::Text("bob".into())],
    /// ];
    /// let inserted = conn
    ///     .load_data_batched("users", &["id", "name"], rows, 500)
    ///     .await?;
    /// assert_eq!(inserted, 2);
    /// # Ok(())
    /// # }
    /// ```
    pub async fn load_data_batched(
        &self,
        table: &str,
        columns: &[&str],
        rows: Vec<Vec<oxisql_core::Value>>,
        batch_size: usize,
    ) -> Result<u64, MysqlError> {
        if rows.is_empty() {
            return Ok(0);
        }

        let batch_size = batch_size.max(1);
        let col_list = columns.join(", ");
        let mut total_inserted = 0u64;

        for chunk in rows.chunks(batch_size) {
            // Build "(?, ?, …), (?, ?, …), …" with one tuple per row.
            let placeholders: String = chunk
                .iter()
                .map(|row| {
                    let ph: Vec<&str> = row.iter().map(|_| "?").collect();
                    format!("({})", ph.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", ");

            let sql = format!("INSERT INTO {table} ({col_list}) VALUES {placeholders}");

            // Flatten all values in row-major order for positional binding.
            let params: Vec<mysql_async::Value> = chunk
                .iter()
                .flat_map(|row| row.iter().map(core_value_to_mysql))
                .collect();

            let mut conn = self.pool.get_conn().await.map_err(MysqlError::Connection)?;
            // exec_drop accepts &str directly and uses the binary protocol.
            conn.exec_drop(sql.as_str(), mysql_async::Params::Positional(params))
                .await
                .map_err(MysqlError::Query)?;

            total_inserted += chunk.len() as u64;
        }

        Ok(total_inserted)
    }
}

// ── Reconnect-error detection ─────────────────────────────────────────────────

/// Return `true` if `err` indicates a transient connection loss that can be
/// retried by acquiring a fresh pool connection.
///
/// Covers:
///
/// | Code  | Meaning |
/// |-------|---------|
/// | 2006  | `CR_SERVER_GONE_ERROR` — server closed the socket |
/// | 2013  | `CR_SERVER_LOST` — connection lost during query |
/// | 1047  | `ER_UNKNOWN_COM_ERROR` — can appear after server restart |
/// | `Io`  | Any I/O-layer error (broken pipe, connection reset, etc.) |
///
/// Note: errors inside an active **transaction** should **not** be retried
/// even if `is_reconnect_error` returns `true`, because the transaction state
/// on the server is unknown.  Auto-retry is only safe on stateless,
/// non-transactional operations.
pub fn is_reconnect_error(err: &mysql_async::Error) -> bool {
    match err {
        mysql_async::Error::Server(srv_err) => {
            matches!(
                srv_err.code,
                2006 | // CR_SERVER_GONE_ERROR
                2013 | // CR_SERVER_LOST
                1047 // ER_UNKNOWN_COM_ERROR (can occur on reconnect)
            )
        }
        mysql_async::Error::Io(_) => true,
        _ => false,
    }
}

// ── MyConnectionBuilder ───────────────────────────────────────────────────────

/// Builder for configuring and establishing a MySQL connection.
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
/// use oxisql_mysql::MyConnectionBuilder;
///
/// let conn = MyConnectionBuilder::new()
///     .host("localhost")
///     .port(3306)
///     .dbname("mydb")
///     .user("root")
///     .password("secret")
///     .connect()
///     .await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Default)]
pub struct MyConnectionBuilder {
    host: Option<String>,
    port: Option<u16>,
    dbname: Option<String>,
    user: Option<String>,
    password: Option<String>,
    connect_timeout_secs: Option<u64>,
    tls_mode: TlsMode,
    /// Explicit `SslOpts` override, taking precedence over `tls_mode` when set.
    ssl_opts_override: Option<SslOpts>,
    /// Minimum idle connections in the pool.
    pool_min: Option<usize>,
    /// Maximum connections in the pool.
    pool_max: Option<usize>,
    /// Idle timeout — connections idle longer than this are removed from the pool.
    pool_idle_timeout: Option<Duration>,
    /// Absolute connection TTL — maximum lifetime of any pooled connection.
    pool_ttl: Option<Duration>,
}

impl MyConnectionBuilder {
    /// Create a builder with defaults.
    ///
    /// Defaults: `host = "localhost"`, `port = 3306`, `user = "root"`,
    /// `password = ""`, `dbname = ""`, `tls_mode = TlsMode::Disabled`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the MySQL host (default: `"localhost"`).
    pub fn host(mut self, h: impl Into<String>) -> Self {
        self.host = Some(h.into());
        self
    }

    /// Set the MySQL port (default: `3306`).
    pub fn port(mut self, p: u16) -> Self {
        self.port = Some(p);
        self
    }

    /// Set the database name to connect to.
    pub fn dbname(mut self, db: impl Into<String>) -> Self {
        self.dbname = Some(db.into());
        self
    }

    /// Set the MySQL username (default: `"root"`).
    pub fn user(mut self, u: impl Into<String>) -> Self {
        self.user = Some(u.into());
        self
    }

    /// Set the MySQL password (default: `""`).
    pub fn password(mut self, pw: impl Into<String>) -> Self {
        self.password = Some(pw.into());
        self
    }

    /// Set the connection timeout in seconds.
    ///
    /// When set, the initial pool connection acquisition in [`connect`][Self::connect]
    /// is wrapped with `tokio::time::timeout`.  If the first `get_conn()` call
    /// does not return within the given duration a
    /// [`MysqlError::ConnectionTimeout`] is returned immediately.
    ///
    /// `mysql_async` 0.36 does not expose a per-connection TCP timeout via
    /// `OptsBuilder`, so the timeout is enforced at the Tokio async layer.
    pub fn connect_timeout_secs(mut self, secs: u64) -> Self {
        self.connect_timeout_secs = Some(secs);
        self
    }

    /// Set the minimum number of idle connections kept alive in the pool
    /// (default: `0`).
    ///
    /// The pool will maintain at least this many open connections even when idle.
    /// Must satisfy `pool_min <= pool_max`.
    pub fn pool_min(mut self, n: usize) -> Self {
        self.pool_min = Some(n);
        self
    }

    /// Set the maximum number of connections in the pool (default: `10`).
    ///
    /// Additional connection requests above this limit will queue until a
    /// connection becomes available.  Must be `> 0`.
    pub fn pool_max(mut self, n: usize) -> Self {
        self.pool_max = Some(n);
        self
    }

    /// Set the idle connection timeout in seconds (default: `600`).
    ///
    /// Connections that have been idle for longer than this duration are
    /// eligible for removal by the pool recycler.  Set to `0` to disable idle
    /// recycling.
    pub fn pool_idle_timeout(mut self, secs: u64) -> Self {
        self.pool_idle_timeout = Some(Duration::from_secs(secs));
        self
    }

    /// Set the absolute maximum connection lifetime in seconds.
    ///
    /// After this TTL the connection is closed and removed from the pool,
    /// independent of whether it is idle or active.  Disabled by default.
    pub fn pool_ttl(mut self, secs: u64) -> Self {
        self.pool_ttl = Some(Duration::from_secs(secs));
        self
    }

    /// Set the TLS mode (default: [`TlsMode::Disabled`]).
    pub fn tls_mode(mut self, mode: TlsMode) -> Self {
        self.tls_mode = mode;
        self
    }

    /// Require TLS but skip server certificate verification.
    ///
    /// **INSECURE** — intended for development and testing environments only.
    /// Production code should use [`ssl_with_ca_pem`][Self::ssl_with_ca_pem] or
    /// supply a trusted root certificate store instead.
    ///
    /// Installs the `rustls-rustcrypto` provider (guarded) and configures
    /// `SslOpts` to accept invalid certificates and skip domain validation.
    pub fn ssl_skip_verify(mut self) -> Self {
        ensure_crypto_provider();
        self.ssl_opts_override = Some(
            SslOpts::default()
                .with_danger_accept_invalid_certs(true)
                .with_danger_skip_domain_validation(true),
        );
        self
    }

    /// Require TLS and verify the server certificate against a custom CA.
    ///
    /// `ca_pem` must be a PEM-encoded certificate authority certificate.
    /// Multiple certificates may be concatenated in a single `Vec<u8>`.
    ///
    /// Installs the `rustls-rustcrypto` provider (guarded).
    pub fn ssl_with_ca_pem(mut self, ca_pem: Vec<u8>) -> Self {
        ensure_crypto_provider();
        // PathOrBuf<'static> is not re-exported from mysql_async's public API,
        // but impl From<Vec<u8>> for PathOrBuf<'static> is, so type inference
        // resolves the correct variant when we pass the Vec<u8> into the
        // expected Vec<PathOrBuf<'static>> parameter.
        self.ssl_opts_override = Some(SslOpts::default().with_root_certs(vec![ca_pem.into()]));
        self
    }

    /// Disable TLS entirely (plain-text connection).
    ///
    /// This is the default behaviour; calling this method is only necessary
    /// to explicitly override a previously set TLS mode on the builder.
    pub fn ssl_disabled(mut self) -> Self {
        self.tls_mode = TlsMode::Disabled;
        self.ssl_opts_override = None;
        self
    }

    /// Build the MySQL connection URI and connect.
    ///
    /// When [`ssl_skip_verify`][Self::ssl_skip_verify] or
    /// [`ssl_with_ca_pem`][Self::ssl_with_ca_pem] has been called, those
    /// `SslOpts` take precedence over the `tls_mode` setting.
    ///
    /// Pool options set via [`pool_min`][Self::pool_min],
    /// [`pool_max`][Self::pool_max], [`pool_idle_timeout`][Self::pool_idle_timeout],
    /// and [`pool_ttl`][Self::pool_ttl] are applied unconditionally.
    ///
    /// # Errors
    ///
    /// Returns [`MysqlError`] if the URI is malformed, pool constraints are
    /// invalid (`pool_min > pool_max`), or the connection fails.
    pub async fn connect(self) -> Result<MyConnection, crate::error::MysqlError> {
        let host = self.host.as_deref().unwrap_or("localhost");
        let port = self.port.unwrap_or(3306);
        let user = self.user.as_deref().unwrap_or("root");
        let password = self.password.as_deref().unwrap_or("");
        let dbname = self.dbname.as_deref().unwrap_or("");
        let uri = format!("mysql://{user}:{password}@{host}:{port}/{dbname}");

        // Build PoolOpts from pool_min/pool_max/pool_idle_timeout/pool_ttl.
        let pool_opts = self.build_pool_opts()?;

        // If caller used ssl_skip_verify / ssl_with_ca_pem, bypass TlsMode and
        // pass the explicit SslOpts directly.
        let pool = if let Some(ssl_opts) = self.ssl_opts_override {
            build_pool(&uri, Some(ssl_opts), pool_opts)?
        } else {
            // TlsMode path.
            let ssl_opts = match self.tls_mode {
                TlsMode::Disabled => None,
                TlsMode::Rustls(_cfg) => {
                    ensure_crypto_provider();
                    Some(SslOpts::default())
                }
            };
            build_pool(&uri, ssl_opts, pool_opts)?
        };

        // Probe with an eager get_conn() under the connection timeout, if set.
        // mysql_async 0.36 does not expose a TCP-level connect timeout via
        // OptsBuilder, so we enforce the timeout at the Tokio async layer.
        if let Some(timeout_secs) = self.connect_timeout_secs {
            let timeout_dur = Duration::from_secs(timeout_secs);
            let probe = tokio::time::timeout(timeout_dur, pool.get_conn()).await;
            match probe {
                Err(_elapsed) => {
                    return Err(MysqlError::ConnectionTimeout(format!(
                        "could not acquire a connection within {timeout_secs}s"
                    )));
                }
                Ok(Err(e)) => {
                    return Err(MysqlError::Connection(e));
                }
                Ok(Ok(_conn)) => {
                    // Connection confirmed; the Conn is returned to the pool
                    // when it is dropped here.
                }
            }
        }

        Ok(MyConnection {
            pool,
            last_warnings: Arc::new(Mutex::new(Vec::new())),
        })
    }

    /// Assemble a `PoolOpts` from pool configuration fields, if any were set.
    ///
    /// Returns `None` when no pool configuration was requested (the pool will
    /// use mysql_async defaults).  Returns `Err` when `pool_min > pool_max`.
    fn build_pool_opts(&self) -> Result<Option<PoolOpts>, MysqlError> {
        let has_pool_config = self.pool_min.is_some()
            || self.pool_max.is_some()
            || self.pool_idle_timeout.is_some()
            || self.pool_ttl.is_some();

        if !has_pool_config {
            return Ok(None);
        }

        let min = self.pool_min.unwrap_or(0);
        let max = self.pool_max.unwrap_or(10);

        let constraints = PoolConstraints::new(min, max).ok_or_else(|| {
            MysqlError::PoolExhausted(format!(
                "invalid pool constraints: pool_min ({min}) must be <= pool_max ({max}) and max must be > 0"
            ))
        })?;

        let mut opts = PoolOpts::default().with_constraints(constraints);

        if let Some(idle_ttl) = self.pool_idle_timeout {
            opts = opts.with_inactive_connection_ttl(idle_ttl);
        }
        if let Some(abs_ttl) = self.pool_ttl {
            opts = opts.with_abs_conn_ttl(Some(abs_ttl));
        }

        Ok(Some(opts))
    }
}
