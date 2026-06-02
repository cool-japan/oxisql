#![forbid(unsafe_code)]
#![warn(missing_docs)]
//! `oxisql-postgres` — Pure-Rust PostgreSQL backend for OxiSQL.
//!
//! Provides [`PgConnection`], which implements [`oxisql_core::Connection`]
//! over `tokio-postgres` (no `libpq`, no C bindings) with TLS support via
//! `rustls` + `rustls-rustcrypto` (no `ring`, no `openssl-sys`).
//!
//! # Wire protocol compliance
//!
//! This crate uses the PostgreSQL **Frontend/Backend Protocol version 3**
//! (introduced in PostgreSQL 7.4) as implemented by `tokio-postgres`.  All
//! client–server communication is framed according to that specification:
//!
//! | API path                          | Wire mode                             |
//! |-----------------------------------|---------------------------------------|
//! | `execute` / `query`               | Extended-query protocol (`Parse` → `Bind` → `Execute` → `Sync`) |
//! | `execute_batch` / `begin_txn`     | Simple-query protocol (`Query` message) |
//! | `query_binary`                    | Extended-query with explicit type OIDs; results decoded in **binary format** (format code 1) |
//! | `prepare` / `describe`            | Extended-query `Parse` + `Describe` only (no `Execute`) |
//! | `pipeline`                        | Multiple extended-query cycles flushed as a single send buffer |
//!
//! ## Limitations
//!
//! * **Logical replication** (Streaming Replication Protocol) is not supported.
//! * **Cancellation** via the `CancelRequest` flow is not exposed at the
//!   `Connection` trait level; individual queries can be cancelled by dropping
//!   the `Future`.
//! * Notification delivery via `LISTEN` is only available on connections
//!   created through [`PgConnection::connect`] (not `from_client`), because
//!   the background `Connection` driver that routes `NotificationResponse`
//!   messages is only spawned in that code path.
//! * PostgreSQL protocol v2 (pre-7.4) servers are not supported.
//!
//! # Quick start (no TLS)
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_postgres::{PgConnection, TlsMode};
//! use oxisql_core::Connection;
//!
//! let conn = PgConnection::connect(
//!     "host=localhost port=5432 user=postgres password=secret",
//!     TlsMode::Disabled,
//! ).await?;
//!
//! conn.execute("CREATE TABLE IF NOT EXISTS t (id BIGINT)", &[]).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Quick start (TLS via OxiTLS)
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use oxisql_postgres::{PgConnection, TlsMode};
//!
//! // Build a ClientConfig trusting the Mozilla CA bundle.
//! // Requires the "pure" + "webpki-roots" features of oxitls (the defaults).
//! let root_store = oxitls::webpki_root_certs();
//! let client_cfg = oxitls::client_config(root_store)
//!     .map_err(|e| format!("TLS cfg: {e}"))?;
//!
//! let conn = PgConnection::connect(
//!     "host=db.example.com port=5432 user=postgres sslmode=require",
//!     TlsMode::Rustls(client_cfg),
//! ).await?;
//! # Ok(())
//! # }
//! ```

mod builder;
mod connection;
mod copy;
mod error;
mod notify;
mod pipeline;
mod prepared;
mod tls;
pub mod types;

pub use builder::{PgConnectionBuilder, TlsMode};
pub use connection::{
    parse_pg_conn_str, ColumnDescription, PgConnParts, PgConnection, PgTransaction,
};
pub use error::PgError;
pub use notify::{NotificationStream, PgNotification};
pub use pipeline::{PgPipeline, PipelineResult};
pub use prepared::PgPrepared;
pub use types::{pg_row_to_row, value_to_param, OwnedParam};
