//! PostgreSQL connection implementing [`oxisql_core::Connection`].
//!
//! # Interior mutability
//!
//! `tokio_postgres::Client::transaction` requires `&mut self`.  Since the
//! `Connection` trait only offers `&self`, we wrap the client in an
//! `Arc<Mutex<Client>>` and acquire an `OwnedMutexGuard` for each transaction
//! scope.  This matches the same pattern used by `oxisql-embedded`.
//!
//! # Transaction semantics
//!
//! `PgTransaction` holds an `Option<OwnedMutexGuard<Client>>` for the duration
//! of the transaction.  Callers should call [`Transaction::commit`] or
//! [`Transaction::rollback`] to terminate the transaction explicitly.
//!
//! ## Automatic rollback on Drop
//!
//! If a `PgTransaction` is dropped without an explicit `commit` or `rollback`,
//! the `Drop` implementation attempts to schedule a `ROLLBACK` command on the
//! active Tokio runtime.  When no runtime is reachable (e.g., in a sync
//! context), the guard is simply released and the server will roll back the
//! open transaction on the next connection reset.
//!
//! Explicit termination is still recommended for clarity and to capture errors:
//! ```rust,no_run
//! # async fn example(conn: &oxisql_postgres::PgConnection) -> Result<(), oxisql_core::OxiSqlError> {
//! use oxisql_core::Connection;
//! let txn = conn.transaction().await?;
//! // ... operations ...
//! txn.rollback().await?;  // or .commit()
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_postgres::types::ToSql;
use tokio_postgres::{NoTls, Statement};

use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    TableInfo, TableType, ToSqlValue, Transaction,
};

use crate::builder::TlsMode;
use crate::copy;
use crate::error::PgError;
use crate::notify::{self, PgNotification};
use crate::pipeline::PgPipeline;
use crate::prepared::{PgPrepared, StmtCache};
use crate::tls::PgTls;
use crate::types::{pg_row_to_row, value_to_param, OwnedParam};

// ── PgConnection ─────────────────────────────────────────────────────────────

/// Metadata for a single output column from a prepared statement.
///
/// Obtained via [`PgConnection::describe`] — describes column names and
/// PostgreSQL type names without executing the query.
#[derive(Debug, Clone)]
pub struct ColumnDescription {
    /// The column name as reported by the server.
    pub name: String,
    /// The PostgreSQL type name (e.g. `"int4"`, `"text"`, `"uuid"`).
    pub type_name: String,
    /// Always `true`: PostgreSQL does not expose nullability in column
    /// descriptions returned by the extended-query protocol.
    pub nullable: bool,
}

/// An asynchronous PostgreSQL connection backed by `tokio-postgres` (Pure Rust,
/// no `libpq`).
///
/// The inner `Client` is wrapped in `Arc<Mutex<_>>` to allow the `Connection`
/// trait's `&self` transaction method to obtain exclusive mutable access.
///
/// `Clone` is cheap: both `Arc`s inside are reference-counted.
///
/// # Statement cache
///
/// A second `Arc<std::sync::Mutex<HashMap<…>>>` holds compiled
/// `tokio_postgres::Statement` handles keyed on SQL text.  The
/// `std::sync::Mutex` (not `tokio::sync::Mutex`) is intentional: we only hold
/// the lock during purely synchronous HashMap lookups, never across an `await`.
///
/// # Reconnection
///
/// When a `PgConnection` is created via [`PgConnection::connect`], the original
/// connection string and TLS mode are stored internally.  Call
/// [`PgConnection::reconnect`] to obtain a brand-new `PgConnection` built from
/// the same parameters — useful after a broken-pipe error.  The caller is
/// responsible for replacing their handle with the returned value.
#[derive(Clone)]
pub struct PgConnection {
    inner: Arc<Mutex<tokio_postgres::Client>>,
    stmt_cache: StmtCache,
    /// Original connection string passed to [`PgConnection::connect`].
    ///
    /// `None` when created via [`PgConnection::from_client`].
    reconnect_uri: Option<String>,
    /// TLS mode used for the initial connection.
    ///
    /// Stored alongside `reconnect_uri` so that [`PgConnection::reconnect`]
    /// can re-establish an encrypted connection without silently downgrading
    /// to plaintext.
    reconnect_tls: TlsMode,
    /// Sender side of the broadcast channel used for LISTEN/NOTIFY.
    ///
    /// `None` when the connection was created via [`PgConnection::from_client`]
    /// (no `tokio_postgres::Connection` object was available to intercept
    /// notifications).  `Some` when created via [`PgConnection::connect`].
    notif_tx: Option<tokio::sync::broadcast::Sender<PgNotification>>,
}

