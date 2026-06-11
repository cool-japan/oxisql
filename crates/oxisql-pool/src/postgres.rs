//! PostgreSQL connection pool backed by `deadpool-postgres`.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use deadpool_postgres::{Config, Runtime};
//! use oxisql_pool::postgres::OxidbPgPool;
//!
//! let mut cfg = Config::new();
//! cfg.host = Some("localhost".to_string());
//! cfg.dbname = Some("mydb".to_string());
//! cfg.user = Some("postgres".to_string());
//!
//! let pool = OxidbPgPool::new(cfg, Runtime::Tokio1)?;
//! let client = pool.get().await?;
//! // use client as a tokio_postgres Client
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use deadpool_postgres::{Client, Config, CreatePoolError, PoolError, Runtime};
use oxisql_core::{
    ColumnInfo, Connection, ConnectionPool, ForeignKeyInfo, IndexInfo, OxiSqlError,
    PreparedStatement, Row, TableInfo, TableType, ToSqlValue, Transaction,
};
use oxisql_postgres::types::{pg_row_to_row, value_to_param, OwnedParam};
use tokio_postgres::Statement;

// ── helper: build owned params ────────────────────────────────────────────────

fn build_pg_params(params: &[&dyn ToSqlValue]) -> Vec<OwnedParam> {
    params
        .iter()
        .map(|p| value_to_param(&p.to_value()))
        .collect()
}

fn pg_param_refs(owned: &[OwnedParam]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    owned.iter().map(|p| p as _).collect()
}

// ── PgPooledPrepared ──────────────────────────────────────────────────────────

/// A prepared statement bound to a borrowed `tokio_postgres::Client`.
///
/// The lifetime `'a` is tied to the [`PgPooledConn`] that produced this
/// statement, ensuring the connection outlives all derived prepared statements.
pub struct PgPooledPrepared<'a> {
    client: &'a tokio_postgres::Client,
    stmt: Statement,
    sql_text: String,
}

