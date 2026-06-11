//! Core database traits: [`Connection`], [`Transaction`], and [`ToSqlValue`].

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::prepare::PreparedStatement;
use crate::schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableInfo};
use crate::{OxiSqlError, Row, SqlWarning, Value};

// ── ToSqlValue ──────────────────────────────────────────────────────────────

/// A value that can be used as a positional SQL parameter (`$1`, `$2`, …).
pub trait ToSqlValue: Send + Sync {
    /// Convert `self` into a [`Value`].
    fn to_value(&self) -> Value;
}

impl ToSqlValue for i64 {
    fn to_value(&self) -> Value {
        Value::I64(*self)
    }
}

impl ToSqlValue for i32 {
    fn to_value(&self) -> Value {
        Value::I64(i64::from(*self))
    }
}

impl ToSqlValue for f64 {
    fn to_value(&self) -> Value {
        Value::F64(*self)
    }
}

impl ToSqlValue for str {
    fn to_value(&self) -> Value {
        Value::Text(self.to_string())
    }
}

impl ToSqlValue for String {
    fn to_value(&self) -> Value {
        Value::Text(self.clone())
    }
}

impl ToSqlValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

impl ToSqlValue for Vec<u8> {
    fn to_value(&self) -> Value {
        Value::Blob(self.clone())
    }
}

impl<T: ToSqlValue> ToSqlValue for Option<T> {
    fn to_value(&self) -> Value {
        match self {
            Some(v) => v.to_value(),
            None => Value::Null,
        }
    }
}

/// Blanket impl: any shared reference to a [`ToSqlValue`] is also a
/// [`ToSqlValue`].  This makes `&"hello"` (i.e. `&&str`) and `&&42_i64`
/// work as parameter values without needing per-reference-depth impls.
impl<T: ToSqlValue + ?Sized> ToSqlValue for &T {
    fn to_value(&self) -> Value {
        (**self).to_value()
    }
}

/// Allow a [`Value`] to be used directly as a SQL parameter.
///
/// This enables fan-out helpers (e.g. `MultiConnection`) to collect
/// parameter snapshots as `Vec<Value>` and pass them back to
/// `Connection::execute` / `Connection::query` without an intermediate
/// conversion step.
impl ToSqlValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

// ── Connection trait ────────────────────────────────────────────────────────

/// An async database connection.
///
/// Implementations must be `Send + Sync` so they can be shared across async
/// tasks.  Use [`transaction`](Connection::transaction) to obtain a transaction
/// that provides serialised, rollbackable execution.
#[async_trait]
pub trait Connection: Send + Sync {
    /// Execute a DML/DDL statement and return the number of rows affected.
    ///
    /// Positional parameters (`$1`, `$2`, …) are substituted from `params`.
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError>;

    /// Execute a `SELECT` statement and return the result rows.
    ///
    /// Positional parameters (`$1`, `$2`, …) are substituted from `params`.
    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError>;

    /// Begin a transaction, returning a handle that can be committed or
    /// rolled back.
    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError>;