impl PgConnection {
    /// Construct a `PgConnection` from an existing `tokio_postgres::Client`.
    ///
    /// Useful when the `Client` was obtained outside of OxiSQL (e.g., from a
    /// custom connection pool or test harness).  A fresh, empty statement cache
    /// is created automatically.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let (client, conn) = tokio_postgres::connect(
    ///     "host=localhost user=postgres",
    ///     tokio_postgres::NoTls,
    /// ).await?;
    /// tokio::spawn(async move { let _: Result<_, _> = conn.await; });
    ///
    /// let pg_conn = oxisql_postgres::PgConnection::from_client(client);
    /// # Ok(())
    /// # }
    /// ```
    pub fn from_client(client: tokio_postgres::Client) -> Self {
        Self {
            inner: Arc::new(Mutex::new(client)),
            stmt_cache: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            reconnect_uri: None,
            reconnect_tls: TlsMode::Disabled,
            notif_tx: None,
        }
    }

    /// Connect to a PostgreSQL server using the given connection string.
    ///
    /// `conn_str` follows the `tokio-postgres` format, e.g.:
    /// `"host=localhost port=5432 user=postgres password=secret dbname=mydb"`.
    ///
    /// # TLS
    ///
    /// Pass [`TlsMode::Disabled`] for a plain-text connection (suitable for
    /// `localhost` or trusted network).  Pass [`TlsMode::Rustls`] with a
    /// `ClientConfig` built from OxiTLS for encrypted connections.
    pub async fn connect(conn_str: &str, tls: TlsMode) -> Result<Self, PgError> {
        // Preserve the TLS mode for reconnect before moving it into the match.
        let tls_clone = tls.clone();
        let (client, notif_tx) = match tls {
            TlsMode::Disabled => {
                let (client, connection) = tokio_postgres::connect(conn_str, NoTls).await?;
                // Spawn the notification-forwarding connection driver.
                let tx = notify::spawn_connection_driver(connection);
                (client, tx)
            }
            TlsMode::Rustls(cfg) => {
                let tls_connector = PgTls::new(cfg);
                let (client, connection) = tokio_postgres::connect(conn_str, tls_connector).await?;
                let tx = notify::spawn_connection_driver(connection);
                (client, tx)
            }
        };
        Ok(Self {
            inner: Arc::new(Mutex::new(client)),
            stmt_cache: Arc::new(std::sync::Mutex::new(HashMap::new())),
            reconnect_uri: Some(conn_str.to_string()),
            reconnect_tls: tls_clone,
            notif_tx: Some(notif_tx),
        })
    }

    /// Connect to a PostgreSQL server with a maximum wait duration.
    ///
    /// Wraps [`PgConnection::connect`] with [`tokio::time::timeout`].  If the
    /// connection is not established within `timeout`, returns
    /// [`PgError::Timeout`] rather than hanging indefinitely.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Timeout`] if `timeout` elapses before the server
    /// acknowledges the connection.  All other error conditions are propagated
    /// unchanged from [`PgConnection::connect`].
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use std::time::Duration;
    /// use oxisql_postgres::{PgConnection, TlsMode};
    ///
    /// let conn = PgConnection::connect_with_timeout(
    ///     "host=localhost user=postgres",
    ///     TlsMode::Disabled,
    ///     Duration::from_secs(5),
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_with_timeout(
        conn_str: &str,
        tls: TlsMode,
        timeout: Duration,
    ) -> Result<Self, PgError> {
        tokio::time::timeout(timeout, Self::connect(conn_str, tls))
            .await
            .map_err(|_| {
                PgError::Timeout(format!(
                    "connection timed out after {}ms",
                    timeout.as_millis()
                ))
            })?
    }

    /// Connect with TLS that skips server certificate verification.
    ///
    /// # Security Warning
    ///
    /// This method accepts **any** server certificate without validation.  It
    /// is vulnerable to man-in-the-middle attacks and should only be used in
    /// development or testing environments.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the TLS config cannot be built, or a
    /// [`PgError::Postgres`] / [`PgError::Timeout`] on connection failure.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let conn = oxisql_postgres::PgConnection::connect_skip_verify(
    ///     "host=localhost user=postgres",
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_skip_verify(conn_str: &str) -> Result<Self, PgError> {
        Self::connect(conn_str, TlsMode::skip_verify()?).await
    }

