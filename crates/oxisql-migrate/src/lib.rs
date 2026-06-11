#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-migrate` — directory-based SQL migration runner for OxiSQL.
//!
//! Migrations are plain `.sql` files placed in a directory and named with a
//! 14-digit timestamp prefix followed by a double underscore and a descriptive
//! name:
//!
//! ```text
//! migrations/
//!   20230101000000__create_users.sql
//!   20230101000001__insert_seed_data.sql
//!   20230101000002__alter_users_add_email.sql
//! ```
//!
//! The runner:
//! 1. Scans the directory with [`scanner::scan_migrations`].
//! 2. Consults the `_oxisql_migrations` tracking table (created on first run)
//!    via the `tracker` module to find pending migrations.
//! 3. Executes each pending migration in version order and records it in the
//!    tracker.
//!
//! # Example
//!
//! ```rust,no_run
//! # #[cfg(feature = "migrate")]
//! # {
//! # #[tokio::main]
//! # async fn main() -> Result<(), oxisql_migrate::MigrationError> {
//! use gluesql::prelude::{Glue, MemoryStorage};
//! use oxisql_migrate::runner::MigrationRunner;
//!
//! let mut glue = Glue::new(MemoryStorage::default());
//! let runner = MigrationRunner::new("migrations/");
//! let applied = runner.run_embedded(&mut glue).await?;
//! println!("Applied {applied} migration(s)");
//! # Ok(())
//! # }
//! # }
//! ```

/// Errors produced by the migration subsystem.
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// A filesystem I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// A migration filename does not match the expected pattern.
    #[error("invalid migration filename: {0}")]
    InvalidFilename(String),
    /// A GlueSQL execution error.
    #[error("migration execution error: {0}")]
    Execution(String),
    /// The SQL in a migration file could not be parsed.
    #[error("migration parse error: {0}")]
    Parse(String),
    /// An already-applied migration file has been modified.
    #[error(
        "checksum mismatch: migration {version} ({name}) has been modified since it was applied"
    )]
    ChecksumMismatch {
        /// The migration version.
        version: u64,
        /// The migration name.
        name: String,
    },
    /// A rollback was requested for a migration that has no `.down.sql` file.
    #[error("no down-migration file for version {version} ({name})")]
    NoDownMigration {
        /// The migration version.
        version: u64,
        /// The migration name.
        name: String,
    },
    /// A pool checkout or connection error prevented the migration from running.
    #[error("connection error: {0}")]
    Connection(String),
    /// The advisory lock could not be acquired within the timeout.
    #[error("migration lock timeout: {0}")]
    LockTimeout(String),
}

#[cfg(feature = "migrate")]
impl From<sqlparser::parser::ParserError> for MigrationError {
    fn from(e: sqlparser::parser::ParserError) -> Self {
        MigrationError::Parse(e.to_string())
    }
}

/// Options that customise the behaviour of `runner::MigrationRunner`.
///
/// Construct with `MigrateOptions::default()` and override specific fields as
/// needed before passing to `runner::MigrationRunner::new_with_options`.
#[derive(Debug, Clone)]
pub struct MigrateOptions {
    /// Name of the migration-tracking table.
    ///
    /// Defaults to `"_oxisql_migrations"`.  Change this if multiple
    /// independent migration sets share the same database.
    pub tracker_table: String,
    /// When `true` the runner logs pending migrations but skips execution.
    ///
    /// Returns `Ok(0)` without modifying the database or the tracker.
    pub dry_run: bool,
    /// When `Some(v)` only migrations with `version <= v` are applied.
    ///
    /// Migrations with a higher version number are left pending.
    pub target_version: Option<u64>,
}

impl Default for MigrateOptions {
    fn default() -> Self {
        Self {
            tracker_table: "_oxisql_migrations".to_string(),
            dry_run: false,
            target_version: None,
        }
    }
}

/// Migration file discovery — scans a directory for migration files.
pub mod scanner;

/// Migration tracker — creates and queries `_oxisql_migrations` table.
#[cfg(feature = "migrate")]
pub mod tracker;

/// Generic migration tracker — works over any [`oxisql_core::Connection`].
#[cfg(feature = "migrate")]
pub mod tracker_generic;

/// Pluggable migration tracker backend trait.
#[cfg(feature = "migrate")]
pub mod tracker_backend;

#[cfg(feature = "migrate")]
pub use tracker_backend::TrackerBackend;

/// Advisory distributed locking for migration runs.
#[cfg(feature = "migrate")]
pub mod lock;

/// Migration runner — orchestrates scan → filter → execute → track.
#[cfg(feature = "migrate")]
pub mod runner;
