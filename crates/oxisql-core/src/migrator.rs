//! [`Migrator`] trait — abstract interface for applying and rolling back
//! database schema migrations.

use std::collections::HashSet;

use async_trait::async_trait;

use crate::OxiSqlError;

/// The execution status of a single migration version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationStatus {
    /// The migration has been applied to the target database.
    Applied,
    /// The migration has not yet been applied.
    Pending,
    /// The migration status could not be determined (e.g. checksum mismatch).
    Unknown,
}

/// Metadata describing a single migration version.
#[derive(Debug, Clone)]
pub struct MigrationInfo {
    /// The numeric version identifier (e.g. timestamp or sequential number).
    pub version: u64,
    /// A human-readable name for the migration.
    pub name: String,
    /// The current status of this migration.
    pub status: MigrationStatus,
    /// ISO-8601 timestamp at which the migration was applied, if known.
    pub applied_at: Option<String>,
    /// Checksum of the migration SQL, if computed.
    pub checksum: Option<String>,
}

/// An async trait for managing database schema migrations.
///
/// Implementors are expected to be `Send + Sync` so they can be used safely
/// across async task boundaries.
///
/// # Default implementation
///
/// [`Migrator::pending`] has a default body that calls [`Migrator::status`]
/// and returns any version from `all_versions` that is not yet
/// [`MigrationStatus::Applied`].  Backends may override this with a more
/// efficient query.
#[async_trait]
pub trait Migrator: Send + Sync {
    /// Apply a migration identified by `version` by executing `sql`.
    ///
    /// The implementation is expected to record the successful application
    /// (e.g. by inserting a row into a migration history table) so that
    /// [`status`](Migrator::status) reflects the change.
    async fn apply(&mut self, version: u64, sql: &str) -> Result<(), OxiSqlError>;

    /// Roll back a previously applied migration by executing `down_sql`.
    ///
    /// The implementation should remove or update the corresponding history
    /// record so that [`status`](Migrator::status) no longer reports this
    /// version as [`MigrationStatus::Applied`].
    async fn rollback(&mut self, version: u64, down_sql: &str) -> Result<(), OxiSqlError>;

    /// Return metadata about all known migrations and their current status.
    async fn status(&self) -> Result<Vec<MigrationInfo>, OxiSqlError>;

    /// Return the subset of `all_versions` that have not yet been applied.
    ///
    /// The default implementation calls [`status`](Migrator::status) and
    /// filters out already-applied versions.  Backends may override this with
    /// a cheaper single-query implementation.
    async fn pending(&self, all_versions: &[u64]) -> Result<Vec<u64>, OxiSqlError> {
        let applied: HashSet<u64> = self
            .status()
            .await?
            .into_iter()
            .filter(|m| m.status == MigrationStatus::Applied)
            .map(|m| m.version)
            .collect();
        Ok(all_versions
            .iter()
            .filter(|v| !applied.contains(v))
            .copied()
            .collect())
    }
}