    /// Connect with TLS using a custom CA certificate in PEM format.
    ///
    /// The provided `ca_pem` bytes are parsed as a PEM bundle and added to the
    /// trust store (in addition to the Mozilla WebPKI root certificates).
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Tls`] if the PEM cannot be parsed or is empty, or a
    /// [`PgError::Postgres`] / [`PgError::Timeout`] on connection failure.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// let ca_pem = std::fs::read("/path/to/ca.crt")?;
    /// let conn = oxisql_postgres::PgConnection::connect_with_ca(
    ///     "host=db.example.com user=postgres",
    ///     ca_pem,
    /// ).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn connect_with_ca(conn_str: &str, ca_pem: Vec<u8>) -> Result<Self, PgError> {
        Self::connect(conn_str, TlsMode::with_ca_pem(ca_pem)?).await
    }
}

// ── PgConnection — query analysis helpers ────────────────────────────────────

impl PgConnection {
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

// ── PgConnection — COPY / NOTIFY / pipeline methods ─────────────────────────

impl PgConnection {
    /// Bulk-insert rows into `table` using the PostgreSQL `COPY … FROM STDIN`
    /// protocol (text/TSV format).
    ///
    /// Each element of `rows` must contain exactly `columns.len()` string
    /// values.  Values are automatically escaped for the PostgreSQL text COPY
    /// format.  Pass `"\\N"` (backslash-N) to represent a SQL `NULL`.
    ///
    /// Returns the number of rows inserted as reported by PostgreSQL.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Copy`] if `columns` is empty, if a row has the
    /// wrong number of fields, or if the underlying COPY protocol fails.
    pub async fn copy_in_text(
        &self,
        table: &str,
        columns: &[&str],
        rows: impl Iterator<Item = Vec<String>>,
    ) -> Result<u64, PgError> {
        copy::copy_in_text(&self.inner, table, columns, rows).await
    }

    /// Extract rows from `table` using the PostgreSQL `COPY … TO STDOUT`
    /// protocol (text/TSV format).
    ///
    /// Returns a `Vec<Vec<String>>` where each inner vec contains the field
    /// values for one row, with escape sequences decoded.  The literal
    /// `"\\N"` in the returned data represents a SQL `NULL`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Copy`] if `columns` is empty or if the underlying
    /// COPY protocol fails.
    pub async fn copy_out_text(
        &self,
        table: &str,
        columns: &[&str],
    ) -> Result<Vec<Vec<String>>, PgError> {
        copy::copy_out_text(&self.inner, table, columns).await
    }

    /// Send a `NOTIFY` command to `channel` with an optional `payload`.
    ///
    /// The channel name must consist only of ASCII alphanumeric characters
    /// and underscores.  The payload is automatically single-quote–escaped
    /// before inclusion in the SQL string.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] if the channel name is invalid or if
    /// the underlying query fails.
    pub async fn notify(&self, channel: &str, payload: &str) -> Result<(), PgError> {
        notify::notify(&self.inner, channel, payload).await
    }

    /// Register `LISTEN` on `channel` and return a [`notify::NotificationStream`].
    ///
    /// Requires that this connection was created via [`PgConnection::connect`].
    /// Connections obtained via [`PgConnection::from_client`] return
    /// [`PgError::Notify`] because no `tokio_postgres::Connection` driver is
    /// available to intercept incoming notifications.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] if the channel name is invalid, if this
    /// connection was created via `from_client`, or if the `LISTEN` command
    /// fails.
    pub async fn listen(&self, channel: &str) -> Result<notify::NotificationStream, PgError> {
        notify::listen(&self.inner, &self.notif_tx, channel).await
    }

    /// Create a new [`PgPipeline`] for batched query dispatch.
    ///
    /// Queue queries with [`PgPipeline::add_execute`] and
    /// [`PgPipeline::add_query`], then send all at once with
    /// [`PgPipeline::finish`].
    pub fn pipeline(&self) -> PgPipeline {
        PgPipeline::new(Arc::clone(&self.inner))
    }

    /// Establish a fresh connection using the same URI and TLS mode as this
    /// connection.
    ///
    /// Returns a brand-new [`PgConnection`] with an empty statement cache.
    /// The caller should replace their handle with the returned value after a
    /// broken-pipe or connection-lost error.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] (repurposed as a "no URI" sentinel) when
    /// this connection was created via [`PgConnection::from_client`] and no
    /// original connection string was recorded.
    ///
    /// Returns [`PgError::Postgres`] on connection failure.
    pub async fn reconnect(&self) -> Result<PgConnection, PgError> {
        match &self.reconnect_uri {
            None => Err(PgError::Notify(
                "cannot reconnect: connection was created via from_client (no URI stored)"
                    .to_string(),
            )),
            Some(uri) => PgConnection::connect(uri, self.reconnect_tls.clone()).await,
        }
    }

    /// Execute a query using the binary protocol for better throughput.
    ///
    /// Uses `tokio-postgres`'s `query_typed`, which sends explicit Postgres type
    /// annotations alongside each parameter value.  The server then returns
    /// result columns in **binary format** (format code 1), bypassing the
    /// text-serialization step used by [`Connection::query`].  This reduces
    /// per-row encoding/decoding overhead on both client and server, improving
    /// throughput for large result sets.
    ///
    /// Parameter types are inferred from the [`ToSqlValue`] variants:
    ///
    /// | OxiSQL variant | Declared Postgres type |
    /// |---|---|
    /// | `Bool` | `BOOL` |
    /// | `I64` | `INT8` |
    /// | `F64` | `FLOAT8` |
    /// | `Text` | `TEXT` |
    /// | `Blob` | `BYTEA` |
    /// | `Null` / extended types | `UNKNOWN` (server infers from context) |
    ///
    /// **Note**: parameters are declared as INT8/FLOAT8/TEXT rather than
    /// narrower types (INT4, FLOAT4, etc.).  If a column has a narrower type
    /// and no implicit cast is available, prefer [`Connection::query`] which
    /// lets the server apply type inference after `prepare`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Postgres`] on connection or server-side errors.
    /// Returns [`PgError::TypeConversion`] if a result column cannot be decoded.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[tokio::main]
    /// # async fn main() -> Result<(), Box<dyn std::error::Error>> {
    /// use oxisql_postgres::{PgConnection, TlsMode};
    ///
    /// let conn = PgConnection::connect("host=localhost user=postgres", TlsMode::Disabled).await?;
    /// let rows = conn.query_binary("SELECT id, name FROM users WHERE id = $1", &[&42_i64]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn query_binary(
        &self,
        sql: &str,
        params: &[&dyn oxisql_core::ToSqlValue],
    ) -> Result<Vec<Row>, PgError> {
        use tokio_postgres::types::Type;

        let client = self.inner.lock().await;
        let owned = build_params(params);

        // Build (ref, Type) pairs.  Type::UNKNOWN lets the server use context
        // to resolve the type when we cannot determine it exactly.
        let typed_params: Vec<(&(dyn ToSql + Sync), Type)> = owned
            .iter()
            .map(|p| {
                let ty = match p {
                    OwnedParam::Bool(_) => Type::BOOL,
                    OwnedParam::I64(_) => Type::INT8,
                    OwnedParam::F64(_) => Type::FLOAT8,
                    OwnedParam::Text(_) => Type::TEXT,
                    OwnedParam::Blob(_) => Type::BYTEA,
                    // NULL and extended types (sent as text): let the server infer.
                    OwnedParam::Null => Type::UNKNOWN,
                };
                let dyn_ref: &(dyn ToSql + Sync) = p;
                (dyn_ref, ty)
            })
            .collect();

        let pg_rows = client
            .query_typed(sql, typed_params.as_slice())
            .await
            .map_err(PgError::Postgres)?;

        pg_rows
            .into_iter()
            .map(crate::types::pg_row_to_row)
            .collect()
    }

    /// Return column metadata for the given SQL without executing it.
    ///
    /// Uses `client.prepare(sql)` to obtain the server-reported column
    /// descriptions.  Each [`ColumnDescription`] carries the column name and
    /// PostgreSQL type name (e.g. `"int4"`, `"text"`, `"uuid"`).
    ///
    /// `nullable` is always `true`; the PostgreSQL extended-query protocol
    /// does not carry nullability information in statement descriptions.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Postgres`] if the server rejects the SQL (syntax
    /// error, unknown table, etc.).
    pub async fn describe(&self, sql: &str) -> Result<Vec<ColumnDescription>, PgError> {
        let client = self.inner.lock().await;
        let stmt = client.prepare(sql).await.map_err(PgError::Postgres)?;
        Ok(stmt
            .columns()
            .iter()
            .map(|col| ColumnDescription {
                name: col.name().to_string(),
                type_name: col.type_().name().to_string(),
                nullable: true,
            })
            .collect())
    }
}

