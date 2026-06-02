//! Query logging middleware for the OxiSQL facade.
//!
//! Provides [`LoggingConnection`], a labelled wrapper around any
//! `Box<dyn Connection>` that logs every SQL operation (with timing)
//! using the [`log`] crate.
//!
//! This is the facade-level counterpart to the generic
//! [`oxisql_core::middleware::LoggingConnection`].  The two types have
//! different trade-offs:
//!
//! - `oxisql_core::middleware::LoggingConnection<C>` — generic, zero-cost
//!   wrapper, no heap allocation for the inner connection.
//! - `oxisql::logging::LoggingConnection` — type-erased (`Box<dyn
//!   Connection>`), carrying an explicit string `label` for identifying
//!   the connection site in logs.  This is convenient when you receive a
//!   boxed connection from [`crate::connect`] and want to wrap it without
//!   knowing the concrete backend type.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql::OxiSqlError> {
//! use oxisql::Connection;
//! use oxisql::logging::LoggingConnection;
//!
//! let inner = oxisql::connect("memory://").await?;
//! let conn = LoggingConnection::new(inner, "my_connection");
//! conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;
//! // Logs: [DEBUG] [my_connection] execute: "CREATE TABLE t (id INTEGER)" → 0 rows, 1.23ms
//! # Ok(())
//! # }
//! ```

use std::pin::Pin;

use futures::Stream;
use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    TableInfo, ToSqlValue, Transaction,
};

/// A labelled [`Connection`] wrapper that logs all queries with timing.
///
/// Wraps any `Box<dyn Connection>` and logs every SQL operation at `DEBUG`
/// level (on success) or `WARN` level (on error).  The `label` is prepended
/// to every log line so you can distinguish multiple connections in the same
/// process.
///
/// # Log format
///
/// ```text
/// [DEBUG] [<label>] execute: "<sql>" → <n> rows, <elapsed>
/// [WARN]  [<label>] execute ERROR: "<sql>" → <error>, <elapsed>
/// [DEBUG] [<label>] query: "<sql>" → <n> rows, <elapsed>
/// [WARN]  [<label>] query ERROR: "<sql>" → <error>, <elapsed>
/// ```
///
/// # Example
///
/// ```rust,no_run
/// # #[tokio::main]
/// # async fn main() -> Result<(), oxisql::OxiSqlError> {
/// use oxisql::Connection;
/// use oxisql::logging::LoggingConnection;
///
/// let inner = oxisql::connect("memory://").await?;
/// let conn = LoggingConnection::new(inner, "my_connection");
/// conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;
/// # Ok(())
/// # }
/// ```
pub struct LoggingConnection {
    inner: Box<dyn Connection>,
    label: String,
}

impl LoggingConnection {
    /// Wrap `inner` with a `label` that is prepended to every log line.
    pub fn new(inner: Box<dyn Connection>, label: impl Into<String>) -> Self {
        Self {
            inner,
            label: label.into(),
        }
    }

    /// Return the label associated with this connection.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Consume the wrapper and return the inner connection.
    pub fn into_inner(self) -> Box<dyn Connection> {
        self.inner
    }
}

#[async_trait::async_trait]
impl Connection for LoggingConnection {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let start = std::time::Instant::now();
        let result = self.inner.execute(sql, params).await;
        let elapsed = start.elapsed();
        match &result {
            Ok(n) => log::debug!(
                "[{}] execute: {:?} \u{2192} {} rows, {:?}",
                self.label,
                sql,
                n,
                elapsed,
            ),
            Err(e) => log::warn!(
                "[{}] execute ERROR: {:?} \u{2192} {:?}, {:?}",
                self.label,
                sql,
                e,
                elapsed,
            ),
        }
        result
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let start = std::time::Instant::now();
        let result = self.inner.query(sql, params).await;
        let elapsed = start.elapsed();
        match &result {
            Ok(rows) => log::debug!(
                "[{}] query: {:?} \u{2192} {} rows, {:?}",
                self.label,
                sql,
                rows.len(),
                elapsed,
            ),
            Err(e) => log::warn!(
                "[{}] query ERROR: {:?} \u{2192} {:?}, {:?}",
                self.label,
                sql,
                e,
                elapsed,
            ),
        }
        result
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        log::debug!("[{}] transaction: BEGIN", self.label);
        self.inner.transaction().await
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let start = std::time::Instant::now();
        let result = self.inner.execute_batch(sql).await;
        let elapsed = start.elapsed();
        match &result {
            Ok(n) => log::debug!(
                "[{}] execute_batch: \u{2192} {} rows total, {:?}",
                self.label,
                n,
                elapsed,
            ),
            Err(e) => log::warn!(
                "[{}] execute_batch ERROR: \u{2192} {:?}, {:?}",
                self.label,
                e,
                elapsed,
            ),
        }
        result
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        log::debug!("[{}] ping", self.label);
        self.inner.ping().await
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        log::debug!("[{}] prepare: {:?}", self.label, sql);
        self.inner.prepare(sql).await
    }

    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        self.inner.tables().await
    }

    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        self.inner.columns(table).await
    }

    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        self.inner.indexes(table).await
    }

    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        self.inner.foreign_keys(table).await
    }

    fn query_stream<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a dyn ToSqlValue],
    ) -> Pin<Box<dyn Stream<Item = Result<Row, OxiSqlError>> + Send + 'a>> {
        log::debug!("[{}] query_stream: {:?}", self.label, sql);
        self.inner.query_stream(sql, params)
    }
}
