//! [`PgPrepared`] — a compiled PostgreSQL prepared statement.
//!
//! Wraps a `tokio_postgres::Statement` (a server-side handle) together with
//! a shared reference to the underlying client so the statement can be
//! re-executed with different parameters without re-parsing on every call.
//!
//! # Cache
//!
//! [`StmtCache`] is a `std::sync::Mutex`-guarded `HashMap` keyed on the SQL
//! text.  Using `std::sync::Mutex` (not `tokio::sync::Mutex`) is deliberate:
//! the lock is always acquired and released *synchronously* — it is never held
//! across an `await` point, so no deadlock / blocking risk.
//!
//! `tokio_postgres::Statement` is cheaply `Clone`-able; it holds a
//! reference-counted handle to the server-side plan.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_postgres::Statement;

use oxisql_core::{OxiSqlError, PreparedStatement, Row, ToSqlValue};

use crate::types::{pg_row_to_row, value_to_param, OwnedParam};

// ── StmtCache ──────────────────────────────────────────────────────────────────

/// A shared, synchronously-locked cache mapping SQL text → compiled
/// `tokio_postgres::Statement`.
pub(crate) type StmtCache = Arc<Mutex<HashMap<String, Statement>>>;

// ── helper ─────────────────────────────────────────────────────────────────────

fn build_owned(params: &[&dyn ToSqlValue]) -> Vec<OwnedParam> {
    params
        .iter()
        .map(|p| value_to_param(&p.to_value()))
        .collect()
}

fn owned_refs(owned: &[OwnedParam]) -> Vec<&(dyn tokio_postgres::types::ToSql + Sync)> {
    owned.iter().map(|p| p as _).collect()
}

// ── PgPrepared ─────────────────────────────────────────────────────────────────

/// A compiled PostgreSQL prepared statement.
///
/// Obtain via [`oxisql_core::Connection::prepare`] on a [`crate::PgConnection`].
///
/// The statement is compiled once by the server and re-used for every
/// subsequent execution, avoiding repeated parse/plan cycles.
pub struct PgPrepared {
    pub(crate) client: Arc<tokio::sync::Mutex<tokio_postgres::Client>>,
    pub(crate) stmt: Statement,
    pub(crate) sql_text: String,
}

#[async_trait]
impl PreparedStatement for PgPrepared {
    /// Execute the prepared statement (DML/DDL) and return the row count.
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let owned = build_owned(params);
        let refs = owned_refs(&owned);
        let client = self.client.lock().await;
        client
            .execute(&self.stmt, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))
    }

    /// Execute the prepared statement as a `SELECT` and return all rows.
    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let owned = build_owned(params);
        let refs = owned_refs(&owned);
        let client = self.client.lock().await;
        let pg_rows = client
            .query(&self.stmt, refs.as_slice())
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        pg_rows
            .into_iter()
            .map(|r| pg_row_to_row(r).map_err(OxiSqlError::from))
            .collect()
    }

    /// Return the original SQL text this statement was compiled from.
    fn sql(&self) -> &str {
        &self.sql_text
    }
}