// ── helper ────────────────────────────────────────────────────────────────────

/// Convert `oxisql_core` params to `OwnedParam` values and return both vecs so
/// the caller can build a `&[&(dyn ToSql + Sync)]` slice from the refs.
fn build_params(params: &[&dyn ToSqlValue]) -> Vec<OwnedParam> {
    params
        .iter()
        .map(|p| value_to_param(&p.to_value()))
        .collect()
}

fn owned_refs(owned: &[OwnedParam]) -> Vec<&(dyn ToSql + Sync)> {
    owned.iter().map(|p| p as &(dyn ToSql + Sync)).collect()
}

// ── Connection impl ───────────────────────────────────────────────────────────

#[async_trait]
impl Connection for PgConnection {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let client = self.inner.lock().await;
        let owned = build_params(params);
        let refs = owned_refs(&owned);
        client
            .execute(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let client = self.inner.lock().await;
        let owned = build_params(params);
        let refs = owned_refs(&owned);
        let pg_rows = client
            .query(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        // Acquire an owned guard so `PgTransaction` can be `'static`.
        let guard = Arc::clone(&self.inner).lock_owned().await;
        // BEGIN is issued via batch_execute so we have explicit transaction control.
        guard
            .batch_execute("BEGIN")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(Box::new(PgTransaction {
            guard: Some(guard),
            done: false,
        }))
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let client = self.inner.lock().await;
        client
            .batch_execute(sql)
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        // batch_execute does not return row counts
        Ok(0)
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        let client = self.inner.lock().await;
        client
            .simple_query("")
            .await
            .map_err(|e| OxiSqlError::Other(e.to_string()))?;
        Ok(())
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT table_name, table_schema, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY table_schema, table_name",
                &[],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("table_name")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let schema: Option<String> = row.try_get::<String>("table_schema").ok();
                let type_str: String = row
                    .try_get::<String>("table_type")
                    .unwrap_or_else(|_| "TABLE".to_string());
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
                "SELECT column_name, ordinal_position, data_type, is_nullable, column_default, \
                        character_maximum_length, numeric_precision, numeric_scale \
                 FROM information_schema.columns \
                 WHERE table_name = $1 \
                 ORDER BY ordinal_position",
                &[&table],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("column_name")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let ordinal: i64 = row
                    .try_get::<i64>("ordinal_position")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let data_type = row
                    .try_get::<String>("data_type")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let nullable_str = row
                    .try_get::<String>("is_nullable")
                    .unwrap_or_else(|_| "YES".to_string());
                let default = row.try_get::<String>("column_default").ok();
                let max_len: Option<i64> = row.try_get::<i64>("character_maximum_length").ok();
                let num_prec: Option<i64> = row.try_get::<i64>("numeric_precision").ok();
                let num_scale: Option<i64> = row.try_get::<i64>("numeric_scale").ok();
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
                "SELECT indexname, indexdef \
                 FROM pg_indexes \
                 WHERE tablename = $1 \
                 ORDER BY indexname",
                &[&table],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("indexname")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let def = row.try_get::<String>("indexdef").unwrap_or_default();
                let unique = def.contains("UNIQUE");
                let primary = name.ends_with("_pkey");
                // Extract columns from the index definition: content between
                // the last '(' and ')'.
                let columns = if let (Some(start), Some(end)) = (def.rfind('('), def.rfind(')')) {
                    def[start + 1..end]
                        .split(',')
                        .map(|s| s.trim().to_string())
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(IndexInfo {
                    name,
                    columns,
                    unique,
                    primary,
                })
            })
            .collect()
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        let rows = self
            .query(
                "SELECT tc.constraint_name, kcu.column_name, \
                        ccu.table_name AS foreign_table, \
                        ccu.column_name AS foreign_column \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                      ON tc.constraint_name = kcu.constraint_name \
                      AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                      ON ccu.constraint_name = tc.constraint_name \
                      AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_name = $1",
                &[&table],
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let constraint_name = row
                    .try_get::<String>("constraint_name")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let column = row
                    .try_get::<String>("column_name")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let foreign_table = row
                    .try_get::<String>("foreign_table")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let foreign_column = row
                    .try_get::<String>("foreign_column")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                Ok(ForeignKeyInfo {
                    constraint_name,
                    column,
                    foreign_table,
                    foreign_column,
                })
            })
            .collect()
    }

    /// Stream query results row-by-row from PostgreSQL.
    ///
    /// Acquires a lock on the client, runs `client.query()` (which fetches all
    /// rows in a single round-trip), then yields each converted row individually.
    /// This is equivalent to the default trait implementation but avoids going
    /// through `async_trait`'s boxing.
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

    /// Compile a SQL statement for repeated execution.
    ///
    /// The compiled [`Statement`] is cached by SQL text: subsequent calls with
    /// the same SQL return a clone of the cached handle without any server
    /// round-trip.
    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        // --- cache look-up (synchronous lock, never held across await) ---
        {
            let cache = self
                .stmt_cache
                .lock()
                .map_err(|_| OxiSqlError::Other("statement cache lock poisoned".into()))?;
            if let Some(stmt) = cache.get(sql) {
                return Ok(Box::new(PgPrepared {
                    client: Arc::clone(&self.inner),
                    stmt: stmt.clone(),
                    sql_text: sql.to_string(),
                }));
            }
        } // lock released before any await

        // --- cache miss: prepare on the server ---
        let stmt: Statement = {
            let client = self.inner.lock().await;
            client
                .prepare(sql)
                .await
                .map_err(|e| OxiSqlError::Execution(e.to_string()))?
        }; // client lock released

        // --- insert into cache ---
        {
            let mut cache = self
                .stmt_cache
                .lock()
                .map_err(|_| OxiSqlError::Other("statement cache lock poisoned".into()))?;
            cache.insert(sql.to_string(), stmt.clone());
        }

        Ok(Box::new(PgPrepared {
            client: Arc::clone(&self.inner),
            stmt,
            sql_text: sql.to_string(),
        }))
    }
}