#[async_trait]
impl<'a> PreparedStatement for PgPooledPrepared<'a> {
    /// Execute the prepared statement and return the rows-affected count.
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        self.client
            .execute(&self.stmt, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    /// Execute the prepared statement as a `SELECT` and return all result rows.
    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        let pg_rows = self
            .client
            .query(&self.stmt, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Return the original SQL text used to compile this statement.
    fn sql(&self) -> &str {
        &self.sql_text
    }
}

// ── PgPooledTxn ───────────────────────────────────────────────────────────────

/// A PostgreSQL transaction borrowed from a [`PgPooledConn`].
///
/// The transaction is begun with `BEGIN` when [`PgPooledConn::transaction`] is
/// called.  Callers must call [`commit`](Transaction::commit) or
/// [`rollback`](Transaction::rollback) explicitly.
///
/// # Drop behaviour
///
/// On drop without an explicit commit, the connection is recycled by deadpool.
/// The PostgreSQL server rolls back any open transaction when the idle connection
/// is reused or the server's idle-connection cleanup fires.
pub struct PgPooledTxn<'a> {
    client: &'a tokio_postgres::Client,
}

#[async_trait]
impl<'a> Transaction for PgPooledTxn<'a> {
    /// Execute a DML/DDL statement within the transaction.
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        self.client
            .execute(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    /// Execute a `SELECT` statement within the transaction.
    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        let pg_rows = self
            .client
            .query(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Commit the transaction.
    async fn commit(self: Box<Self>) -> Result<(), OxiSqlError> {
        self.client
            .batch_execute("COMMIT")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    /// Roll back the transaction.
    async fn rollback(self: Box<Self>) -> Result<(), OxiSqlError> {
        self.client
            .batch_execute("ROLLBACK")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }
}

// ── PgPooledConn ─────────────────────────────────────────────────────────────

/// A pooled PostgreSQL connection checked out from [`OxidbPgPool`].
///
/// The inner [`deadpool_postgres::Client`] is a `deadpool::managed::Object<Manager>`
/// that derefs (via `ClientWrapper`) to `tokio_postgres::Client` and returns itself
/// to the pool when dropped.
pub struct PgPooledConn {
    /// The checked-out connection object.  Returned to the pool on `Drop`.
    obj: Client,
}

#[async_trait]
impl Connection for PgPooledConn {
    /// Execute a DML/DDL statement and return the rows-affected count.
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        self.obj
            .execute(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    /// Execute a `SELECT` and return all result rows.
    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let owned = build_pg_params(params);
        let refs = pg_param_refs(&owned);
        let pg_rows = self
            .obj
            .query(sql, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Begin a transaction on this pooled connection.
    ///
    /// Issues `BEGIN` via the simple-query protocol.  The returned
    /// [`PgPooledTxn`] borrows `&self`; the borrow is released when the
    /// transaction is committed or rolled back.
    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        // Deref chain: Client (Object<Manager>) → ClientWrapper → tokio_postgres::Client
        // Auto-deref coerces &deadpool_postgres::Client → &tokio_postgres::Client.
        let pg_client: &tokio_postgres::Client = &self.obj;
        pg_client
            .batch_execute("BEGIN")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(Box::new(PgPooledTxn { client: pg_client }))
    }

    /// Execute multiple semicolon-separated SQL statements in one call.
    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        self.obj
            .batch_execute(sql)
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(0)
    }

    /// Lightweight connectivity check via an empty simple-query.
    async fn ping(&self) -> Result<(), OxiSqlError> {
        self.obj
            .simple_query("")
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(())
    }

    /// Compile a SQL statement for repeated execution.
    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        // Auto-deref coerces &deadpool_postgres::Client → &tokio_postgres::Client.
        let pg_client: &tokio_postgres::Client = &self.obj;
        let stmt = pg_client
            .prepare(sql)
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(Box::new(PgPooledPrepared {
            client: pg_client,
            stmt,
            sql_text: sql.to_string(),
        }))
    }

    /// List all user tables visible to the current connection.
    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        let pg_rows = self
            .obj
            .query(
                "SELECT table_name, table_schema, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY table_schema, table_name",
                &[],
            )
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        let rows: Vec<Row> = pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect::<Result<Vec<_>, _>>()?;
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

    /// List all columns of the named table.
    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        let table_param = OwnedParam::Text(table.to_string());
        let pg_rows = self
            .obj
            .query(
                "SELECT column_name, ordinal_position, data_type, is_nullable, column_default, \
                        character_maximum_length, numeric_precision, numeric_scale \
                 FROM information_schema.columns \
                 WHERE table_name = $1 \
                 ORDER BY ordinal_position",
                &[&table_param as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        let rows: Vec<Row> = pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect::<Result<Vec<_>, _>>()?;
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

    /// List all indexes on the named table.
    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        let table_param = OwnedParam::Text(table.to_string());
        let pg_rows = self
            .obj
            .query(
                "SELECT indexname, indexdef \
                 FROM pg_indexes \
                 WHERE tablename = $1 \
                 ORDER BY indexname",
                &[&table_param as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        let rows: Vec<Row> = pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|row| {
                let name = row
                    .try_get::<String>("indexname")
                    .map_err(|e| OxiSqlError::Other(e.to_string()))?;
                let def = row.try_get::<String>("indexdef").unwrap_or_default();
                let unique = def.contains("UNIQUE");
                let primary = name.ends_with("_pkey");
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

    /// List all foreign-key constraints on the named table.
    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        let table_param = OwnedParam::Text(table.to_string());
        let pg_rows = self
            .obj
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
                &[&table_param as &(dyn tokio_postgres::types::ToSql + Sync)],
            )
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        let rows: Vec<Row> = pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect::<Result<Vec<_>, _>>()?;
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
                    ..Default::default()
                })
            })
            .collect()
    }
}

// ── OxidbPgPool ──────────────────────────────────────────────────────────────

/// A thin newtype wrapper around [`deadpool_postgres::Pool`].
///
/// Construct via [`OxidbPgPool::new`], then call [`OxidbPgPool::get`] to
/// check out a pooled [`Client`].
///
/// `Clone` is inexpensive — the inner [`deadpool_postgres::Pool`] is backed
/// by an `Arc`, so cloning only increments the reference count.
#[derive(Clone)]
pub struct OxidbPgPool(deadpool_postgres::Pool);

impl OxidbPgPool {
    /// Create a new pool from a `deadpool_postgres::Config`.
    ///
    /// Pass `Runtime::Tokio1` when running under a Tokio executor.
    ///
    /// # Errors
    ///
    /// Returns [`CreatePoolError`] if the configuration is invalid.
    pub fn new(config: Config, runtime: Runtime) -> Result<Self, CreatePoolError> {
        let pool = config.create_pool(Some(runtime), tokio_postgres::NoTls)?;
        Ok(Self(pool))
    }

    /// Check out a raw `deadpool_postgres::Client` from the pool.
    ///
    /// Waits until a connection is available (or the pool creates a new one
    /// up to its `max_size` limit).
    ///
    /// # Errors
    ///
    /// Returns [`PoolError`] if all connections are exhausted, the pool is
    /// closed, or the underlying `tokio-postgres` connection fails.
    pub async fn get(&self) -> Result<Client, PoolError> {
        self.0.get().await
    }

