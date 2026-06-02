//! Error types for the SQLite-compat (Limbo) backend.

use oxisql_core::OxiSqlError;

/// Errors produced by the `oxisql-sqlite-compat` backend.
///
/// These are always mapped to the corresponding [`OxiSqlError`] variant before
/// they cross the [`Connection`][oxisql_core::Connection] trait boundary.
/// Internal helpers and module tests use this type directly for richer
/// context.
#[derive(Debug, thiserror::Error)]
pub enum SqliteCompatError {
    /// A Limbo SQL execution or step error.
    ///
    /// The inner string is the `Display` representation of `limbo::Error`.
    #[error("limbo execution error: {0}")]
    Limbo(String),

    /// A type-mapping failure (e.g. value out of range, unexpected variant).
    #[error("type mapping error: {0}")]
    TypeMap(String),

    /// Connection setup / open failure.
    #[error("connection error: {0}")]
    Connection(String),

    /// Attempted a nested `BEGIN` while already inside a transaction.
    #[error("transaction already active — nested transactions are not supported")]
    NestedTransaction,

    /// A schema introspection query returned an unexpected result.
    #[error("schema introspection error: {0}")]
    Schema(String),

    /// A general internal error that does not fit the above categories.
    #[error("{0}")]
    Other(String),
}

impl From<limbo::Error> for SqliteCompatError {
    fn from(e: limbo::Error) -> Self {
        SqliteCompatError::Limbo(e.to_string())
    }
}

impl From<SqliteCompatError> for OxiSqlError {
    fn from(e: SqliteCompatError) -> Self {
        match e {
            SqliteCompatError::Limbo(msg) => OxiSqlError::Execution(msg),
            SqliteCompatError::TypeMap(msg) => OxiSqlError::Other(msg),
            SqliteCompatError::Connection(msg) => OxiSqlError::Other(msg),
            SqliteCompatError::NestedTransaction => {
                OxiSqlError::Other("nested transactions are not supported".into())
            }
            SqliteCompatError::Schema(msg) => OxiSqlError::Other(msg),
            SqliteCompatError::Other(msg) => OxiSqlError::Other(msg),
        }
    }
}
