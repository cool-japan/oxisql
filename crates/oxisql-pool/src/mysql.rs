//! MySQL connection pool via a custom [`deadpool::managed::Manager`] over
//! `mysql_async::Conn`.
//!
//! `deadpool-mysql` does not exist on crates.io; this module implements the
//! `Manager` trait manually so that `deadpool` manages raw `mysql_async::Conn`
//! objects rather than a higher-level `mysql_async::Pool`.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_pool::mysql::new_mysql_pool;
//!
//! let pool = new_mysql_pool("mysql://root:secret@localhost:3306/mydb", 8)?;
//! let mut conn = pool.get().await?;
//! // use conn as a mysql_async::Conn
//! # Ok(())
//! # }
//! ```

use async_trait::async_trait;
use deadpool::managed::{Manager, Metrics, RecycleError, RecycleResult};
use mysql_async::{prelude::Queryable, Conn, Opts};
use oxisql_core::{
    ColumnInfo, Connection, ConnectionPool, ForeignKeyInfo, IndexInfo, OxiSqlError,
    PreparedStatement, Row, TableInfo, TableType, ToSqlValue, Transaction,
};
use oxisql_mysql::{
    connection::core_params_to_mysql, error::classify_mysql_error, types::mysql_row_to_core,
};
use tokio::sync::Mutex as TokioMutex;

// ── MysqlManager ─────────────────────────────────────────────────────────────

/// A `deadpool::managed::Manager` that creates and recycles `mysql_async::Conn`
/// objects.
///
/// Construct via [`MysqlManager::new`] and pass to
/// [`deadpool::managed::Pool::builder`].
pub struct MysqlManager {
    opts: Opts,
}

impl MysqlManager {
    /// Parse a MySQL URL and build the manager.
    ///
    /// # Errors
    ///
    /// Returns [`mysql_async::UrlError`] if the URL is malformed.
    pub fn new(url: &str) -> Result<Self, mysql_async::UrlError> {
        let opts = url.parse::<Opts>()?;
        Ok(Self { opts })
    }
}

impl Manager for MysqlManager {
    type Type = Conn;
    type Error = mysql_async::Error;

    async fn create(&self) -> Result<Conn, mysql_async::Error> {
        Conn::new(self.opts.clone()).await
    }

    async fn recycle(&self, conn: &mut Conn, _: &Metrics) -> RecycleResult<mysql_async::Error> {
        // Execute a lightweight query to verify the connection is still alive.
        conn.query_iter("SELECT 1")
            .await
            .map_err(RecycleError::Backend)?
            .drop_result()
            .await
            .map_err(RecycleError::Backend)
    }
}

// ── MysqlPooledTxn ────────────────────────────────────────────────────────────

/// A MySQL transaction running on a borrowed [`MysqlPooledConn`].
///
/// The transaction is begun with `BEGIN` when [`MysqlPooledConn::transaction`]
/// is called.  Callers must call [`commit`](Transaction::commit) or
/// [`rollback`](Transaction::rollback) explicitly.
///
/// # Drop behaviour
///
/// On drop without an explicit commit or rollback, the connection is recycled
/// by deadpool.  MySQL server rolls back the implicit transaction when the
/// underlying connection is returned to the pool and the next statement is run.
pub struct MysqlPooledTxn<'a> {
    conn: &'a TokioMutex<deadpool::managed::Object<MysqlManager>>,
}

#[async_trait]
impl<'a> Transaction for MysqlPooledTxn<'a> {
    /// Execute a DML/DDL statement within the transaction.
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mut guard = self.conn.lock().await;
        let mysql_params = core_params_to_mysql(params);
        let result = guard
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

    /// Execute a `SELECT` statement within the transaction.
    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let mut guard = self.conn.lock().await;
        let mysql_params = core_params_to_mysql(params);
        let mysql_rows: Vec<mysql_async::Row> = guard
            .exec(sql, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
        mysql_rows
            .into_iter()
            .map(|r| mysql_row_to_core(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Commit the transaction.
    async fn commit(self: Box<Self>) -> Result<(), OxiSqlError> {
        let mut guard = self.conn.lock().await;
        guard
            .query_drop("COMMIT")
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))
    }

    /// Roll back the transaction.
    async fn rollback(self: Box<Self>) -> Result<(), OxiSqlError> {
        let mut guard = self.conn.lock().await;
        guard
            .query_drop("ROLLBACK")
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))
    }
}

// ── MysqlPooledConn ───────────────────────────────────────────────────────────

/// A pooled MySQL connection checked out from [`MysqlPool`].
///
/// `mysql_async::Conn` requires `&mut self` for all query operations.
/// The inner connection is therefore wrapped in a [`TokioMutex`] so that the
/// `Connection` trait (which provides `&self`) can take a mutable lock on demand.
pub struct MysqlPooledConn {
    /// Mutex-wrapped pool object.  Returned to the pool when `obj` is dropped.
    obj: TokioMutex<deadpool::managed::Object<MysqlManager>>,
}

#[async_trait]
impl Connection for MysqlPooledConn {
    /// Execute a DML/DDL statement and return the rows-affected count.
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mut guard = self.obj.lock().await;
        let mysql_params = core_params_to_mysql(params);
        let result = guard
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

