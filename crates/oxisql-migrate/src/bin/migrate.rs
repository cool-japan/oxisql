//! Command-line interface for `oxisql-migrate`.
//!
//! Applies, checks status of, or rolls back OxiSQL migrations stored as plain
//! `.sql` files in a directory.
//!
//! # Usage
//!
//! ```text
//! oxisql-migrate run    --dir migrations/
//! oxisql-migrate status --dir migrations/
//! oxisql-migrate rollback --dir migrations/ --version 3
//! ```
//!
//! The `run` and `rollback` subcommands use an in-process GlueSQL
//! `MemoryStorage` backend.  Each invocation starts with a fresh in-memory
//! database, so `run` is most useful for smoke-testing that migration files
//! parse and execute without error.  For persistent databases, use the
//! `MigrationRunner` API directly in your application code.

use clap::{Parser, Subcommand};
use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_embedded::EmbeddedConnection;
use oxisql_migrate::{
    runner::{MigrationRunner, MigrationState},
    scanner::scan_migrations,
    MigrationError,
};

/// OxiSQL migration runner CLI.
///
/// Applies, reports on, or rolls back SQL migrations stored as plain `.sql`
/// files following the `<14-digit-timestamp>__<name>.sql` naming convention.
#[derive(Debug, Parser)]
#[command(name = "oxisql-migrate")]
#[command(about = "Apply, check status of, or roll back OxiSQL migrations")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Apply all pending migrations against a fresh in-memory database.
    ///
    /// Useful as a CI smoke-test: confirms that migration files are syntactically
    /// valid and execute without error.  The in-memory database is discarded when
    /// the process exits.
    Run {
        /// Directory containing migration `.sql` files.
        #[arg(short, long, default_value = "migrations")]
        dir: String,
        /// Force-update checksums for any Modified migrations before running.
        ///
        /// When set, migrations whose on-disk content no longer matches the
        /// stored checksum are re-checksummed rather than rejected, then the
        /// normal apply step proceeds.
        #[arg(long)]
        force: bool,
    },
    /// Show the status (pending / applied) of all migrations.
    ///
    /// Scans the migration directory and lists each file with its version and
    /// descriptive name.  Does not modify any database.
    Status {
        /// Directory containing migration `.sql` files.
        #[arg(short, long, default_value = "migrations")]
        dir: String,
    },
    /// Roll back applied migrations to the specified target version.
    ///
    /// Reverts all migrations with a version number greater than `--version` in
    /// descending order.  Each migration to be rolled back must have a
    /// corresponding `.down.sql` companion file.  Uses a fresh in-memory
    /// database, so this is primarily useful for testing rollback logic.
    Rollback {
        /// Directory containing migration `.sql` files.
        #[arg(short, long, default_value = "migrations")]
        dir: String,
        /// Target version: all migrations with version > this value are reverted.
        /// Use 0 to roll back all applied migrations.
        #[arg(short, long)]
        version: u64,
    },
    /// Preview pending migrations without applying them.
    ///
    /// Lists all migrations that would be applied against a fresh in-memory
    /// database without executing any SQL or modifying tracker state.
    DryRun {
        /// Directory containing migration `.sql` files.
        #[arg(short, long, default_value = "migrations")]
        dir: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run { dir, force } => cmd_run(&dir, force).await,
        Commands::Status { dir } => cmd_status(&dir),
        Commands::Rollback { dir, version } => cmd_rollback(&dir, version).await,
        Commands::DryRun { dir } => cmd_dry_run(&dir).await,
    }
}

