//! MySQL prepared statement implementing [`oxisql_core::PreparedStatement`].
//!
//! MySQL statement IDs are connection-local, so each [`MySqlPrepared`] holds an
//! exclusive [`mysql_async::Conn`] from the pool.  The connection is returned to
//! the pool automatically when this struct is dropped.

use async_trait::async_trait;
use mysql_async::prelude::Queryable;

use oxisql_core::{OxiSqlError, PreparedStatement, Row, ToSqlValue};

use crate::connection::core_params_to_mysql;
use crate::error::MysqlError;
use crate::types::mysql_row_to_core;

/// A MySQL prepared statement holding its dedicated connection.
///
/// MySQL statement IDs are connection-local, so each `MySqlPrepared`
/// owns an exclusive `Conn` from the pool.  The connection is returned to
/// the pool when this struct is dropped.
///
/// Obtain via [`oxisql_core::Connection::prepare`] on a [`crate::MyConnection`].
pub struct MySqlPrepared {
    conn: mysql_async::Conn,
    stmt: mysql_async::Statement,
    sql_text: String,
}

impl MySqlPrepared {
    pub(crate) fn new(
        conn: mysql_async::Conn,
        stmt: mysql_async::Statement,
        sql_text: String,
    ) -> Self {
        Self {
            conn,
            stmt,
            sql_text,
        }
    }
}

#[async_trait]
impl PreparedStatement for MySqlPrepared {
    /// Execute the prepared statement and return the number of rows affected.
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mysql_params = core_params_to_mysql(params);
        let result = self
            .conn
            .exec_iter(&self.stmt, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))?;
        let affected = result.affected_rows();
        result
            .drop_result()
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))?;
        Ok(affected)
    }

    /// Execute the prepared statement as a `SELECT` and return all result rows.
    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let mysql_params = core_params_to_mysql(params);
        let mysql_rows: Vec<mysql_async::Row> = self
            .conn
            .exec(&self.stmt, mysql_params)
            .await
            .map_err(|e| OxiSqlError::from(MysqlError::Query(e)))?;
        mysql_rows
            .into_iter()
            .map(|r| mysql_row_to_core(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Return the original SQL text this statement was compiled from.
    fn sql(&self) -> &str {
        &self.sql_text
    }
}
