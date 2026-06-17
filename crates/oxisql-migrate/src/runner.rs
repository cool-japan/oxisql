//! Migration runner — orchestrates scan -> filter pending -> execute -> track.
//!
//! `MigrationRunner` is the primary entry point for applying migrations.
//!
//! Each migration file's SQL is validated with `sqlparser` before execution so
//! that syntax errors are caught eagerly (before any state changes) and
//! reported as [`MigrationError::Parse`].

use std::path::{Path, PathBuf};

use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_core::Connection;
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

use crate::{
    lock::MigrationLock,
    scanner::{scan_migrations, MigrationFile},
    tracker::{
        applied_versions, compute_checksum, get_checksum, initialize_tracker, mark_applied,
        mark_reverted, update_checksum,
    },
    tracker_generic::GenericTracker,
    MigrateOptions, MigrationError,
};

/// The state of a migration relative to the tracker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationState {
    /// The migration has been applied (exists in tracker).
    Applied,
    /// The migration is pending (not yet applied).
    Pending,
    /// The migration was applied but its checksum no longer matches the file.
    Modified,
    /// The migration was applied but no corresponding file exists on disk.
    Orphaned,
}

impl std::fmt::Display for MigrationState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MigrationState::Applied => write!(f, "applied"),
            MigrationState::Pending => write!(f, "pending"),
            MigrationState::Modified => write!(f, "modified"),
            MigrationState::Orphaned => write!(f, "orphaned"),
        }
    }
}

/// Applies SQL migration files from a directory against an embedded GlueSQL
/// instance or any [`Connection`] backend.
///
/// Files are discovered by [`scan_migrations`], filtered to those not yet
/// recorded in `_oxisql_migrations`, executed in ascending version order, and
/// recorded in the tracker.
///
/// The runner caches the result of the first directory scan so that repeated
/// calls to [`status`](MigrationRunner::status) do not re-read the filesystem.
/// Call [`invalidate_cache`](MigrationRunner::invalidate_cache) if the
/// directory contents may have changed between calls.
pub struct MigrationRunner {
    /// Path to the directory containing `.sql` migration files.
    dir: PathBuf,
    /// Configuration options for this runner instance.
    opts: MigrateOptions,
    /// Cached result of the last [`scan_migrations`] call.
    ///
    /// Populated on the first [`status`] call and reused by subsequent calls
    /// without re-scanning the directory.
    cached_migrations: Option<Vec<MigrationFile>>,
    /// Optional advisory lock acquired around the migration run.
    ///
    /// When `Some`, [`run_with_conn`](MigrationRunner::run_with_conn) will
    /// call `lock.acquire(30)` before running migrations and `lock.release()`
    /// once finished (whether or not an error occurred).
    lock: Option<Box<dyn MigrationLock>>,
}