    /// Execute a `SELECT` and return all result rows.
    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let mut guard = self.obj.lock().await;
        let mysql_params = core_params_to_mysql(params);
        let mysql_rows: Vec<mysql_async::Row> = guard
            .exec(sql, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
        mysql_rows
            .into_iter()
            .map(|r| mysql_row_to_core(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Begin a transaction on this pooled connection.
    ///
    /// Issues `BEGIN` by locking the mutex and sending the statement.
    /// The returned [`MysqlPooledTxn`] borrows `&self.obj` (the `TokioMutex`).
    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        {
            let mut guard = self.obj.lock().await;
            guard
                .query_drop("BEGIN")
                .await
                .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
        }
        Ok(Box::new(MysqlPooledTxn { conn: &self.obj }))
    }

    /// Execute multiple semicolon-separated SQL statements.
    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let mut guard = self.obj.lock().await;
        let mut total = 0u64;
        for stmt in sql.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                let result = guard
                    .exec_iter(trimmed, mysql_async::Params::Empty)
                    .await
                    .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
                total += result.affected_rows();
                result
                    .drop_result()
                    .await
                    .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))?;
            }
        }
        Ok(total)
    }

    /// Lightweight connectivity check via `COM_PING`.
    ///
    /// `mysql_async::Queryable::ping` takes `&mut self` and returns
    /// `BoxFuture<'_, ()>` which resolves to `Result<(), mysql_async::Error>`.
    async fn ping(&self) -> Result<(), OxiSqlError> {
        let mut guard = self.obj.lock().await;
        guard
            .ping()
            .await
            .map_err(|e| OxiSqlError::from(classify_mysql_error(e)))
    }

    /// Compile a SQL statement for repeated execution.
    ///
    /// # Note
    ///
    /// MySQL statement IDs are connection-local and `MySqlPrepared` requires an
    /// owned `mysql_async::Conn`.  Because `MysqlPooledConn` owns its connection
    /// behind a `Mutex`, the connection cannot be moved into `MySqlPrepared`.
    /// This returns an error indicating that prepared statements on pooled
    /// connections require a dedicated connection; use
    /// `oxisql_mysql::MyConnection::prepare` instead.
    ///
    /// This is a known limitation and is tracked as a future improvement.
    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        let _ = sql;
        Err(OxiSqlError::Other(
            "prepared statements on pooled MySQL connections require a dedicated connection; \
             use oxisql_mysql::MyConnection::prepare instead"
                .to_string(),
        ))
    }

    /// List all tables in the current database.
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

    /// List all columns of the named table.
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

    /// List all indexes defined on the named table.
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
        let mut index_map: std::collections::HashMap<String, (bool, bool, Vec<String>)> =
            std::collections::HashMap::new();
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

    /// List all foreign-key constraints on the named table.
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
}

// ── MysqlPool ─────────────────────────────────────────────────────────────────

/// A thin newtype wrapper around the deadpool `Pool` for [`MysqlManager`].
///
/// Construct via [`new_mysql_pool`], then call [`MysqlPool::get`] to check
/// out a pooled [`Conn`].
pub struct MysqlPool(pub deadpool::managed::Pool<MysqlManager>);

impl MysqlPool {
    /// Check out a raw connection from the pool.
    ///
    /// # Errors
    ///
    /// Returns a [`deadpool::managed::PoolError`] if the pool is exhausted,
    /// closed, or the backend connection fails.
    pub async fn get(
        &self,
    ) -> Result<
        deadpool::managed::Object<MysqlManager>,
        deadpool::managed::PoolError<mysql_async::Error>,
    > {
        self.0.get().await
    }

    /// Return the name of the backend powering this pool.
    pub fn backend_name(&self) -> &'static str {
        "mysql"
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

    /// Verify the pool is healthy by checking out a connection and running
    /// a lightweight `SELECT 1` query.
    ///
    /// # Errors
    ///
    /// Returns [`crate::PoolError`] if the connection cannot be checked out or
    /// the query fails.
    pub async fn health_check(&self) -> Result<(), crate::PoolError> {
        let mut conn = self.0.get().await.map_err(crate::PoolError::Mysql)?;
        conn.query_iter("SELECT 1")
            .await
            .map_err(|e| crate::PoolError::Build(e.to_string()))?
            .drop_result()
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
}

// ── ConnectionPool impl ───────────────────────────────────────────────────────

#[async_trait]
impl ConnectionPool for MysqlPool {
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
        Ok(Box::new(MysqlPooledConn {
            obj: TokioMutex::new(obj),
        }))
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
    /// Checks out a connection and issues `COM_PING`.
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

// ── Pool constructor ──────────────────────────────────────────────────────────

/// Create a new [`MysqlPool`] with the given URL and `max_size`.
///
/// # Errors
///
/// Returns [`crate::PoolError::MysqlUrl`] if the URL is invalid, or
/// [`crate::PoolError::Build`] if pool construction fails (only occurs on a
/// mis-configured `max_size` of 0).
pub fn new_mysql_pool(url: &str, max_size: usize) -> Result<MysqlPool, crate::PoolError> {
    let manager = MysqlManager::new(url).map_err(crate::PoolError::MysqlUrl)?;
    deadpool::managed::Pool::builder(manager)
        .max_size(max_size)
        .build()
        .map(MysqlPool)
        .map_err(|e| crate::PoolError::Build(e.to_string()))
}