    /// Execute multiple semicolon-separated SQL statements in a single call.
    ///
    /// The default implementation splits on `;` and calls [`execute`](Connection::execute)
    /// for each statement.  Backends may override with a more efficient
    /// batch execution path (e.g. `batch_execute` on Postgres).
    ///
    /// **Note:** The default implementation uses a naive `;` split and will
    /// break on SQL containing semicolons inside string literals.  Backends
    /// that override this method (Postgres, MySQL, embedded) handle this
    /// correctly via their native batch execution APIs.
    ///
    /// Returns the total number of rows affected across all statements.
    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let mut total = 0u64;
        for stmt in sql.split(';') {
            let trimmed = stmt.trim();
            if !trimmed.is_empty() {
                total += self.execute(trimmed, &[]).await?;
            }
        }
        Ok(total)
    }

    /// Lightweight connectivity check.
    ///
    /// The default implementation executes `SELECT 1` and discards the result.
    /// Backends should override with a more efficient probe if available.
    async fn ping(&self) -> Result<(), OxiSqlError> {
        self.query("SELECT 1", &[]).await?;
        Ok(())
    }

    /// Compile a SQL statement for repeated execution with different parameters.
    ///
    /// Returns a [`PreparedStatement`] that avoids re-parsing on each call.
    /// The default implementation returns an error indicating the backend does
    /// not support prepared statements; override in backend-specific impls.
    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        let _ = sql;
        Err(OxiSqlError::Other(
            "prepared statements are not supported by this backend".into(),
        ))
    }

    /// List all tables visible to the current connection.
    ///
    /// The default implementation returns an unsupported error.
    /// Backends that support introspection (Postgres, MySQL, embedded) override this.
    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        Err(OxiSqlError::Other(
            "schema introspection not supported by this backend".into(),
        ))
    }

    /// List all columns of the named table.
    ///
    /// The default implementation returns an unsupported error.
    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        let _ = table;
        Err(OxiSqlError::Other(
            "schema introspection not supported by this backend".into(),
        ))
    }

    /// List all indexes defined on the named table.
    ///
    /// The default implementation returns an unsupported error.
    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        let _ = table;
        Err(OxiSqlError::Other(
            "schema introspection not supported by this backend".into(),
        ))
    }

    /// List all foreign-key constraints on the named table.
    ///
    /// The default implementation returns an unsupported error.
    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        let _ = table;
        Err(OxiSqlError::Other(
            "schema introspection not supported by this backend".into(),
        ))
    }

    /// Execute a SQL statement with named parameters.
    ///
    /// Placeholders `:name`, `$name`, and `@name` are translated to positional
    /// form and forwarded to [`execute`](Connection::execute).
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Params`] if a named placeholder has no corresponding
    /// entry in `params`, or any error from the underlying `execute` call.
    async fn execute_named(
        &self,
        sql: &str,
        params: &[(&str, &dyn ToSqlValue)],
    ) -> Result<u64, OxiSqlError> {
        let (rewritten, names) = crate::params::rewrite_named_params(sql)?;
        let positional = crate::params::bind_named_params(&names, params)?;
        self.execute(&rewritten, &positional).await
    }

    /// Execute a SQL query with named parameters and return the result rows.
    ///
    /// Placeholders `:name`, `$name`, and `@name` are translated to positional
    /// form and forwarded to [`query`](Connection::query).
    ///
    /// See [`execute_named`](Connection::execute_named) for placeholder syntax.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Params`] if a named placeholder has no corresponding
    /// entry in `params`, or any error from the underlying `query` call.
    async fn query_named(
        &self,
        sql: &str,
        params: &[(&str, &dyn ToSqlValue)],
    ) -> Result<Vec<Row>, OxiSqlError> {
        let (rewritten, names) = crate::params::rewrite_named_params(sql)?;
        let positional = crate::params::bind_named_params(&names, params)?;
        self.query(&rewritten, &positional).await
    }

    /// Return any SQL warnings generated by the most recently executed statement.
    ///
    /// The list is cleared before each `execute` or `query` call and repopulated
    /// afterwards with warnings issued by the server (if any).  Backends that do
    /// not support warning retrieval return an empty `Vec` (the default).
    ///
    /// MySQL warnings are fetched via `SHOW WARNINGS` — only when the server
    /// reports `warnings_count > 0` in the `OkPacket`, so there is no extra
    /// round-trip on the common no-warning path.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use oxisql_core::Connection;
    /// # async fn example(conn: &dyn Connection) {
    /// conn.execute("INSERT INTO t (col) VALUES (?)", &[&"too long value"]).await.unwrap();
    /// for w in conn.last_warnings() {
    ///     eprintln!("Warning {}: {}", w.code, w.message);
    /// }
    /// # }
    /// ```
    fn last_warnings(&self) -> Vec<SqlWarning> {
        Vec::new()
    }

    /// Execute a `SELECT` and return rows as an async stream.
    ///
    /// The default implementation materialises the full result via [`query`](Connection::query)
    /// then streams the rows.  Backends that support true server-side cursors may
    /// override with incremental fetching.
    ///
    /// This is a regular (non-`async`) method that returns `Pin<Box<dyn Stream>>` directly.
    /// The default body wraps the future returned by [`query`](Connection::query) in a
    /// one-shot stream and flattens the result into per-row items.
    fn query_stream<'a>(
        &'a self,
        sql: &'a str,
        params: &'a [&'a dyn ToSqlValue],
    ) -> Pin<Box<dyn Stream<Item = Result<Row, OxiSqlError>> + Send + 'a>> {
        use futures::StreamExt;
        let fut = self.query(sql, params);
        Box::pin(futures::stream::once(fut).flat_map(|result| match result {
            Ok(rows) => futures::stream::iter(rows.into_iter().map(Ok)).left_stream(),
            Err(e) => futures::stream::once(async move { Err(e) }).right_stream(),
        }))
    }
}