/// Apply all pending migrations against a fresh in-memory database.
///
/// When `force` is `true` the runner first detects any `Modified` migrations
/// (whose on-disk content no longer matches the stored checksum) and
/// re-checksums them before running.  This uses the embedded GlueSQL path so
/// that both `status` and `run_embedded` share the same in-memory tracker.
///
/// When `force` is `false` the existing `EmbeddedConnection` / `run_with_conn`
/// path is used (no tracker re-checksumming).
async fn cmd_run(dir: &str, force: bool) -> anyhow::Result<()> {
    if force {
        let mut glue = Glue::new(MemoryStorage::default());
        let mut runner = MigrationRunner::new(dir);

        // Detect Modified migrations and bring their checksums up to date.
        let statuses = runner
            .status(&mut glue)
            .await
            .map_err(format_migration_error)?;
        for (mf, state) in &statuses {
            if *state == MigrationState::Modified {
                runner
                    .force_rechecksum(&mut glue, mf.version)
                    .await
                    .map_err(format_migration_error)?;
            }
        }

        let applied = runner
            .run_embedded(&mut glue)
            .await
            .map_err(format_migration_error)?;

        if applied == 0 {
            println!("No pending migrations — all up to date.");
        } else {
            println!("Applied {applied} migration(s) successfully.");
        }
    } else {
        let conn = EmbeddedConnection::open_memory()
            .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?;

        let mut runner = MigrationRunner::new(dir);
        let applied = runner
            .run_with_conn(&conn)
            .await
            .map_err(format_migration_error)?;

        if applied == 0 {
            println!("No pending migrations — all up to date.");
        } else {
            println!("Applied {applied} migration(s) successfully.");
        }
    }
    Ok(())
}

/// List the status of every migration file in the directory.
///
/// This function does not require a live database connection: it scans the
/// directory and prints each file with version and name.  All files are
/// reported as "pending" since there is no tracker to query.
fn cmd_status(dir: &str) -> anyhow::Result<()> {
    let migrations = scan_migrations(std::path::Path::new(dir))
        .map_err(|e| anyhow::anyhow!("failed to scan migrations: {e}"))?;

    if migrations.is_empty() {
        println!("No migration files found in '{dir}'.");
        return Ok(());
    }

    println!("{:<20} {:<8} NAME", "VERSION", "STATUS");
    println!("{:-<60}", "");

    for mf in &migrations {
        let down = if mf.down_path.is_some() {
            " (has .down.sql)"
        } else {
            ""
        };
        println!("{:<20} {:<8} {}{}", mf.version, "pending", mf.name, down);
    }
    println!();
    println!(
        "{} migration file(s) found. Status shown against a fresh database (no tracker).",
        migrations.len()
    );
    Ok(())
}

/// List all pending migrations that would be applied without executing them.
///
/// Uses a fresh in-memory GlueSQL instance as the tracker so the output
/// reflects what `run` would apply against an empty database.
async fn cmd_dry_run(dir: &str) -> anyhow::Result<()> {
    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(dir);

    let pending = runner
        .dry_run(&mut glue)
        .await
        .map_err(format_migration_error)?;

    if pending.is_empty() {
        println!("No pending migrations — all up to date.");
        return Ok(());
    }

    println!("{:<20} {:<8} NAME", "VERSION", "STATUS");
    println!("{:-<60}", "");

    for mf in &pending {
        let down = if mf.down_path.is_some() {
            " (has .down.sql)"
        } else {
            ""
        };
        println!("{:<20} {:<8} {}{}", mf.version, "pending", mf.name, down);
    }
    println!();
    println!(
        "{} pending migration(s) would be applied (dry-run, no changes made).",
        pending.len()
    );
    Ok(())
}

/// Roll back applied migrations to `target_version` against a fresh in-memory
/// connection.
///
/// This exercises the `.down.sql` rollback path.  The in-memory database is
/// discarded when the process exits.
async fn cmd_rollback(dir: &str, target_version: u64) -> anyhow::Result<()> {
    let conn = EmbeddedConnection::open_memory()
        .map_err(|e| anyhow::anyhow!("failed to open in-memory database: {e}"))?;

    // First apply all migrations so there is something to roll back.
    let mut runner = MigrationRunner::new(dir);
    let applied = runner
        .run_with_conn(&conn)
        .await
        .map_err(format_migration_error)?;

    if applied == 0 {
        println!("No migrations to apply — nothing to roll back.");
        return Ok(());
    }

    // Now roll back to the target version.
    let reverted = runner
        .rollback_with_conn(&conn, target_version)
        .await
        .map_err(format_migration_error)?;

    if reverted == 0 {
        println!(
            "No migrations with version > {target_version} were applied; nothing rolled back."
        );
    } else {
        println!("Rolled back {reverted} migration(s) to version {target_version}.");
    }
    Ok(())
}

/// Convert a [`MigrationError`] into an [`anyhow::Error`] with a descriptive
/// message suitable for display on the terminal.
fn format_migration_error(e: MigrationError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}