// ── PgTransaction ─────────────────────────────────────────────────────────────

/// A PostgreSQL transaction that holds exclusive access to the client for the
/// duration of the transaction scope.
///
/// # Preferred cleanup
///
/// Call [`Transaction::commit`] or [`Transaction::rollback`] to end the
/// transaction explicitly.  If neither is called before this value is dropped,
/// the `Drop` implementation attempts to schedule a `ROLLBACK` on the current
/// Tokio runtime (best-effort).  If no runtime is reachable the connection is
/// simply released; the server will clean up via connection reset.
pub struct PgTransaction {
    /// `Some` until `commit`, `rollback`, or `Drop` consumes the guard.
    guard: Option<OwnedMutexGuard<tokio_postgres::Client>>,
    /// `true` once `commit` or `rollback` has been called.
    done: bool,
}

impl Drop for PgTransaction {
    fn drop(&mut self) {
        if let Some(guard) = self.guard.take() {
            if !self.done {
                // Best-effort: schedule ROLLBACK on the active Tokio runtime.
                // If no runtime is reachable, the guard drops here, releasing
                // the mutex so the connection becomes usable again.  The server
                // will roll back automatically on connection reset.
                match tokio::runtime::Handle::try_current() {
                    Ok(handle) => {
                        handle.spawn(async move {
                            let _ = guard.batch_execute("ROLLBACK").await;
                        });
                    }
                    Err(_) => {
                        // Not in a Tokio context — guard is dropped silently.
                        drop(guard);
                    }
                }
            }
        }
    }
}

