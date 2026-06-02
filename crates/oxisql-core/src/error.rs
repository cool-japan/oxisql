//! Error type for OxiSQL operations.

use std::fmt;

/// Errors that can occur during OxiSQL operations.
#[derive(Debug)]
pub enum OxiSqlError {
    /// The SQL could not be parsed.
    Parse(String),
    /// The statement failed during execution.
    Execution(String),
    /// No connection is available.
    NotConnected,
    /// A value had an unexpected type.
    TypeMismatch {
        /// The type that was expected.
        expected: &'static str,
        /// The type that was encountered.
        got: &'static str,
    },
    /// A unique or foreign key constraint was violated.
    ConstraintViolation(String),
    /// A connection or query timeout occurred.
    Timeout(String),
    /// A connection pool error (exhausted, timeout, build failure).
    ConnectionPool(String),
    /// A migration error.
    Migration(String),
    /// The requested URI or feature is not supported by this backend.
    UnsupportedUri(String),
    /// A named-parameter binding error (missing or malformed placeholder).
    Params(String),
    /// Any other error.
    Other(String),
}

impl fmt::Display for OxiSqlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OxiSqlError::Parse(s) => write!(f, "SQL parse error: {s}"),
            OxiSqlError::Execution(s) => write!(f, "execution error: {s}"),
            OxiSqlError::NotConnected => write!(f, "not connected"),
            OxiSqlError::TypeMismatch { expected, got } => {
                write!(f, "type mismatch: expected {expected}, got {got}")
            }
            OxiSqlError::ConstraintViolation(s) => {
                write!(f, "constraint violation: {s}")
            }
            OxiSqlError::Timeout(s) => write!(f, "timeout: {s}"),
            OxiSqlError::ConnectionPool(s) => write!(f, "connection pool error: {s}"),
            OxiSqlError::Migration(s) => write!(f, "migration error: {s}"),
            OxiSqlError::UnsupportedUri(s) => write!(f, "unsupported URI or feature: {s}"),
            OxiSqlError::Params(s) => write!(f, "parameter binding error: {s}"),
            OxiSqlError::Other(s) => write!(f, "error: {s}"),
        }
    }
}

impl std::error::Error for OxiSqlError {}
