/// Errors that can arise within the `oxisql-postgres` backend.
///
/// This is a backend-private type.  Public API surfaces translate these errors
/// into [`oxisql_core::OxiSqlError`].
#[derive(Debug)]
pub enum PgError {
    /// Error during connection or wire-protocol communication.
    Postgres(tokio_postgres::Error),
    /// A column value could not be mapped to the requested OxiSQL type.
    TypeConversion(String),
    /// TLS setup failed.
    Tls(String),
    /// A unique/foreign-key/check constraint was violated.
    ConstraintViolation {
        /// The PostgreSQL constraint name.
        constraint: String,
        /// Detail from the server error.
        detail: String,
    },
    /// The operation timed out.
    Timeout(String),
    /// The connection pool is exhausted.
    PoolExhausted(String),
    /// A COPY IN or COPY OUT protocol error.
    Copy(String),
    /// A LISTEN/NOTIFY error (invalid channel name or unsupported connection type).
    Notify(String),
    /// A connection-level error (e.g. cancel request failed).
    Connection(String),
    /// A replication-protocol error (slot management, negotiation, or unexpected server state).
    Replication(String),
    /// A malformed or truncated wire-protocol message was received (decoder bounds-check failure).
    Protocol(String),
}

impl std::fmt::Display for PgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PgError::Postgres(e) => write!(f, "postgres error: {e}"),
            PgError::TypeConversion(msg) => write!(f, "type conversion error: {msg}"),
            PgError::Tls(msg) => write!(f, "TLS error: {msg}"),
            PgError::ConstraintViolation { constraint, detail } => {
                write!(f, "constraint violation on '{constraint}': {detail}")
            }
            PgError::Timeout(msg) => write!(f, "postgres timeout: {msg}"),
            PgError::PoolExhausted(msg) => write!(f, "postgres pool exhausted: {msg}"),
            PgError::Copy(msg) => write!(f, "postgres COPY error: {msg}"),
            PgError::Notify(msg) => write!(f, "postgres NOTIFY error: {msg}"),
            PgError::Connection(msg) => write!(f, "postgres connection error: {msg}"),
            PgError::Replication(msg) => write!(f, "postgres replication error: {msg}"),
            PgError::Protocol(msg) => write!(f, "postgres protocol error: {msg}"),
        }
    }
}

impl std::error::Error for PgError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PgError::Postgres(e) => Some(e),
            PgError::TypeConversion(_)
            | PgError::Tls(_)
            | PgError::ConstraintViolation { .. }
            | PgError::Timeout(_)
            | PgError::PoolExhausted(_)
            | PgError::Copy(_)
            | PgError::Notify(_)
            | PgError::Connection(_)
            | PgError::Replication(_)
            | PgError::Protocol(_) => None,
        }
    }
}

impl From<tokio_postgres::Error> for PgError {
    fn from(e: tokio_postgres::Error) -> Self {
        use tokio_postgres::error::SqlState;
        if let Some(db_err) = e.as_db_error() {
            let code = db_err.code();
            if code == &SqlState::UNIQUE_VIOLATION
                || code == &SqlState::FOREIGN_KEY_VIOLATION
                || code == &SqlState::CHECK_VIOLATION
            {
                return PgError::ConstraintViolation {
                    constraint: db_err.constraint().unwrap_or("unknown").to_string(),
                    detail: db_err.detail().unwrap_or("").to_string(),
                };
            }
        }
        PgError::Postgres(e)
    }
}

impl From<std::io::Error> for PgError {
    fn from(e: std::io::Error) -> Self {
        PgError::Connection(e.to_string())
    }
}

impl From<PgError> for oxisql_core::OxiSqlError {
    fn from(e: PgError) -> Self {
        match e {
            PgError::ConstraintViolation {
                ref constraint,
                ref detail,
            } => oxisql_core::OxiSqlError::ConstraintViolation(format!("'{constraint}': {detail}")),
            PgError::Timeout(msg) => oxisql_core::OxiSqlError::Timeout(msg),
            PgError::PoolExhausted(msg) => oxisql_core::OxiSqlError::ConnectionPool(msg),
            PgError::Postgres(ref inner) if inner.code().is_some() => {
                oxisql_core::OxiSqlError::Execution(e.to_string())
            }
            PgError::Postgres(_) => oxisql_core::OxiSqlError::Other(e.to_string()),
            // OxiSqlError::TypeMismatch uses &'static str — we cannot put a heap
            // string there without leaking.  Use Other, which accepts String.
            PgError::TypeConversion(msg) => {
                oxisql_core::OxiSqlError::Other(format!("type conversion error: {msg}"))
            }
            PgError::Tls(msg) => oxisql_core::OxiSqlError::Other(msg),
            PgError::Copy(msg) => oxisql_core::OxiSqlError::Other(format!("COPY error: {msg}")),
            PgError::Notify(msg) => oxisql_core::OxiSqlError::Other(format!("NOTIFY error: {msg}")),
            PgError::Connection(msg) => {
                oxisql_core::OxiSqlError::Other(format!("connection error: {msg}"))
            }
            PgError::Replication(msg) => {
                oxisql_core::OxiSqlError::Other(format!("replication error: {msg}"))
            }
            PgError::Protocol(msg) => {
                oxisql_core::OxiSqlError::Other(format!("protocol error: {msg}"))
            }
        }
    }
}
