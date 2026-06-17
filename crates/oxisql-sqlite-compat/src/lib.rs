#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-sqlite-compat` — Pure-Rust SQLite-compatible backend for OxiSQL.
//!
//! Wraps [oxisqlite](https://github.com/cool-japan/oxisql) — a C-free fork of
//! [Limbo](https://github.com/tursodatabase/limbo) 0.0.22 with all C/C++
//! dependencies stripped — and implements [`Connection`] so that any OxiSQL
//! consumer can use SQLite without linking `libsqlite3` or any C/C++ dependency.
//!
//! # Quick start
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql_core::OxiSqlError> {
//! use oxisql_sqlite_compat::SqliteConnection;
//! use oxisql_core::Connection;
//!
//! // In-memory database (destroyed when the connection is dropped).
//! let conn = SqliteConnection::open_memory().await?;
//!
//! conn.execute("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)", &[]).await?;
//! conn.execute("INSERT INTO users VALUES ($1, $2)", &[&1i64, &"Alice"]).await?;
//!
//! let rows = conn.query("SELECT id, name FROM users", &[]).await?;
//! assert_eq!(rows.len(), 1);
//! # Ok(())
//! # }
//! ```
//!
//! # File-backed database
//!
//! ```rust,no_run
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql_core::OxiSqlError> {
//! use oxisql_sqlite_compat::SqliteConnection;
//!
//! # let path = std::env::temp_dir().join("mydb.sqlite3");
//! let conn = SqliteConnection::open(path.to_str().unwrap()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Limbo version caveats (0.0.22)
//!
//! - **Affected-row count**: Limbo's `execute()` returns a status code, not an
//!   affected-row count.  This crate issues `SELECT changes()` after each DML
//!   to retrieve the count, which adds one round-trip per write operation.
//! - **Positional parameters**: Limbo only supports `?` placeholders.  OxiSQL
//!   uses `$1`, `$2`, …  — this crate performs a quote-aware rewrite before each
//!   statement is prepared.
//! - **Named parameters**: Not supported (upstream limbo 0.0.22 limitation).
//!   Calling code should use positional parameters only.
//! - **Prepared-statement caching**: limbo 0.0.22 / oxisqlite does not cache
//!   compiled bytecode.  The [`PreparedStatement`] wrapper re-prepares on every call.
//! - **Savepoints**: Not supported.  Calling `savepoint` / `rollback_to_savepoint`
//!   / `release_savepoint` returns `OxiSqlError::Other`.
//!
//! [`Connection`]: oxisql_core::Connection
//! [`PreparedStatement`]: oxisql_core::PreparedStatement

pub mod connection;
pub mod error;
pub mod types;

#[cfg(feature = "blocking")]
pub mod blocking;
#[cfg(feature = "blocking")]
pub use blocking::{SqliteBlockingPrepared, SqliteBlockingTransaction, SqliteConnectionBlocking};

pub use connection::SqliteConnection;
pub use error::SqliteCompatError;
