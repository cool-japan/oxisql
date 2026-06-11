//! Integration tests for the `-- oxisql:no-transaction` migration directive
//! and the CLI `dry-run` subcommand.

#![cfg(feature = "migrate")]

use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_migrate::{
    runner::{MigrationRunner, MigrationState},
    tracker::initialize_tracker,
};

/// Helper: unique temp directory derived from process id + nanoseconds.
fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{}_{}_{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    ))
}

// ── dry-run tests ─────────────────────────────────────────────────────────────

/// `dry_run()` returns all pending migrations without applying any of them.
///
/// After calling `dry_run`, a subsequent `status` call must show every
/// migration still in the `Pending` state.
#[tokio::test]
async fn test_dry_run_lists_pending_without_applying() {
    let dir = unique_temp_dir("oxisql_dry_run_list");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Two forward migrations.
    std::fs::write(
        dir.join("20240601000001__create_alpha.sql"),
        "CREATE TABLE alpha (id INTEGER)",
    )
    .expect("write migration 1");
    std::fs::write(
        dir.join("20240601000002__create_beta.sql"),
        "CREATE TABLE beta (id INTEGER)",
    )
    .expect("write migration 2");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    // Dry-run must return both files.
    let pending = runner.dry_run(&mut glue).await.expect("dry_run failed");
    assert_eq!(
        pending.len(),
        2,
        "dry_run should list 2 pending migrations; got {pending:?}"
    );
    let versions: Vec<u64> = pending.iter().map(|mf| mf.version).collect();
    assert!(
        versions.contains(&20_240_601_000_001_u64),
        "version 1 should be listed"
    );
    assert!(
        versions.contains(&20_240_601_000_002_u64),
        "version 2 should be listed"
    );

    // Nothing must have been applied — the tracker table should exist (dry_run
    // calls initialize_tracker) but have zero rows.
    initialize_tracker(&mut glue)
        .await
        .expect("initialize_tracker");
    let applied_count = oxisql_migrate::tracker::applied_versions(&mut glue)
        .await
        .expect("applied_versions");
    assert_eq!(
        applied_count.len(),
        0,
        "dry_run must not write any tracker rows"
    );

    // Status check: both should still be Pending.
    let mut runner2 = MigrationRunner::new(&dir);
    let statuses = runner2
        .status(&mut glue)
        .await
        .expect("status after dry_run");
    assert_eq!(statuses.len(), 2, "status should show 2 entries");
    for (mf, state) in &statuses {
        assert_eq!(
            *state,
            MigrationState::Pending,
            "migration {} should still be Pending after dry_run",
            mf.version
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

// ── no-transaction directive tests ────────────────────────────────────────────

/// A migration prefixed with `-- oxisql:no-transaction` executes without error.
///
/// The directive must not prevent the SQL from running — it only skips the
/// `BEGIN`/`COMMIT` wrapping for that migration.
#[tokio::test]
async fn test_no_transaction_directive_executes_successfully() {
    let dir = unique_temp_dir("oxisql_no_txn_directive");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // Migration with the directive on the very first line.
    std::fs::write(
        dir.join("20240602000001__no_txn_table.sql"),
        "-- oxisql:no-transaction\nCREATE TABLE no_txn_tbl (id INTEGER)",
    )
    .expect("write directive migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded with no-transaction directive should succeed");
    assert_eq!(applied, 1, "the directive migration should be applied");

    // Verify the table was actually created.
    glue.execute("SELECT * FROM no_txn_tbl")
        .await
        .expect("no_txn_tbl should exist after the directive migration");

    std::fs::remove_dir_all(&dir).ok();
}

/// A migration *without* the directive is still wrapped in a transaction and
/// executes correctly.
#[tokio::test]
async fn test_normal_migration_without_directive_still_works() {
    let dir = unique_temp_dir("oxisql_normal_no_directive");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    std::fs::write(
        dir.join("20240602000001__create_normal.sql"),
        "CREATE TABLE normal_tbl (id INTEGER)",
    )
    .expect("write normal migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded without directive should succeed");
    assert_eq!(applied, 1);

    glue.execute("SELECT * FROM normal_tbl")
        .await
        .expect("normal_tbl should exist");

    std::fs::remove_dir_all(&dir).ok();
}

/// The directive only takes effect when it appears before any SQL statements.
/// A comment after the first SQL token must not trigger the no-transaction path.
#[tokio::test]
async fn test_directive_in_sql_body_is_ignored() {
    let dir = unique_temp_dir("oxisql_directive_in_body");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    // The directive appears after a non-comment SQL line — must be ignored.
    std::fs::write(
        dir.join("20240602000001__after_sql.sql"),
        "CREATE TABLE body_tbl (id INTEGER)\n-- oxisql:no-transaction\n",
    )
    .expect("write migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    // Should still execute without error — the directive is simply not found.
    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("migration with late directive should succeed");
    assert_eq!(applied, 1);

    std::fs::remove_dir_all(&dir).ok();
}

/// Blank lines before the directive are permitted.
#[tokio::test]
async fn test_directive_after_blank_lines_is_recognised() {
    let dir = unique_temp_dir("oxisql_directive_blank_lines");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    std::fs::write(
        dir.join("20240602000001__blank_then_directive.sql"),
        "\n\n-- oxisql:no-transaction\nCREATE TABLE blank_dir_tbl (id INTEGER)",
    )
    .expect("write migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("directive after blank lines should be recognised");
    assert_eq!(applied, 1);

    std::fs::remove_dir_all(&dir).ok();
}
