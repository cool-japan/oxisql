//! Sled-backed persistent connection, transaction, and prepared statement.
//!
//! This module is only compiled when the `sled-storage` Cargo feature is
//! enabled.  All items are re-exported from the crate root via
//! `pub use sled_conn::SledEmbeddedConnection`.

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use gluesql::prelude::Glue;
use oxisql_core::{Connection, OxiSqlError, PreparedStatement, Row, ToSqlValue, Transaction};
use tokio::sync::Mutex;

use crate::sled_storage::SledGlueStorage;
use crate::{
    handle_attach, handle_pragma, payload_to_affected_rows, payload_to_rows, substitute_params,
};

// ── low-level helpers ────────────────────────────────────────────────────────

/// Run SQL against a mutable `Glue<SledGlueStorage>` and return affected row count.
async fn sled_execute(
    glue: &mut Glue<SledGlueStorage>,
    sql: &str,
    params: &[&dyn ToSqlValue],
) -> Result<u64, OxiSqlError> {
    let sql = substitute_params(sql, params)?;
    let payloads = glue
        .execute(&sql)
        .await
        .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
    Ok(payloads.iter().map(payload_to_affected_rows).sum())
}

/// Run a SELECT SQL against a mutable `Glue<SledGlueStorage>` and return rows.
async fn sled_query(
    glue: &mut Glue<SledGlueStorage>,
    sql: &str,
    params: &[&dyn ToSqlValue],
) -> Result<Vec<Row>, OxiSqlError> {
    let sql = substitute_params(sql, params)?;
    let payloads = glue
        .execute(&sql)
        .await
        .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
    Ok(payloads.into_iter().flat_map(payload_to_rows).collect())
}

// ── SledEmbeddedConnection ───────────────────────────────────────────────────

/// A persistent SQL connection backed by [`SledGlueStorage`].
///
/// Data written through this connection is persisted to the sled database
/// on disk and survives process restarts.
///
/// Use [`SledEmbeddedConnection::open`] to create a file-backed instance.
/// Unlike the redb backend, sled does not support in-memory operation.
#[derive(Clone)]
pub struct SledEmbeddedConnection {
    inner: Arc<Mutex<Glue<SledGlueStorage>>>,
}

impl fmt::Debug for SledEmbeddedConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SledEmbeddedConnection")
            .field("inner", &"Arc<Mutex<Glue<SledGlueStorage>>>")
            .finish()
    }
}

impl SledEmbeddedConnection {
    /// Open (or create) a persistent sled database at `path`.
    ///
    /// The path points to a sled database directory.  sled creates the
    /// directory if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the sled database cannot be opened.
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, OxiSqlError> {
        let storage = SledGlueStorage::open(path).map_err(|e| OxiSqlError::Other(e.to_string()))?;
        let glue = Glue::new(storage);
        Ok(Self {
            inner: Arc::new(Mutex::new(glue)),
        })
    }
}

// ── SledEmbeddedTransaction ──────────────────────────────────────────────────

/// Transaction over a [`SledEmbeddedConnection`].
pub struct SledEmbeddedTransaction {
    guard: tokio::sync::OwnedMutexGuard<Glue<SledGlueStorage>>,
}

#[async_trait]
impl Transaction for SledEmbeddedTransaction {
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        sled_execute(&mut self.guard, sql, params).await
    }

    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        sled_query(&mut self.guard, sql, params).await
    }

    async fn commit(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        sled_execute(&mut self.guard, "COMMIT", &[]).await?;
        Ok(())
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        sled_execute(&mut self.guard, "ROLLBACK", &[]).await?;
        Ok(())
    }

    async fn savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by SledGlueStorage".into(),
        ))
    }

    async fn release_savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by SledGlueStorage".into(),
        ))
    }

    async fn rollback_to_savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by SledGlueStorage".into(),
        ))
    }
}

// ── SledEmbeddedPrepared ─────────────────────────────────────────────────────

/// A "prepared" statement for the sled-backed connection.
pub struct SledEmbeddedPrepared {
    inner: Arc<Mutex<Glue<SledGlueStorage>>>,
    sql_text: String,
}

#[async_trait]
impl PreparedStatement for SledEmbeddedPrepared {
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        sled_execute(&mut guard, &self.sql_text, params).await
    }

    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        sled_query(&mut guard, &self.sql_text, params).await
    }

    fn sql(&self) -> &str {
        &self.sql_text
    }
}

// ── Connection impl ──────────────────────────────────────────────────────────

#[async_trait]
impl Connection for SledEmbeddedConnection {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        if let Some(result) = handle_pragma(sql) {
            let _ = result?;
            return Ok(0);
        }
        if let Some(result) = handle_attach(sql) {
            let _ = result?;
            return Ok(0);
        }
        let mut guard = self.inner.lock().await;
        sled_execute(&mut guard, sql, params).await
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        if let Some(result) = handle_pragma(sql) {
            return result;
        }
        if let Some(result) = handle_attach(sql) {
            return result;
        }
        let mut guard = self.inner.lock().await;
        sled_query(&mut guard, sql, params).await
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        let guard = Arc::clone(&self.inner).lock_owned().await;
        let mut txn = SledEmbeddedTransaction { guard };
        txn.execute("BEGIN", &[]).await?;
        Ok(Box::new(txn))
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        let payloads = guard
            .execute(sql)
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(payloads.iter().map(payload_to_affected_rows).sum())
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        let _guard = self.inner.lock().await;
        Ok(())
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        Ok(Box::new(SledEmbeddedPrepared {
            inner: Arc::clone(&self.inner),
            sql_text: sql.to_string(),
        }))
    }
}
