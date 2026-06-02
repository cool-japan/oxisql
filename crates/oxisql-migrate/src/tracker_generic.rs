//! Generic migration tracker backed by any [`oxisql_core::Connection`].
//!
//! Uses standard SQL compatible with PostgreSQL, MySQL, and other backends.
//! Values are escaped and formatted into SQL strings (no parameterized queries
//! are needed since all values originate from internal code, not user input).

use async_trait::async_trait;
use oxisql_core::Connection;

use crate::tracker::{fnv1a_checksum, now_utc_iso8601, TRACKER_TABLE};
use crate::tracker_backend::TrackerBackend;
use crate::MigrationError;

/// DDL for the migration tracking table — uses INTEGER for broad compatibility
/// (supported by GlueSQL, PostgreSQL, and MySQL alike).
const CREATE_TRACKER_SQL_GENERIC: &str = "CREATE TABLE IF NOT EXISTS _oxisql_migrations (
    version    INTEGER NOT NULL,
    name       TEXT    NOT NULL,
    applied_at TEXT    NOT NULL,
    checksum   TEXT    NOT NULL
)";

/// A migration tracker backed by any [`Connection`].
///
/// Uses `IF NOT EXISTS` DDL and plain SQL DML so it works against
/// PostgreSQL, MySQL, and any other backend that supports standard SQL.
pub struct GenericTracker<'a> {
    conn: &'a dyn Connection,
}

impl<'a> GenericTracker<'a> {
    /// Wrap a connection reference for migration tracking.
    pub fn new(conn: &'a dyn Connection) -> Self {
        Self { conn }
    }

    /// Create the `_oxisql_migrations` table if it does not already exist.
    pub async fn initialize(&self) -> Result<(), MigrationError> {
        self.conn
            .execute(CREATE_TRACKER_SQL_GENERIC, &[])
            .await
            .map(|_| ())
            .map_err(|e| MigrationError::Execution(e.to_string()))
    }

    /// Return the versions that have already been applied.
    pub async fn applied_versions(&self) -> Result<Vec<u64>, MigrationError> {
        let rows = self
            .conn
            .query(&format!("SELECT version FROM {TRACKER_TABLE}"), &[])
            .await
            .map_err(|e| MigrationError::Execution(e.to_string()))?;
        let mut versions = Vec::new();
        for row in rows {
            if let Ok(v) = row.try_get::<i64>("version") {
                versions.push(v as u64);
            }
        }
        Ok(versions)
    }

    /// Return the stored checksum for `version`, if any.
    pub async fn get_checksum(&self, version: u64) -> Result<Option<String>, MigrationError> {
        let rows = self
            .conn
            .query(
                &format!("SELECT checksum FROM {TRACKER_TABLE} WHERE version = {version}"),
                &[],
            )
            .await
            .map_err(|e| MigrationError::Execution(e.to_string()))?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|r| r.try_get::<String>("checksum").ok()))
    }

    /// Record that `version` / `name` was applied.
    pub async fn mark_applied(
        &self,
        version: u64,
        name: &str,
        checksum: &str,
    ) -> Result<(), MigrationError> {
        let applied_at = now_utc_iso8601();
        let escaped_name = name.replace('\'', "''");
        let escaped_checksum = checksum.replace('\'', "''");
        let sql = format!(
            "INSERT INTO {TRACKER_TABLE} (version, name, applied_at, checksum) \
             VALUES ({version}, '{escaped_name}', '{applied_at}', '{escaped_checksum}')"
        );
        self.conn
            .execute(&sql, &[])
            .await
            .map(|_| ())
            .map_err(|e| MigrationError::Execution(e.to_string()))
    }

    /// Remove the tracking record for `version`.
    pub async fn mark_reverted(&self, version: u64) -> Result<(), MigrationError> {
        self.conn
            .execute(
                &format!("DELETE FROM {TRACKER_TABLE} WHERE version = {version}"),
                &[],
            )
            .await
            .map(|_| ())
            .map_err(|e| MigrationError::Execution(e.to_string()))
    }

    /// Compute a checksum for the given SQL content.
    pub fn compute_checksum(sql: &str) -> String {
        fnv1a_checksum(sql.as_bytes())
    }
}

#[async_trait]
impl TrackerBackend for GenericTracker<'_> {
    async fn initialize(&self) -> Result<(), MigrationError> {
        GenericTracker::initialize(self).await
    }

    async fn applied_versions(&self) -> Result<Vec<u64>, MigrationError> {
        GenericTracker::applied_versions(self).await
    }

    async fn mark_applied(
        &self,
        version: u64,
        name: &str,
        checksum: &str,
    ) -> Result<(), MigrationError> {
        GenericTracker::mark_applied(self, version, name, checksum).await
    }

    async fn mark_reverted(&self, version: u64) -> Result<(), MigrationError> {
        GenericTracker::mark_reverted(self, version).await
    }

    async fn get_checksum(&self, version: u64) -> Result<Option<String>, MigrationError> {
        GenericTracker::get_checksum(self, version).await
    }
}
