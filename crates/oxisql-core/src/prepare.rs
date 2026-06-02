//! [`PreparedStatement`] trait for OxiSQL backends.

use async_trait::async_trait;

use crate::{OxiSqlError, Row, ToSqlValue};

/// A server-side prepared statement.
///
/// Prepared statements are compiled once by the database server and can be
/// executed multiple times with different parameter sets, avoiding repeated
/// parsing and planning overhead.
///
/// Obtain via `Connection::prepare`.
#[async_trait]
pub trait PreparedStatement: Send {
    /// Execute the prepared statement and return the number of rows affected.
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError>;

    /// Execute the prepared statement as a `SELECT` and return all result rows.
    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError>;

    /// Return the original SQL text this statement was compiled from.
    fn sql(&self) -> &str;
}
