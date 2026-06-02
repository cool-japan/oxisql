//! `TrackerBackend` — abstract interface for migration tracking storage.
//!
//! Implement this trait to make any connection type usable as a migration
//! tracker, allowing [`MigrationRunner`](crate::runner::MigrationRunner) to work against embedded,
//! PostgreSQL, MySQL, or any other backend interchangeably.

use async_trait::async_trait;

use crate::MigrationError;

/// Abstraction over migration-tracking storage.
///
/// Implement this trait for any backend (embedded GlueSQL, PostgreSQL, MySQL,
/// …) to make it pluggable into [`MigrationRunner`](crate::runner::MigrationRunner).
///
/// All methods are `async` to support both synchronous and network-backed
/// implementations without blocking.
#[async_trait]
pub trait TrackerBackend: Send + Sync {
    /// Create the migration-tracking table if it does not already exist.
    ///
    /// Implementations must be idempotent (`CREATE TABLE IF NOT EXISTS`
    /// semantics).
    async fn initialize(&self) -> Result<(), MigrationError>;

    /// Return the list of migration versions that have already been applied,
    /// in any order.
    async fn applied_versions(&self) -> Result<Vec<u64>, MigrationError>;

    /// Record that the migration identified by `version` and `name` was
    /// applied, storing `checksum` for later drift detection.
    async fn mark_applied(
        &self,
        version: u64,
        name: &str,
        checksum: &str,
    ) -> Result<(), MigrationError>;

    /// Remove the tracking record for `version` (called after a successful
    /// down-migration execution).
    async fn mark_reverted(&self, version: u64) -> Result<(), MigrationError>;

    /// Return the stored checksum for `version`, or `None` if the version has
    /// not been applied or no checksum was stored.
    async fn get_checksum(&self, version: u64) -> Result<Option<String>, MigrationError>;
}