    /// Return the maximum number of connections in the pool.
    pub fn max_size(&self) -> usize {
        self.0.status().max_size
    }

    /// Return the number of connections currently available (idle) in the
    /// pool.
    pub fn available(&self) -> usize {
        self.0.status().available
    }

    /// Return the name of the backend powering this pool.
    pub fn backend_name(&self) -> &'static str {
        "postgres"
    }

    /// Close the pool.
    ///
    /// Dropping the pool also releases all connections; this method is provided
    /// for API symmetry with other backends.  The underlying deadpool pool is
    /// closed and no new connections will be handed out after this call.
    pub fn close(&self) {
        self.0.close();
    }

    /// Verify the pool is healthy by acquiring a connection and issuing a
    /// lightweight `SELECT 1` query.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError`] if the connection cannot be checked out or
    /// the query fails.
    pub async fn health_check(&self) -> Result<(), crate::PoolError> {
        let client = self.0.get().await?;
        client
            .simple_query("SELECT 1")
            .await
            .map_err(|e| crate::PoolError::Build(e.to_string()))?;
        Ok(())
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

    /// Create a pool from a `postgres://` or `postgresql://` URL.
    ///
    /// The URL is forwarded to `deadpool_postgres` as the `url` config field,
    /// which delegates parsing to `tokio_postgres`.
    ///
    /// Pool construction does **not** open any connections; the first error from
    /// the database will appear on the first [`OxidbPgPool::get`] call.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError::CreatePool`] if the URL or configuration is
    /// invalid.
    pub fn try_from_url(url: &str) -> Result<Self, crate::PoolError> {
        let mut cfg = Config::new();
        cfg.url = Some(url.to_string());
        cfg.create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
            .map(Self)
            .map_err(crate::PoolError::CreatePool)
    }
}

impl TryFrom<Config> for OxidbPgPool {
    type Error = crate::PoolError;

    /// Create an [`OxidbPgPool`] from a `deadpool_postgres::Config`.
    ///
    /// Uses [`Runtime::Tokio1`] automatically — suitable for any Tokio executor.
    /// Connections use [`tokio_postgres::NoTls`]; for TLS construct the pool
    /// via [`OxidbPgPool::new`] with a custom TLS connector.
    ///
    /// Pool construction does **not** open any connections; the first error from
    /// the database will appear on the first [`OxidbPgPool::get`] call.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError::CreatePool`] if the configuration is invalid
    /// (e.g. `max_size` is zero or a mandatory field combination is rejected by
    /// `deadpool_postgres`).
    fn try_from(config: Config) -> Result<Self, Self::Error> {
        let pool = config
            .create_pool(Some(Runtime::Tokio1), tokio_postgres::NoTls)
            .map_err(crate::PoolError::CreatePool)?;
        Ok(Self(pool))
    }
}

// ── ConnectionPool impl ───────────────────────────────────────────────────────

#[async_trait]
impl ConnectionPool for OxidbPgPool {
    /// Check out a connection from the pool as a [`Box<dyn Connection>`].
    ///
    /// The connection is automatically returned to the pool when the returned
    /// box is dropped.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::ConnectionPool`] if the pool is exhausted, closed,
    /// or the backend connection fails.
    async fn get(&self) -> Result<Box<dyn Connection + Send>, OxiSqlError> {
        let obj = self
            .0
            .get()
            .await
            .map_err(|e| OxiSqlError::ConnectionPool(e.to_string()))?;
        Ok(Box::new(PgPooledConn { obj }))
    }

    /// Maximum number of connections the pool will hold.
    fn pool_size(&self) -> usize {
        self.0.status().max_size
    }

    /// Number of connections currently idle (available for checkout).
    fn idle_count(&self) -> usize {
        self.0.status().available
    }

    /// Number of connections currently checked out (active).
    fn active_count(&self) -> usize {
        let s = self.0.status();
        s.size.saturating_sub(s.available)
    }

    /// Verify that the pool is healthy by probing the backend.
    ///
    /// Checks out a connection and issues a lightweight empty simple-query.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::ConnectionPool`] if the connection cannot be
    /// checked out or the ping fails.
    async fn health_check(&self) -> Result<(), OxiSqlError> {
        let conn = ConnectionPool::get(self).await?;
        conn.ping().await?;
        Ok(())
    }

    /// Drain all idle connections and prevent new checkouts.
    async fn close(&self) {
        self.0.close();
    }
}