/// Helper: borrow the active guard or return a "transaction consumed" error.
fn guard_ref(
    guard: &mut Option<OwnedMutexGuard<tokio_postgres::Client>>,
) -> Result<&mut OwnedMutexGuard<tokio_postgres::Client>, OxiSqlError> {
    guard.as_mut().ok_or_else(|| {
        OxiSqlError::Execution("transaction already consumed (committed or rolled back)".into())
    })
}

#[async_trait]
impl Transaction for PgTransaction {
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let owned = build_params(params);
        let refs = owned_refs(&owned);
        guard_ref(&mut self.guard)?
            .execute(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let owned = build_params(params);
        let refs = owned_refs(&owned);
        let pg_rows = guard_ref(&mut self.guard)?
            .query(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    async fn commit(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        self.done = true;
        let guard = self
            .guard
            .take()
            .ok_or_else(|| OxiSqlError::Execution("transaction already consumed".into()))?;
        guard
            .batch_execute("COMMIT")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        self.done = true;
        let guard = self
            .guard
            .take()
            .ok_or_else(|| OxiSqlError::Execution("transaction already consumed".into()))?;
        guard
            .batch_execute("ROLLBACK")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        guard_ref(&mut self.guard)?
            .batch_execute(&format!("SAVEPOINT {name}"))
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn release_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        guard_ref(&mut self.guard)?
            .batch_execute(&format!("RELEASE SAVEPOINT {name}"))
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        validate_savepoint_name(name)?;
        guard_ref(&mut self.guard)?
            .batch_execute(&format!("ROLLBACK TO SAVEPOINT {name}"))
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }
}

// ── PgTransaction — inherent savepoint/timeout API (returns PgError) ─────────

impl PgTransaction {
    /// Create a savepoint with the given name.
    ///
    /// The name must consist only of ASCII alphanumeric characters and
    /// underscores (`[a-zA-Z0-9_]`).  This restriction prevents SQL injection
    /// because savepoint names cannot be parameterized in the PostgreSQL
    /// wire protocol.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] if the name is empty or contains unsafe
    /// characters.  Returns [`PgError::Postgres`] on wire-level errors.
    pub async fn savepoint_pg(&mut self, name: &str) -> Result<(), PgError> {
        if !is_valid_savepoint_name(name) {
            return Err(PgError::Notify(format!(
                "invalid savepoint name {name:?}: only alphanumeric characters and underscores allowed"
            )));
        }
        let guard = self
            .guard
            .as_ref()
            .ok_or_else(|| PgError::Notify("transaction already consumed".into()))?;
        guard
            .batch_execute(&format!("SAVEPOINT {name}"))
            .await
            .map_err(PgError::Postgres)
    }

    /// Roll back to a savepoint without releasing it.
    ///
    /// The savepoint remains active after this call; you can roll back to it
    /// again or release it with [`PgTransaction::release_savepoint_pg`].
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] if the name is invalid or the transaction
    /// has already been consumed.  Returns [`PgError::Postgres`] on server
    /// errors (e.g. the named savepoint does not exist).
    pub async fn rollback_to_savepoint_pg(&mut self, name: &str) -> Result<(), PgError> {
        if !is_valid_savepoint_name(name) {
            return Err(PgError::Notify(format!(
                "invalid savepoint name {name:?}: only alphanumeric characters and underscores allowed"
            )));
        }
        let guard = self
            .guard
            .as_ref()
            .ok_or_else(|| PgError::Notify("transaction already consumed".into()))?;
        guard
            .batch_execute(&format!("ROLLBACK TO SAVEPOINT {name}"))
            .await
            .map_err(PgError::Postgres)
    }

    /// Release (remove) a savepoint.
    ///
    /// All savepoints created after the named savepoint are also removed.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Notify`] if the name is invalid or the transaction
    /// has already been consumed.  Returns [`PgError::Postgres`] on server
    /// errors (e.g. the named savepoint does not exist).
    pub async fn release_savepoint_pg(&mut self, name: &str) -> Result<(), PgError> {
        if !is_valid_savepoint_name(name) {
            return Err(PgError::Notify(format!(
                "invalid savepoint name {name:?}: only alphanumeric characters and underscores allowed"
            )));
        }
        let guard = self
            .guard
            .as_ref()
            .ok_or_else(|| PgError::Notify("transaction already consumed".into()))?;
        guard
            .batch_execute(&format!("RELEASE SAVEPOINT {name}"))
            .await
            .map_err(PgError::Postgres)
    }
}

/// Fast path: check if a savepoint name contains only safe SQL identifier chars.
///
/// Accepts `[a-zA-Z0-9_]` and rejects empty strings.
fn is_valid_savepoint_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate that a savepoint name contains only safe SQL identifier characters.
///
/// Accepts `[a-zA-Z0-9_]` — no spaces, no quotes, no SQL injection vectors.
fn validate_savepoint_name(name: &str) -> Result<(), OxiSqlError> {
    if name.is_empty() {
        return Err(OxiSqlError::Other(
            "savepoint name must not be empty".into(),
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(OxiSqlError::Other(format!(
            "invalid savepoint name {name:?}: only alphanumeric characters and underscores allowed"
        )));
    }
    Ok(())
}

// ── PgConnParts / parse_pg_conn_str ───────────────────────────────────────────

/// Parsed fields extracted from a PostgreSQL connection string.
///
/// Obtained via [`parse_pg_conn_str`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgConnParts {
    /// The host name or IP address (defaults to `"localhost"`).
    pub host: String,
    /// The server port (defaults to `5432`).
    pub port: u16,
    /// The target database name, if present.
    pub dbname: Option<String>,
    /// The login user name, if present.
    pub user: Option<String>,
}

/// Parse a PostgreSQL connection string into its constituent parts.
///
/// Accepts two common formats:
///
/// **Key-value format** (the `libpq`/`tokio-postgres` native format):
/// ```text
/// host=myhost port=5433 dbname=testdb user=admin password=secret
/// ```
/// Unknown keys are silently ignored.  Missing `host` defaults to
/// `"localhost"`; missing `port` defaults to `5432`.
///
/// **URI format**:
/// ```text
/// postgres://user:password@host:port/dbname
/// postgresql://host/dbname
/// ```
/// The scheme must be `postgres://` or `postgresql://`.
///
/// # Errors
///
/// Returns [`OxiSqlError::Other`] when:
/// - A non-empty `port` value cannot be parsed as a `u16`.
/// - The URI scheme is unrecognised (neither `postgres://` nor `postgresql://`).
///
/// # Examples
///
/// ```rust
/// use oxisql_postgres::parse_pg_conn_str;
///
/// let p = parse_pg_conn_str("host=myhost port=5433 dbname=testdb user=admin").unwrap();
/// assert_eq!(p.host, "myhost");
/// assert_eq!(p.port, 5433);
/// assert_eq!(p.dbname, Some("testdb".to_string()));
/// assert_eq!(p.user, Some("admin".to_string()));
/// ```
pub fn parse_pg_conn_str(conn_str: &str) -> Result<PgConnParts, OxiSqlError> {
    let trimmed = conn_str.trim();

    if trimmed.starts_with("postgres://") || trimmed.starts_with("postgresql://") {
        parse_pg_uri(trimmed)
    } else {
        parse_pg_kv(trimmed)
    }
}

/// Parse the `key=value …` form of a Postgres connection string.
fn parse_pg_kv(conn_str: &str) -> Result<PgConnParts, OxiSqlError> {
    let mut parts = PgConnParts {
        host: "localhost".to_string(),
        port: 5432,
        dbname: None,
        user: None,
    };
    for token in conn_str.split_whitespace() {
        if let Some((k, v)) = token.split_once('=') {
            match k {
                "host" | "hostaddr" => parts.host = v.to_string(),
                "port" if !v.is_empty() => {
                    parts.port = v.parse::<u16>().map_err(|_| {
                        OxiSqlError::Other(format!(
                            "invalid port value in connection string: {v:?}"
                        ))
                    })?;
                }
                "dbname" => parts.dbname = Some(v.to_string()),
                "user" => parts.user = Some(v.to_string()),
                _ => {}
            }
        }
    }
    Ok(parts)
}

/// Parse the `postgres://[user[:pass]@]host[:port][/dbname]` URI form.
fn parse_pg_uri(uri: &str) -> Result<PgConnParts, OxiSqlError> {
    // Strip scheme.
    let rest = uri
        .strip_prefix("postgresql://")
        .or_else(|| uri.strip_prefix("postgres://"))
        .ok_or_else(|| {
            OxiSqlError::Other(format!("unrecognised connection string scheme: {uri:?}"))
        })?;

    // rest = [user[:pass]@]host[:port][/dbname][?options]
    // Split off query string first.
    let (rest, _query) = rest.split_once('?').unwrap_or((rest, ""));

    // Split user-info from host-info at the last '@'.
    let (user_info, host_part) = if let Some(at_pos) = rest.rfind('@') {
        (Some(&rest[..at_pos]), &rest[at_pos + 1..])
    } else {
        (None, rest)
    };

    // Extract user from user_info (ignore password).
    let user = user_info.and_then(|ui| {
        let cred = ui.split_once(':').map(|(u, _)| u).unwrap_or(ui);
        if cred.is_empty() {
            None
        } else {
            Some(cred.to_string())
        }
    });

    // Split path (dbname) from host[:port].
    let (authority, dbname) = if let Some(slash_pos) = host_part.find('/') {
        let db = &host_part[slash_pos + 1..];
        (
            &host_part[..slash_pos],
            if db.is_empty() {
                None
            } else {
                Some(db.to_string())
            },
        )
    } else {
        (host_part, None)
    };

    // Split host and port.
    let (host, port) = if authority.starts_with('[') {
        // IPv6 literal: [::1]:5432
        if let Some(bracket_end) = authority.find(']') {
            let h = &authority[1..bracket_end];
            let port_str = authority[bracket_end + 1..].trim_start_matches(':');
            let p = if port_str.is_empty() {
                5432
            } else {
                port_str
                    .parse::<u16>()
                    .map_err(|_| OxiSqlError::Other(format!("invalid port in URI: {port_str:?}")))?
            };
            (h.to_string(), p)
        } else {
            (authority.to_string(), 5432)
        }
    } else if let Some(colon_pos) = authority.rfind(':') {
        let h = &authority[..colon_pos];
        let port_str = &authority[colon_pos + 1..];
        let p = if port_str.is_empty() {
            5432
        } else {
            port_str
                .parse::<u16>()
                .map_err(|_| OxiSqlError::Other(format!("invalid port in URI: {port_str:?}")))?
        };
        (h.to_string(), p)
    } else {
        (authority.to_string(), 5432)
    };

    let host = if host.is_empty() {
        "localhost".to_string()
    } else {
        host
    };

    Ok(PgConnParts {
        host,
        port,
        dbname,
        user,
    })
}