/// A database transaction obtained via [`Connection::transaction`].
///
/// Dropping the transaction without calling [`commit`](Transaction::commit)
/// will implicitly roll back any pending changes.
#[async_trait]
pub trait Transaction: Send + Sync {
    /// Execute a DML/DDL statement within the transaction.
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError>;

    /// Execute a `SELECT` statement within the transaction.
    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError>;

    /// Commit all changes made within this transaction.
    async fn commit(self: Box<Self>) -> Result<(), OxiSqlError>;

    /// Roll back all changes made within this transaction.
    async fn rollback(self: Box<Self>) -> Result<(), OxiSqlError>;

    /// Create a named savepoint within the transaction.
    ///
    /// Savepoints allow nested rollback to a specific point without aborting
    /// the entire transaction.  The `name` must be a valid SQL identifier
    /// (alphanumeric and underscores only; no spaces or special characters).
    ///
    /// The default implementation returns an error.  Backends that support
    /// savepoints (Postgres, MySQL) should override this.
    async fn savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        let _ = name;
        Err(OxiSqlError::Other(
            "savepoints are not supported by this backend".into(),
        ))
    }

    /// Release (discard) a named savepoint.
    ///
    /// The default implementation returns an error.
    async fn release_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        let _ = name;
        Err(OxiSqlError::Other(
            "savepoints are not supported by this backend".into(),
        ))
    }

    /// Roll back the transaction to the named savepoint, undoing all changes
    /// made after the savepoint was created without ending the transaction.
    ///
    /// The default implementation returns an error.
    async fn rollback_to_savepoint(&mut self, name: &str) -> Result<(), OxiSqlError> {
        let _ = name;
        Err(OxiSqlError::Other(
            "savepoints are not supported by this backend".into(),
        ))
    }

    /// Execute a `SELECT` within the transaction and return rows as an async stream.
    ///
    /// Mirrors [`Connection::query_stream`] exactly: the default implementation
    /// materialises the full result via [`query`](Transaction::query) then
    /// streams the rows one by one.  Backends that support server-side cursors
    /// may override with incremental fetching.
    ///
    /// This is a regular (non-`async`) method that returns `Pin<Box<dyn Stream>>`
    /// directly.  The lifetime `'a` ties both `self` and the SQL/params slices
    /// to the returned stream so the borrow checker enforces that `self` is not
    /// moved while the stream is live.
    fn query_stream<'a>(
        &'a mut self,
        sql: &'a str,
        params: &'a [&'a dyn ToSqlValue],
    ) -> Pin<Box<dyn Stream<Item = Result<Row, OxiSqlError>> + Send + 'a>> {
        use futures::StreamExt;
        let fut = self.query(sql, params);
        Box::pin(futures::stream::once(fut).flat_map(|result| match result {
            Ok(rows) => futures::stream::iter(rows.into_iter().map(Ok)).left_stream(),
            Err(e) => futures::stream::once(async move { Err(e) }).right_stream(),
        }))
    }
}
