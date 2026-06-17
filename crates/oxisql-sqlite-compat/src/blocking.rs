//! Synchronous (blocking) wrappers around [`SqliteConnection`].
//!
//! Enable with the `blocking` feature flag. Each method drives the async
//! runtime via a freshly-built `current_thread` runtime.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "blocking")]
//! # {
//! use oxisql_sqlite_compat::blocking::SqliteConnectionBlocking;
//!
//! let conn = SqliteConnectionBlocking::open_memory().expect("open in-memory db");
//! conn.execute("CREATE TABLE t (id INTEGER)", &[]).expect("create table");
//! # }
//! ```

use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    TableInfo, ToSqlValue, Transaction,
};

use crate::connection::SqliteConnection;

// ── runtime helpers ─────────────────────────────────────────────────────────────

/// Build a fresh `current_thread` Tokio runtime and drive `fut` to completion.
///
/// Used for all blocking wrappers that cannot satisfy `'static` bounds (i.e.
/// any call that borrows `self` parameters from the caller's stack). A fresh
/// runtime is created on each call to avoid conflicts with any outer Tokio
/// context.
fn block_local<F, T>(fut: F) -> Result<T, OxiSqlError>
where
    F: std::future::Future<Output = Result<T, OxiSqlError>>,
{
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| OxiSqlError::Other(format!("runtime build error: {e}")))?;
    rt.block_on(fut)
}

/// Drive a `Send + 'static` future to completion.
///
/// When inside a Tokio runtime, spawns a new OS thread to avoid the
/// "cannot block the async executor" panic. Otherwise builds a fresh
/// `current_thread` runtime.
fn block_static<F, T>(fut: F) -> Result<T, OxiSqlError>
where
    F: std::future::Future<Output = Result<T, OxiSqlError>> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => std::thread::spawn(move || handle.block_on(fut))
            .join()
            .map_err(|_| OxiSqlError::Other("blocking thread panicked".into()))?,
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| OxiSqlError::Other(format!("runtime build error: {e}")))?;
            Ok(rt.block_on(fut)?)
        }
    }
}

// ── SqliteConnectionBlocking ────────────────────────────────────────────────────

/// A synchronous (blocking) wrapper around [`SqliteConnection`].
///
/// All methods drive the underlying async API to completion using a freshly
/// built `current_thread` runtime. Activate via `--features blocking`.
#[derive(Clone)]
pub struct SqliteConnectionBlocking(SqliteConnection);

impl SqliteConnectionBlocking {
    /// Open a SQLite database at the given file path.
    ///
    /// Pass `":memory:"` for an in-memory database or use
    /// [`open_memory`](Self::open_memory) for clarity.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if the file cannot be opened or created.
    pub fn open(path: &str) -> Result<Self, OxiSqlError> {
        let path_owned = path.to_owned();
        let inner = block_static(async move { SqliteConnection::open(&path_owned).await })?;
        Ok(Self(inner))
    }

    /// Open a fresh in-memory SQLite database.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError`] if the engine cannot be initialised.
    pub fn open_memory() -> Result<Self, OxiSqlError> {
        Self::open(":memory:")
    }

    /// Return the path this connection was opened with.
    pub fn path(&self) -> &str {
        self.0.path()
    }

    /// Execute a DML/DDL statement and return the number of rows affected.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::execute`].
    pub fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        block_local(self.0.execute(sql, params))
    }

    /// Execute a `SELECT` statement and return all result rows.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::query`].
    pub fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        block_local(self.0.query(sql, params))
    }

    /// Execute multiple semicolon-separated SQL statements.
    ///
    /// Returns the total number of rows affected across all statements.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::execute_batch`].
    pub fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        block_local(self.0.execute_batch(sql))
    }

    /// Lightweight connectivity check (executes `SELECT 1`).
    ///
    /// # Errors
    ///
    /// Returns an error if the connection is broken.
    pub fn ping(&self) -> Result<(), OxiSqlError> {
        block_local(self.0.ping())
    }

    /// Compile a SQL statement for repeated synchronous execution.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::prepare`].
    pub fn prepare(&self, sql: &str) -> Result<SqliteBlockingPrepared<'_>, OxiSqlError> {
        let inner = block_local(self.0.prepare(sql))?;
        Ok(SqliteBlockingPrepared(inner))
    }

    /// Begin a synchronous transaction.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::transaction`].
    pub fn transaction(&self) -> Result<SqliteBlockingTransaction<'_>, OxiSqlError> {
        let inner = block_local(self.0.transaction())?;
        Ok(SqliteBlockingTransaction(inner))
    }

    /// List all tables visible to the current connection.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::tables`].
    pub fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        block_local(self.0.tables())
    }

    /// List all columns of the named table.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::columns`].
    pub fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        block_local(self.0.columns(table))
    }

    /// List all indexes defined on the named table.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::indexes`].
    pub fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        block_local(self.0.indexes(table))
    }

    /// List all foreign-key constraints on the named table.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`oxisql_core::Connection::foreign_keys`].
    pub fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        block_local(self.0.foreign_keys(table))
    }
}

impl std::fmt::Debug for SqliteConnectionBlocking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("SqliteConnectionBlocking")
            .field(&self.0)
            .finish()
    }
}

// ── SqliteBlockingTransaction ───────────────────────────────────────────────────

/// A synchronous transaction wrapper around a boxed async [`Transaction`].
///
/// Obtain via [`SqliteConnectionBlocking::transaction`].
pub struct SqliteBlockingTransaction<'a>(Box<dyn Transaction + 'a>);

impl<'a> SqliteBlockingTransaction<'a> {
    /// Execute a DML/DDL statement within the transaction.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`Transaction::execute`].
    pub fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        block_local(self.0.execute(sql, params))
    }

    /// Execute a `SELECT` statement within the transaction.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`Transaction::query`].
    pub fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        block_local(self.0.query(sql, params))
    }

    /// Commit all changes made within this transaction.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`Transaction::commit`].
    pub fn commit(self) -> Result<(), OxiSqlError> {
        block_local(self.0.commit())
    }

    /// Roll back all changes made within this transaction.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`Transaction::rollback`].
    pub fn rollback(self) -> Result<(), OxiSqlError> {
        block_local(self.0.rollback())
    }
}

// ── SqliteBlockingPrepared ──────────────────────────────────────────────────────

/// A synchronous prepared-statement wrapper around a boxed async [`PreparedStatement`].
///
/// Obtain via [`SqliteConnectionBlocking::prepare`].
pub struct SqliteBlockingPrepared<'a>(Box<dyn PreparedStatement + 'a>);

impl<'a> SqliteBlockingPrepared<'a> {
    /// Execute the prepared statement and return the number of rows affected.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`PreparedStatement::execute`].
    pub fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        block_local(self.0.execute(params))
    }

    /// Execute the prepared statement as a query and return all result rows.
    ///
    /// # Errors
    ///
    /// Propagates any error from the underlying async [`PreparedStatement::query`].
    pub fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        block_local(self.0.query(params))
    }

    /// Return the original SQL string this statement was prepared with.
    pub fn sql(&self) -> &str {
        self.0.sql()
    }
}
