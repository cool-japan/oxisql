//! Backend-private error type for `oxisql-mysql`.
//!
//! Public API surfaces convert these into [`oxisql_core::OxiSqlError`].

/// Errors that can arise within the `oxisql-mysql` backend.
#[derive(Debug)]
pub enum MysqlError {
    /// A connection or protocol error from `mysql_async`.
    Connection(mysql_async::Error),
    /// A SQL execution or query error from `mysql_async`.
    Query(mysql_async::Error),
    /// A value could not be mapped to the requested OxiSQL type.
    TypeMap(String),
    /// A connection timeout occurred.
    ConnectionTimeout(String),
    /// The connection pool is exhausted.
    PoolExhausted(String),
    /// A unique/foreign-key/check constraint was violated.
    ConstraintViolation {
        /// The MySQL SQLSTATE code (e.g. "23000").
        constraint: String,
        /// The original MySQL error message.
        message: String,
    },
}

impl std::fmt::Display for MysqlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MysqlError::Connection(e) => write!(f, "mysql connection error: {e}"),
            MysqlError::Query(e) => write!(f, "mysql query error: {e}"),
            MysqlError::TypeMap(msg) => write!(f, "mysql type mapping error: {msg}"),
            MysqlError::ConnectionTimeout(msg) => write!(f, "mysql connection timeout: {msg}"),
            MysqlError::PoolExhausted(msg) => write!(f, "mysql pool exhausted: {msg}"),
            MysqlError::ConstraintViolation {
                constraint,
                message,
            } => {
                write!(f, "mysql constraint violation [{constraint}]: {message}")
            }
        }
    }
}

impl std::error::Error for MysqlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            MysqlError::Connection(e) | MysqlError::Query(e) => Some(e),
            MysqlError::TypeMap(_) => None,
            MysqlError::ConnectionTimeout(_) | MysqlError::PoolExhausted(_) => None,
            MysqlError::ConstraintViolation { .. } => None,
        }
    }
}

impl From<mysql_async::Error> for MysqlError {
    fn from(e: mysql_async::Error) -> Self {
        MysqlError::Query(e)
    }
}

impl From<MysqlError> for oxisql_core::OxiSqlError {
    fn from(e: MysqlError) -> Self {
        match e {
            MysqlError::Connection(_) => oxisql_core::OxiSqlError::NotConnected,
            MysqlError::Query(ref inner) => oxisql_core::OxiSqlError::Execution(inner.to_string()),
            MysqlError::TypeMap(msg) => {
                oxisql_core::OxiSqlError::Other(format!("type mapping error: {msg}"))
            }
            MysqlError::ConnectionTimeout(msg) => oxisql_core::OxiSqlError::Timeout(msg),
            MysqlError::PoolExhausted(msg) => oxisql_core::OxiSqlError::ConnectionPool(msg),
            MysqlError::ConstraintViolation {
                constraint,
                message,
            } => oxisql_core::OxiSqlError::ConstraintViolation(format!("[{constraint}] {message}")),
        }
    }
}

/// Classify a `mysql_async::Error` into a [`MysqlError`], detecting
/// constraint violations by MySQL server error code.
///
/// - 1062: Duplicate entry (UNIQUE violation)
/// - 1216/1217: Foreign key constraint
/// - 1451/1452: Foreign key constraint
pub fn classify_mysql_error(e: mysql_async::Error) -> MysqlError {
    match e {
        mysql_async::Error::Server(srv_err) => match srv_err.code {
            1062 | 1451 | 1452 | 1216 | 1217 => MysqlError::ConstraintViolation {
                constraint: srv_err.state,
                message: srv_err.message,
            },
            _ => MysqlError::Query(mysql_async::Error::Server(srv_err)),
        },
        other => MysqlError::Query(other),
    }
}
