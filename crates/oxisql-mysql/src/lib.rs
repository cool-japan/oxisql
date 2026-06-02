#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-mysql` — Pure-Rust MySQL backend for OxiSQL.
//!
//! Provides [`MyConnection`], which implements [`oxisql_core::Connection`]
//! over `mysql_async` (no `libmysqlclient`, no C bindings) with optional TLS
//! support via `rustls` + `rustls-rustcrypto` (no `ring`, no `openssl-sys`).
//!
//! # Quick start (no TLS)
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_mysql::{MyConnection, TlsMode};
//! use oxisql_core::Connection;
//!
//! let conn = MyConnection::connect(
//!     "mysql://root:secret@localhost:3306/mydb",
//!     TlsMode::Disabled,
//! ).await?;
//!
//! conn.execute("CREATE TABLE IF NOT EXISTS t (id BIGINT)", &[]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Quick start (TLS via rustls-rustcrypto)
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use oxisql_mysql::{MyConnection, TlsMode};
//!
//! // Build a ClientConfig using the pure-Rust CryptoProvider.
//! // In production, populate root_store from rustls-native-certs or webpki-roots.
//! let provider = Arc::new(rustls_rustcrypto::provider());
//! let root_store = rustls::RootCertStore::empty();
//! let cfg = rustls::ClientConfig::builder_with_provider(provider)
//!     .with_safe_default_protocol_versions()?
//!     .with_root_certificates(root_store)
//!     .with_no_client_auth();
//!
//! let conn = MyConnection::connect(
//!     "mysql://root:secret@db.example.com:3306/mydb",
//!     TlsMode::Rustls(Arc::new(cfg)),
//! ).await?;
//! # Ok(())
//! # }
//! ```

pub mod connection;
pub mod error;
pub mod prepared;
pub mod types;

pub use connection::{
    core_params_to_mysql, core_value_to_mysql, is_reconnect_error, MyConnection,
    MyConnectionBuilder, MyTransaction, TlsMode,
};
pub use error::MysqlError;
pub use prepared::MySqlPrepared;
pub use types::{mysql_row_to_core, mysql_value_to_core, mysql_value_to_core_with_type};

// ── URL parsing utilities ──────────────────────────────────────────────────

/// Parsed components of a `mysql://` connection URL.
///
/// Obtain via [`mysql_url_parts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MysqlUrlParts {
    /// Hostname or IP address.
    pub host: String,
    /// TCP port (default 3306 when not specified in the URL).
    pub port: u16,
    /// Database name, if present.
    pub dbname: Option<String>,
    /// User name, if present.
    pub user: Option<String>,
}

/// Parse a `mysql://` URL and return its components.
///
/// Uses `mysql_async::Opts` for parsing, which validates the URL format.
///
/// # Errors
///
/// Returns a [`String`] describing the parse error if the URL is invalid.
///
/// # Example
///
/// ```rust
/// use oxisql_mysql::mysql_url_parts;
///
/// let parts = mysql_url_parts("mysql://alice:secret@db.example.com:3307/shop")
///     .expect("valid URL");
/// assert_eq!(parts.host, "db.example.com");
/// assert_eq!(parts.port, 3307);
/// assert_eq!(parts.dbname, Some("shop".to_string()));
/// assert_eq!(parts.user, Some("alice".to_string()));
/// ```
pub fn mysql_url_parts(url: &str) -> Result<MysqlUrlParts, String> {
    let opts = url
        .parse::<mysql_async::Opts>()
        .map_err(|e| e.to_string())?;
    Ok(MysqlUrlParts {
        host: opts.ip_or_hostname().to_string(),
        port: opts.tcp_port(),
        dbname: opts.db_name().map(|s| s.to_string()),
        user: opts.user().map(|s| s.to_string()),
    })
}