impl MigrationRunner {
    /// Create a new runner pointing at `dir` with default options.
    ///
    /// The directory does not need to exist at construction time — errors are
    /// returned from [`run_embedded`](MigrationRunner::run_embedded).
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            opts: MigrateOptions::default(),
            cached_migrations: None,
            lock: None,
        }
    }

    /// Create a new runner pointing at `dir` with custom [`MigrateOptions`].
    ///
    /// Use this to configure the tracker table name, enable dry-run mode, or
    /// limit execution to a specific target version.
    pub fn new_with_options(dir: impl Into<PathBuf>, opts: MigrateOptions) -> Self {
        Self {
            dir: dir.into(),
            opts,
            cached_migrations: None,
            lock: None,
        }
    }

    /// Attach an advisory lock to this runner.
    ///
    /// When a lock is attached, [`run_with_conn`](MigrationRunner::run_with_conn)
    /// will acquire it (with a 30-second timeout) before running any migrations
    /// and release it when done — even if the migration run fails.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # #[cfg(feature = "migrate")]
    /// # {
    /// use oxisql_migrate::lock::NoopMigrationLock;
    /// use oxisql_migrate::runner::MigrationRunner;
    ///
    /// let runner = MigrationRunner::new("migrations/")
    ///     .with_lock(NoopMigrationLock::new());
    /// # }
    /// ```
    pub fn with_lock(mut self, lock: impl MigrationLock + 'static) -> Self {
        self.lock = Some(Box::new(lock));
        self
    }

    /// Return the list of migration files, using the in-memory cache when available.
    ///
    /// On the first call the directory is scanned with [`scan_migrations`] and
    /// the result is stored in `self.cached_migrations`.  Subsequent calls
    /// return the cached list directly without reading the filesystem again.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — directory or file read failure on the first scan.
    /// - [`MigrationError::InvalidFilename`] — a file with an unexpected name pattern.
    fn get_migrations(&mut self) -> Result<Vec<MigrationFile>, MigrationError> {
        if let Some(ref cached) = self.cached_migrations {
            return Ok(cached.clone());
        }
        let migrations = scan_migrations(&self.dir)?;
        self.cached_migrations = Some(migrations.clone());
        Ok(migrations)
    }

    /// Invalidate the cached migration file list.
    ///
    /// Call this when the migration directory contents have changed and the
    /// runner should re-scan on the next [`status`](MigrationRunner::status) call.
    pub fn invalidate_cache(&mut self) {
        self.cached_migrations = None;
    }

    /// Apply all pending migrations using a connection from the embedded pool.
    ///
    /// Checks out a [`Connection`] via the `ConnectionPool` trait and
    /// delegates to [`run_with_conn`](MigrationRunner::run_with_conn).
    ///
    /// Returns the number of migrations applied.  Returns `Ok(0)` when all
    /// migrations are already applied (idempotent).
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Connection`] — pool checkout failed (pool is closed).
    /// - [`MigrationError::Io`] — file read failure.
    /// - [`MigrationError::Parse`] — SQL syntax error in a migration file.
    /// - [`MigrationError::Execution`] — SQL execution or tracker failure.
    /// - [`MigrationError::ChecksumMismatch`] — an already-applied file was modified.
    #[cfg(feature = "pool")]
    pub async fn run_pooled(
        &mut self,
        pool: &oxisql_pool::embedded::EmbeddedPool,
    ) -> Result<usize, MigrationError> {
        // Use UFCS to call the `ConnectionPool` trait's `get()` (which returns
        // `Box<dyn Connection + Send>`) rather than the inherent `get()` on
        // `EmbeddedPool` that returns `tokio::sync::MutexGuard`.
        let conn = oxisql_core::ConnectionPool::get(pool)
            .await
            .map_err(|e| MigrationError::Connection(e.to_string()))?;
        self.run_with_conn(conn.as_ref()).await
    }

    /// Apply all pending migrations using any backend from the unified `OxidbPool` enum.
    ///
    /// Dispatches to the appropriate backend:
    ///
    /// - **Embedded** — delegates to [`run_pooled`](MigrationRunner::run_pooled).
    /// - **Postgres / MySQL** — these backends require a live connection obtained
    ///   from outside the pool.  Returns [`MigrationError::Io`] with an
    ///   `Unsupported` kind and a helpful message directing callers to
    ///   [`run_with_conn`](MigrationRunner::run_with_conn).
    ///
    /// Returns the number of migrations applied.  Returns `Ok(0)` when all
    /// migrations are already applied (idempotent).
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Connection`] — pool checkout failed (pool is closed).
    /// - [`MigrationError::Io`] — file read failure, or unsupported backend.
    /// - [`MigrationError::Parse`] — SQL syntax error in a migration file.
    /// - [`MigrationError::Execution`] — SQL execution or tracker failure.
    /// - [`MigrationError::ChecksumMismatch`] — an already-applied file was modified.
    #[cfg(feature = "pool")]
    pub async fn run_with_pool(
        &mut self,
        pool: &oxisql_pool::OxidbPool,
    ) -> Result<usize, MigrationError> {
        // The `pool` feature unconditionally enables `oxisql-pool/embedded`, so
        // `OxidbPool::Embedded` is always compiled in.  Other variants (Postgres,
        // MySQL) may be absent when those features are not enabled in the pool crate;
        // the catch-all covers them and returns a clear error pointing to `run_with_conn`.
        match pool {
            oxisql_pool::OxidbPool::Embedded(ep) => self.run_pooled(ep).await,
            #[allow(unreachable_patterns)]
            _ => Err(MigrationError::Io(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "pooled migrations for non-embedded backends require a live connection \
                 — use MigrationRunner::run_with_conn()",
            ))),
        }
    }

    /// Apply all pending migrations in the runner's directory against `glue`.
    ///
    /// 1. Initialises the `_oxisql_migrations` tracker table if absent.
    /// 2. Scans for `.sql` files matching the version-prefix naming convention.
    /// 3. Filters to files whose version is not yet recorded.
    /// 4. Validates SQL syntax and verifies checksums.
    /// 5. Executes each pending file's SQL in ascending version order.
    /// 6. Records each file in the tracker after successful execution.
    ///
    /// When [`MigrateOptions::dry_run`] is `true` the runner returns `Ok(0)`
    /// without touching the database.  When [`MigrateOptions::target_version`]
    /// is set, only migrations with `version <= target_version` are applied.
    ///
    /// Returns the number of migrations applied.  Returns `Ok(0)` when all
    /// migrations are already applied (idempotent).
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — directory read failure or file read failure.
    /// - [`MigrationError::Execution`] — SQL execution or tracker failure.
    /// - [`MigrationError::ChecksumMismatch`] — an already-applied migration
    ///   file has been modified since it was applied.
    pub async fn run_embedded(
        &self,
        glue: &mut Glue<MemoryStorage>,
    ) -> Result<usize, MigrationError> {
        // Dry-run: report nothing applied, touch nothing.
        if self.opts.dry_run {
            return Ok(0);
        }

        // Ensure the tracker table exists.
        initialize_tracker(glue).await?;

        // Discover migration files.
        let all_files = scan_migrations(&self.dir)?;

        // Query which versions are already applied.
        let applied = applied_versions(glue).await?;

        // Verify checksums of already-applied migrations
        for mf in &all_files {
            if applied.contains(&mf.version) {
                let sql = std::fs::read_to_string(&mf.path)?;
                let current_checksum = compute_checksum(&sql);
                if let Some(stored_checksum) = get_checksum(glue, mf.version).await? {
                    if stored_checksum != current_checksum {
                        return Err(MigrationError::ChecksumMismatch {
                            version: mf.version,
                            name: mf.name.clone(),
                        });
                    }
                }
            }
        }

        // Filter to pending migrations, respecting target_version.
        let pending: Vec<_> = all_files
            .into_iter()
            .filter(|mf| {
                !applied.contains(&mf.version)
                    && self.opts.target_version.is_none_or(|tv| mf.version <= tv)
            })
            .collect();

        let count = pending.len();

        for mf in pending {
            // Read the SQL file.
            let sql = std::fs::read_to_string(&mf.path)?;

            // Pre-validate SQL syntax with sqlparser.
            validate_sql(&sql, &mf.name, mf.version)?;

            // Compute checksum for tracking.
            let checksum = compute_checksum(&sql);

            // Check whether this migration opts out of transaction wrapping.
            let skip_txn = has_no_transaction_directive(&sql);

            // Begin a transaction unless the migration requests no-transaction
            // mode.  If the storage backend does not support transactions, BEGIN
            // may fail — in that case we proceed without one.
            let in_txn = if skip_txn {
                false
            } else {
                glue.execute("BEGIN").await.is_ok()
            };

            // Execute the migration SQL.
            let exec_result = glue.execute(&sql).await;

            match exec_result {
                Err(e) => {
                    if in_txn {
                        let _ = glue.execute("ROLLBACK").await;
                    }
                    return Err(MigrationError::Execution(format!(
                        "migration {} ({}): {e}",
                        mf.version, mf.name
                    )));
                }
                Ok(_) => {
                    // Record in the tracker before committing.
                    let track_result = mark_applied(glue, mf.version, &mf.name, &checksum).await;
                    if let Err(e) = track_result {
                        if in_txn {
                            let _ = glue.execute("ROLLBACK").await;
                        }
                        return Err(e);
                    }
                    if in_txn {
                        glue.execute("COMMIT").await.map_err(|e| {
                            MigrationError::Execution(format!(
                                "COMMIT for migration {}: {e}",
                                mf.version
                            ))
                        })?;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Return all pending migrations without executing them (dry-run).
    ///
    /// Scans the migration directory and filters to files whose version is
    /// not yet recorded in the tracker.  Does not modify the database.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — directory read failure.
    /// - [`MigrationError::Execution`] — tracker query failure.
    pub async fn dry_run(
        &self,
        glue: &mut Glue<MemoryStorage>,
    ) -> Result<Vec<MigrationFile>, MigrationError> {
        initialize_tracker(glue).await?;

        let all_files = scan_migrations(&self.dir)?;
        let applied = applied_versions(glue).await?;

        let pending: Vec<_> = all_files
            .into_iter()
            .filter(|mf| !applied.contains(&mf.version))
            .collect();

        Ok(pending)
    }

    /// Return the status of all migrations (applied, pending, modified, or orphaned).
    ///
    /// Scans the migration directory and cross-references with the tracker
    /// to determine each migration's state.  Versions that are recorded in the
    /// tracker but have no corresponding file on disk are reported as
    /// [`MigrationState::Orphaned`].
    ///
    /// The migration file list is retrieved via the internal cache: the first
    /// call scans the directory; subsequent calls reuse the cached result.
    /// Call [`invalidate_cache`](MigrationRunner::invalidate_cache) to force a
    /// re-scan if the directory contents have changed.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — directory or file read failure.
    /// - [`MigrationError::Execution`] — tracker query failure.
    pub async fn status(
        &mut self,
        glue: &mut Glue<MemoryStorage>,
    ) -> Result<Vec<(MigrationFile, MigrationState)>, MigrationError> {
        initialize_tracker(glue).await?;

        let all_files = self.get_migrations()?;
        let applied = applied_versions(glue).await?;

        // Build a set of versions found on disk for orphan detection.
        let file_versions: std::collections::HashSet<u64> =
            all_files.iter().map(|mf| mf.version).collect();

        let mut result = Vec::with_capacity(all_files.len() + applied.len());

        // Process all files found on disk.
        for mf in all_files {
            let state = if applied.contains(&mf.version) {
                // Check if the file has been modified since application.
                let sql = std::fs::read_to_string(&mf.path)?;
                let current_checksum = compute_checksum(&sql);
                match get_checksum(glue, mf.version).await? {
                    Some(stored) if stored != current_checksum => MigrationState::Modified,
                    _ => MigrationState::Applied,
                }
            } else {
                MigrationState::Pending
            };
            result.push((mf, state));
        }

        // Report orphaned entries: applied in tracker but not on disk.
        for &version in &applied {
            if !file_versions.contains(&version) {
                // Construct a synthetic MigrationFile for the orphaned entry.
                let orphaned_mf = MigrationFile {
                    version,
                    name: String::from("<orphaned>"),
                    path: PathBuf::from("<unknown>"),
                    down_path: None,
                };
                result.push((orphaned_mf, MigrationState::Orphaned));
            }
        }

        // Sort by version so the output is deterministic.
        result.sort_by_key(|(mf, _)| mf.version);

        Ok(result)
    }

    /// Roll back applied migrations down to (but not including) `target_version`.
    ///
    /// Applied migrations with version > `target_version` are reverted in
    /// **descending** version order.  Each must have a `.down.sql` companion
    /// file.
    ///
    /// Returns the number of migrations rolled back.  If `target_version` is
    /// `0`, **all** applied migrations are rolled back.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — file read failure.
    /// - [`MigrationError::Execution`] — SQL execution or tracker failure.
    /// - [`MigrationError::NoDownMigration`] — a migration to be rolled back
    ///   has no `.down.sql` companion file.
    pub async fn rollback(
        &self,
        glue: &mut Glue<MemoryStorage>,
        target_version: u64,
    ) -> Result<usize, MigrationError> {
        // Ensure the tracker table exists.
        initialize_tracker(glue).await?;

        // Query which versions are currently applied.
        let applied = applied_versions(glue).await?;

        // Discover all migration files (with down_path populated by the scanner).
        let all_files = scan_migrations(&self.dir)?;

        // Build a lookup map from version -> MigrationFile.
        let file_map: std::collections::HashMap<u64, MigrationFile> =
            all_files.into_iter().map(|mf| (mf.version, mf)).collect();

        // Collect migrations to roll back: applied AND version > target_version.
        let mut to_revert: Vec<u64> = applied
            .into_iter()
            .filter(|&v| v > target_version)
            .collect();

        // Process in descending order (highest version first).
        to_revert.sort_by(|a, b| b.cmp(a));

        let count = to_revert.len();
        for version in to_revert {
            // Look up the migration file.  If it's missing from disk, we still
            // need the down path — treat as NoDownMigration.
            let down_path = match file_map.get(&version) {
                Some(mf) => match &mf.down_path {
                    Some(p) => p.clone(),
                    None => {
                        return Err(MigrationError::NoDownMigration {
                            version,
                            name: mf.name.clone(),
                        });
                    }
                },
                None => {
                    return Err(MigrationError::NoDownMigration {
                        version,
                        name: String::from("<orphaned>"),
                    });
                }
            };

            // Read the down-migration SQL.
            let sql = std::fs::read_to_string(&down_path)?;

            // Validate SQL syntax before executing.
            validate_sql(&sql, &format!("{version}.down"), version)?;

            // Begin a transaction; if the storage backend does not support
            // transactions, BEGIN may fail — in that case we proceed without one.
            let in_txn = glue.execute("BEGIN").await.is_ok();

            // Execute the down-migration SQL.
            let exec_result = glue.execute(&sql).await;

            match exec_result {
                Err(e) => {
                    if in_txn {
                        let _ = glue.execute("ROLLBACK").await;
                    }
                    return Err(MigrationError::Execution(format!(
                        "rollback migration {version}: {e}"
                    )));
                }
                Ok(_) => {
                    // Remove the tracking record before committing.
                    let revert_result = mark_reverted(glue, version).await;
                    if let Err(e) = revert_result {
                        if in_txn {
                            let _ = glue.execute("ROLLBACK").await;
                        }
                        return Err(e);
                    }
                    if in_txn {
                        glue.execute("COMMIT").await.map_err(|e| {
                            MigrationError::Execution(format!(
                                "COMMIT for rollback migration {version}: {e}"
                            ))
                        })?;
                    }
                }
            }
        }

        Ok(count)
    }

    /// Apply all pending migrations using any [`Connection`] (Postgres, MySQL, or embedded).
    ///
    /// This is the backend-agnostic counterpart to [`run_embedded`](MigrationRunner::run_embedded).  The
    /// `_oxisql_migrations` tracker table is created using `BIGINT` and standard
    /// SQL compatible with PostgreSQL and MySQL.
    ///
    /// Each migration is executed via [`Connection::execute_batch`] so that
    /// multi-statement migration files work correctly.
    ///
    /// Returns the number of migrations applied.  Returns `Ok(0)` when all
    /// migrations are already applied.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — file read failure.
    /// - [`MigrationError::Parse`] — SQL syntax error in a migration file.
    /// - [`MigrationError::Execution`] — SQL execution or tracker failure.
    /// - [`MigrationError::ChecksumMismatch`] — an already-applied file was modified.
    pub async fn run_with_conn(&mut self, conn: &dyn Connection) -> Result<usize, MigrationError> {
        // Acquire the advisory lock if one was configured (30-second timeout).
        if let Some(ref mut lock) = self.lock {
            lock.acquire(30).await?;
        }

        let result = self.run_with_conn_inner(conn).await;

        // Always release the lock, regardless of whether the run succeeded.
        if let Some(ref mut lock) = self.lock {
            // Swallow the release error if the run already failed so the
            // original error is surfaced to the caller.
            let release_result = lock.release().await;
            if let Ok(v) = result {
                return release_result.map(|_| v);
            }
        }

        result
    }

    /// Inner implementation of [`run_with_conn`](MigrationRunner::run_with_conn),
    /// separated so the lock acquire/release wrapper above stays clean.
    async fn run_with_conn_inner(&self, conn: &dyn Connection) -> Result<usize, MigrationError> {
        let tracker = GenericTracker::new(conn);
        tracker.initialize().await?;

        let all_files = scan_migrations(&self.dir)?;
        let applied = tracker.applied_versions().await?;

        // Verify checksums of already-applied migrations.
        for mf in &all_files {
            if applied.contains(&mf.version) {
                let sql = std::fs::read_to_string(&mf.path)?;
                let current_checksum = GenericTracker::compute_checksum(&sql);
                if let Some(stored) = tracker.get_checksum(mf.version).await? {
                    if stored != current_checksum {
                        return Err(MigrationError::ChecksumMismatch {
                            version: mf.version,
                            name: mf.name.clone(),
                        });
                    }
                }
            }
        }

        let pending: Vec<_> = all_files
            .into_iter()
            .filter(|mf| !applied.contains(&mf.version))
            .collect();

        let count = pending.len();

        for mf in pending {
            let sql = std::fs::read_to_string(&mf.path)?;
            validate_sql(&sql, &mf.name, mf.version)?;
            let checksum = GenericTracker::compute_checksum(&sql);

            conn.execute_batch(&sql).await.map_err(|e| {
                MigrationError::Execution(format!("migration {} ({}): {e}", mf.version, mf.name))
            })?;

            tracker
                .mark_applied(mf.version, &mf.name, &checksum)
                .await?;
        }

        Ok(count)
    }

    /// Update the stored checksum for `version` to match the current on-disk
    /// content of the migration file.
    ///
    /// Use this when a migration file has been intentionally edited after it was
    /// applied and you want to suppress the [`MigrationState::Modified`] warning
    /// without reverting and re-applying the migration.
    ///
    /// # Errors
    ///
    /// - [`MigrationError::Io`] — migration file could not be read or the
    ///   version does not correspond to any file in `self.dir`.
    /// - [`MigrationError::Execution`] — the tracker UPDATE failed.
    pub async fn force_rechecksum(
        &self,
        glue: &mut Glue<MemoryStorage>,
        version: u64,
    ) -> Result<(), MigrationError> {
        // Locate the migration file for this version.
        let all_files = scan_migrations(&self.dir)?;
        let mf = all_files
            .into_iter()
            .find(|f| f.version == version)
            .ok_or_else(|| {
                MigrationError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no migration file found for version {version}"),
                ))
            })?;

        // Compute the checksum of the current on-disk content.
        let sql = std::fs::read_to_string(&mf.path)?;
        let new_checksum = compute_checksum(&sql);

        // Update the tracker row.
        update_checksum(glue, version, &new_checksum).await
    }

    /// Roll back applied migrations down to (but not including) `target_version`,
    /// using any [`Connection`].
    ///
    /// Applied migrations with version > `target_version` are reverted in
    /// **descending** version order.  Each must have a `.down.sql` companion file.
    ///
    /// Returns the number of migrations rolled back.
    pub async fn rollback_with_conn(
        &self,
        conn: &dyn Connection,
        target_version: u64,
    ) -> Result<usize, MigrationError> {
        let tracker = GenericTracker::new(conn);
        tracker.initialize().await?;

        let applied = tracker.applied_versions().await?;
        let all_files = scan_migrations(&self.dir)?;

        let file_map: std::collections::HashMap<u64, MigrationFile> =
            all_files.into_iter().map(|mf| (mf.version, mf)).collect();

        let mut to_revert: Vec<u64> = applied
            .into_iter()
            .filter(|&v| v > target_version)
            .collect();
        to_revert.sort_by(|a, b| b.cmp(a));

        let count = to_revert.len();
        for version in to_revert {
            let down_path = match file_map.get(&version) {
                Some(mf) => match &mf.down_path {
                    Some(p) => p.clone(),
                    None => {
                        return Err(MigrationError::NoDownMigration {
                            version,
                            name: mf.name.clone(),
                        });
                    }
                },
                None => {
                    return Err(MigrationError::NoDownMigration {
                        version,
                        name: String::from("<orphaned>"),
                    });
                }
            };

            let sql = std::fs::read_to_string(&down_path)?;
            validate_sql(&sql, &format!("{version}.down"), version)?;

            conn.execute_batch(&sql).await.map_err(|e| {
                MigrationError::Execution(format!("rollback migration {version}: {e}"))
            })?;

            tracker.mark_reverted(version).await?;
        }

        Ok(count)
    }
}

/// Return `true` if the SQL text contains an `-- oxisql:no-transaction`
/// directive in the leading comment block.
///
/// The scan stops at the first non-comment, non-blank line so that the
/// directive is only effective when placed before any SQL statements.
/// This prevents a comment inside a string literal or later in the file from
/// accidentally disabling transaction wrapping.
fn has_no_transaction_directive(sql: &str) -> bool {
    for line in sql.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("--") {
            if trimmed.contains("oxisql:no-transaction") {
                return true;
            }
        } else if !trimmed.is_empty() {
            // First non-comment, non-empty line — stop scanning.
            break;
        }
    }
    false
}

/// Parse `sql` with `sqlparser`'s generic dialect to catch syntax errors before
/// execution.
fn validate_sql(sql: &str, name: &str, version: u64) -> Result<(), MigrationError> {
    let dialect = GenericDialect {};
    Parser::parse_sql(&dialect, sql).map_err(|e| {
        MigrationError::Parse(format!(
            "migration {} ({name}): SQL parse error -- {e}",
            version
        ))
    })?;
    Ok(())
}
