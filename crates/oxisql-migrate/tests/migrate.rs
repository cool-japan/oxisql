//! Integration tests for `oxisql-migrate`.
//!
//! Writes real `.sql` migration files to a temporary directory, then exercises
//! the scanner, tracker, and runner against a GlueSQL `MemoryStorage` instance.
//!
//! Requires the `migrate` feature: `cargo test --features migrate`

#![cfg(feature = "migrate")]

use gluesql::prelude::{Glue, MemoryStorage, Payload};
use oxisql_migrate::{
    runner::MigrationRunner,
    scanner::scan_migrations,
    tracker::{applied_count, applied_versions, initialize_tracker},
};

use oxisql_migrate::{MigrateOptions, MigrationError};

/// Create 3 migration SQL files in a temp directory and return the path.
fn write_test_migrations() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    // Migration 1 — create a table
    std::fs::write(
        dir.join("20230101000000__create_users.sql"),
        "CREATE TABLE users (id INTEGER, name TEXT)",
    )
    .expect("failed to write migration 1");

    // Migration 2 — insert a row
    std::fs::write(
        dir.join("20230101000001__insert_row.sql"),
        "INSERT INTO users VALUES (1, 'Alice')",
    )
    .expect("failed to write migration 2");

    // Migration 3 — create another table (ALTER TABLE not well-supported by GlueSQL)
    std::fs::write(
        dir.join("20230101000002__create_orders.sql"),
        "CREATE TABLE orders (id INTEGER, user_id INTEGER)",
    )
    .expect("failed to write migration 3");

    dir
}

// ── scanner tests ─────────────────────────────────────────────────────────────

#[test]
fn scanner_discovers_sorted_migrations() {
    let dir = write_test_migrations();
    let files = scan_migrations(&dir).expect("scan_migrations failed");

    assert_eq!(files.len(), 3, "expected 3 migration files");

    // Verify sorted ascending version order.
    assert_eq!(files[0].version, 20_230_101_000_000_u64);
    assert_eq!(files[1].version, 20_230_101_000_001_u64);
    assert_eq!(files[2].version, 20_230_101_000_002_u64);

    // Verify name extraction.
    assert_eq!(files[0].name, "create_users");
    assert_eq!(files[1].name, "insert_row");
    assert_eq!(files[2].name, "create_orders");

    // Verify paths exist.
    for f in &files {
        assert!(f.path.exists(), "migration path should exist: {:?}", f.path);
    }
}

#[test]
fn scanner_ignores_non_matching_files() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_noisy_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create temp dir");

    // Non-matching files
    std::fs::write(dir.join("README.md"), "# ignore me").unwrap();
    std::fs::write(dir.join("not_a_migration.sql"), "SELECT 1").unwrap();
    std::fs::write(dir.join("202301010000__short_version.sql"), "SELECT 1").unwrap();

    // Valid migration
    std::fs::write(
        dir.join("20230101000000__valid.sql"),
        "CREATE TABLE valid (id INTEGER)",
    )
    .unwrap();

    let files = scan_migrations(&dir).expect("scan_migrations failed");
    assert_eq!(files.len(), 1, "should only find 1 valid migration");
    assert_eq!(files[0].version, 20_230_101_000_000_u64);
    assert_eq!(files[0].name, "valid");
}

// ── runner tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn runner_applies_three_migrations() {
    let dir = write_test_migrations();
    let mut glue = Glue::new(MemoryStorage::default());

    let runner = MigrationRunner::new(&dir);
    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded failed");

    assert_eq!(applied, 3, "expected 3 migrations to be applied");
}

#[tokio::test]
async fn runner_is_idempotent() {
    let dir = write_test_migrations();
    let mut glue = Glue::new(MemoryStorage::default());

    let runner = MigrationRunner::new(&dir);

    // First run — applies 3
    let first = runner
        .run_embedded(&mut glue)
        .await
        .expect("first run_embedded failed");
    assert_eq!(first, 3);

    // Second run — no-op
    let second = runner
        .run_embedded(&mut glue)
        .await
        .expect("second run_embedded failed");
    assert_eq!(second, 0, "re-run should apply 0 migrations (idempotent)");
}

#[tokio::test]
async fn tracker_records_all_applied_migrations() {
    let dir = write_test_migrations();
    let mut glue = Glue::new(MemoryStorage::default());

    let runner = MigrationRunner::new(&dir);
    runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded failed");

    // Check tracker has 3 rows.
    let count = applied_count(&mut glue)
        .await
        .expect("applied_count failed");
    assert_eq!(count, 3, "_oxisql_migrations should have 3 rows");

    // Check all version numbers are present.
    let versions = applied_versions(&mut glue)
        .await
        .expect("applied_versions failed");
    assert!(versions.contains(&20_230_101_000_000_u64));
    assert!(versions.contains(&20_230_101_000_001_u64));
    assert!(versions.contains(&20_230_101_000_002_u64));
}

#[tokio::test]
async fn tracker_initializes_empty() {
    let mut glue = Glue::new(MemoryStorage::default());

    initialize_tracker(&mut glue)
        .await
        .expect("initialize_tracker failed");

    let count = applied_count(&mut glue)
        .await
        .expect("applied_count failed");
    assert_eq!(count, 0, "fresh tracker should have 0 rows");
}

#[tokio::test]
async fn runner_data_persists_after_migrations() {
    let dir = write_test_migrations();
    let mut glue = Glue::new(MemoryStorage::default());

    let runner = MigrationRunner::new(&dir);
    runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded failed");

    // Verify that migration 2 (INSERT) actually inserted data.
    let payloads = glue
        .execute("SELECT id, name FROM users")
        .await
        .expect("SELECT from users failed");

    let mut found_alice = false;
    for payload in payloads {
        if let Payload::Select { rows, .. } = payload {
            for row in rows {
                let vals: Vec<_> = row.into_iter().collect();
                if vals.len() >= 2 {
                    if let gluesql::prelude::Value::Str(ref s) = vals[1] {
                        if s == "Alice" {
                            found_alice = true;
                        }
                    }
                }
            }
        }
    }
    assert!(
        found_alice,
        "Alice should have been inserted by migration 2"
    );
}

#[tokio::test]
async fn runner_empty_dir_applies_zero() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_empty_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("failed to create empty dir");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);
    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded on empty dir failed");
    assert_eq!(applied, 0);
}

// ── rollback tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_rollback_single_migration() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_rollback_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    // Create forward migration (14-digit version prefix).
    std::fs::write(
        dir.join("20230101000001__create_users.sql"),
        "CREATE TABLE users (id INTEGER, name TEXT)",
    )
    .unwrap();
    // Create down migration.
    std::fs::write(
        dir.join("20230101000001__create_users.down.sql"),
        "DROP TABLE users",
    )
    .unwrap();

    let storage = MemoryStorage::default();
    let mut glue = Glue::new(storage);

    let runner = MigrationRunner::new(&dir);

    // Apply the migration.
    let count = runner.run_embedded(&mut glue).await.unwrap();
    assert_eq!(count, 1);

    // Verify table exists.
    glue.execute("SELECT * FROM users").await.unwrap();

    // Roll back to version 0 (all).
    let rolled_back = runner.rollback(&mut glue, 0).await.unwrap();
    assert_eq!(rolled_back, 1);

    // Verify table is gone.
    let result = glue.execute("SELECT * FROM users").await;
    assert!(result.is_err(), "table should not exist after rollback");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_rollback_partial() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_partial_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("20230101000001__create_a.sql"),
        "CREATE TABLE a (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000001__create_a.down.sql"),
        "DROP TABLE a",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000002__create_b.sql"),
        "CREATE TABLE b (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000002__create_b.down.sql"),
        "DROP TABLE b",
    )
    .unwrap();

    let storage = MemoryStorage::default();
    let mut glue = Glue::new(storage);
    let runner = MigrationRunner::new(&dir);

    // Apply both.
    runner.run_embedded(&mut glue).await.unwrap();

    // Roll back only version 2 (keep version 1).
    let rolled_back = runner
        .rollback(&mut glue, 20_230_101_000_001_u64)
        .await
        .unwrap();
    assert_eq!(rolled_back, 1);

    // Table a should exist, table b should not.
    glue.execute("SELECT * FROM a").await.unwrap();
    let result = glue.execute("SELECT * FROM b").await;
    assert!(
        result.is_err(),
        "table b should not exist after partial rollback"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_rollback_no_down_migration_error() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_no_down_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("20230101000001__create_t.sql"),
        "CREATE TABLE t (id INTEGER)",
    )
    .unwrap();
    // No .down.sql file.

    let storage = MemoryStorage::default();
    let mut glue = Glue::new(storage);
    let runner = MigrationRunner::new(&dir);

    runner.run_embedded(&mut glue).await.unwrap();

    let result = runner.rollback(&mut glue, 0).await;
    assert!(
        matches!(result, Err(MigrationError::NoDownMigration { .. })),
        "expected NoDownMigration error, got: {result:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_orphaned_status() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_orphan_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("20230101000001__create_t.sql"),
        "CREATE TABLE t (id INTEGER)",
    )
    .unwrap();

    let storage = MemoryStorage::default();
    let mut glue = Glue::new(storage);
    let mut runner = MigrationRunner::new(&dir);

    runner.run_embedded(&mut glue).await.unwrap();

    // Remove the migration file to simulate orphan.
    std::fs::remove_file(dir.join("20230101000001__create_t.sql")).unwrap();

    // Invalidate the cache so the removed file is not returned from cache.
    runner.invalidate_cache();
    let statuses = runner.status(&mut glue).await.unwrap();
    // Version 1 is applied but no file exists on disk → should be Orphaned.
    use oxisql_migrate::runner::MigrationState;
    let orphaned_entry = statuses
        .iter()
        .find(|(_, state)| *state == MigrationState::Orphaned);
    assert!(
        orphaned_entry.is_some(),
        "expected at least one Orphaned entry but got: {statuses:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_rollback_all_multiple() {
    let dir = std::env::temp_dir().join(format!(
        "oxisql_migrate_test_rollback_all_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("20230101000001__create_x.sql"),
        "CREATE TABLE x (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000001__create_x.down.sql"),
        "DROP TABLE x",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000002__create_y.sql"),
        "CREATE TABLE y (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000002__create_y.down.sql"),
        "DROP TABLE y",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000003__create_z.sql"),
        "CREATE TABLE z (id INTEGER)",
    )
    .unwrap();
    std::fs::write(
        dir.join("20230101000003__create_z.down.sql"),
        "DROP TABLE z",
    )
    .unwrap();

    let storage = MemoryStorage::default();
    let mut glue = Glue::new(storage);
    let runner = MigrationRunner::new(&dir);

    // Apply all three.
    let applied = runner.run_embedded(&mut glue).await.unwrap();
    assert_eq!(applied, 3);

    // Roll back all (target = 0).
    let rolled_back = runner.rollback(&mut glue, 0).await.unwrap();
    assert_eq!(rolled_back, 3);

    // All tables should be gone.
    assert!(glue.execute("SELECT * FROM x").await.is_err());
    assert!(glue.execute("SELECT * FROM y").await.is_err());
    assert!(glue.execute("SELECT * FROM z").await.is_err());

    // Re-applying should work (tracker is clean).
    let reapplied = runner.run_embedded(&mut glue).await.unwrap();
    assert_eq!(
        reapplied, 3,
        "should be able to re-apply after full rollback"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ── generic Connection tracker tests ─────────────────────────────────────────

#[tokio::test]
async fn run_with_conn_embedded() {
    use oxisql_embedded::EmbeddedConnection;
    use oxisql_migrate::runner::MigrationRunner;
    use std::fs;

    let dir = unique_temp_dir("oxisql_run_with_conn_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create dir");

    // Write a simple forward migration
    fs::write(
        dir.join("20240101000001__create_foo.sql"),
        "CREATE TABLE foo (id INTEGER, label TEXT)",
    )
    .expect("write migration");

    let conn = EmbeddedConnection::open_memory().expect("open");
    let runner = MigrationRunner::new(&dir);

    let applied = runner.run_with_conn(&conn).await.expect("run_with_conn");
    assert_eq!(applied, 1);

    // Second run should be idempotent.
    let applied2 = runner.run_with_conn(&conn).await.expect("idempotent run");
    assert_eq!(applied2, 0);

    fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn rollback_with_conn_embedded() {
    use oxisql_embedded::EmbeddedConnection;
    use oxisql_migrate::runner::MigrationRunner;
    use std::fs;

    let dir = unique_temp_dir("oxisql_rollback_conn_test");
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create dir");

    fs::write(
        dir.join("20240101000001__create_bar.sql"),
        "CREATE TABLE bar (id INTEGER)",
    )
    .expect("forward");
    fs::write(
        dir.join("20240101000001__create_bar.down.sql"),
        "DROP TABLE bar",
    )
    .expect("backward");

    let conn = EmbeddedConnection::open_memory().expect("open");
    let runner = MigrationRunner::new(&dir);

    runner.run_with_conn(&conn).await.expect("apply");
    let rolled = runner.rollback_with_conn(&conn, 0).await.expect("rollback");
    assert_eq!(rolled, 1);

    fs::remove_dir_all(&dir).ok();
}

// ── new tests for Wave 9 ──────────────────────────────────────────────────────

/// Helper: generate a unique temp directory name using nanoseconds.
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

#[tokio::test]
async fn test_rollback_with_down_verification() {
    let dir = unique_temp_dir("oxisql_rollback_down");
    std::fs::create_dir_all(&dir).expect("create dir");

    // Three forward + three down migrations.
    std::fs::write(
        dir.join("20230101000001__create_t1.sql"),
        "CREATE TABLE t1 (id INTEGER)",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000001__create_t1.down.sql"),
        "DROP TABLE t1",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000002__create_t2.sql"),
        "CREATE TABLE t2 (id INTEGER)",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000002__create_t2.down.sql"),
        "DROP TABLE t2",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000003__create_t3.sql"),
        "CREATE TABLE t3 (id INTEGER)",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000003__create_t3.down.sql"),
        "DROP TABLE t3",
    )
    .expect("write");

    let mut glue = Glue::new(MemoryStorage::default());
    let mut runner = MigrationRunner::new(&dir);

    // Apply all three.
    let applied = runner.run_embedded(&mut glue).await.expect("apply");
    assert_eq!(applied, 3);

    // Roll back to version 1 (only versions > 1 are reverted).
    let rolled = runner
        .rollback(&mut glue, 20_230_101_000_001_u64)
        .await
        .expect("rollback");
    assert_eq!(rolled, 2, "versions 2 and 3 should have been rolled back");

    // Status: only version 1 should remain applied.
    let statuses = runner.status(&mut glue).await.expect("status");
    use oxisql_migrate::runner::MigrationState;
    let applied_count_status = statuses
        .iter()
        .filter(|(_, s)| *s == MigrationState::Applied)
        .count();
    assert_eq!(
        applied_count_status, 1,
        "only migration 1 should remain applied"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_checksum_mismatch_detection() {
    let dir = unique_temp_dir("oxisql_checksum");
    std::fs::create_dir_all(&dir).expect("create dir");

    let migration_path = dir.join("20230101000001__create_chk.sql");
    std::fs::write(&migration_path, "CREATE TABLE chk (id INTEGER)").expect("write");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    // Apply the migration.
    runner.run_embedded(&mut glue).await.expect("first apply");

    // Modify the migration file after it was applied.
    std::fs::write(&migration_path, "CREATE TABLE chk (id INTEGER, extra TEXT)").expect("modify");

    // Re-running should detect the checksum mismatch.
    let result = runner.run_embedded(&mut glue).await;
    assert!(
        matches!(result, Err(MigrationError::ChecksumMismatch { .. })),
        "expected ChecksumMismatch, got: {result:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_malformed_sql_migration() {
    let dir = unique_temp_dir("oxisql_malformed");
    std::fs::create_dir_all(&dir).expect("create dir");

    // Write a migration whose SQL cannot be parsed.
    std::fs::write(
        dir.join("20230101000001__bad_sql.sql"),
        "THIS IS NOT VALID SQL !!!",
    )
    .expect("write");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let result = runner.run_embedded(&mut glue).await;
    assert!(
        matches!(result, Err(MigrationError::Parse(_))),
        "expected MigrationError::Parse, got: {result:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_empty_migration_directory() {
    let dir = unique_temp_dir("oxisql_empty_dir");
    std::fs::create_dir_all(&dir).expect("create dir");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run on empty dir");
    assert_eq!(applied, 0, "empty dir should apply 0 migrations");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_new_with_options_dry_run() {
    let dir = unique_temp_dir("oxisql_dry_run");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20230101000001__create_dr.sql"),
        "CREATE TABLE dr (id INTEGER)",
    )
    .expect("write");

    let mut glue = Glue::new(MemoryStorage::default());
    let opts = MigrateOptions {
        dry_run: true,
        ..MigrateOptions::default()
    };
    let runner = MigrationRunner::new_with_options(&dir, opts);

    let applied = runner.run_embedded(&mut glue).await.expect("dry run");
    assert_eq!(applied, 0, "dry_run should apply 0 migrations");

    // Verify nothing was written to the tracker: the tracker table should not
    // even exist because dry_run skips initialize_tracker.
    let result = glue.execute("SELECT version FROM _oxisql_migrations").await;
    assert!(
        result.is_err(),
        "tracker table must not exist after dry_run"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn test_new_with_options_target_version() {
    let dir = unique_temp_dir("oxisql_target_version");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20230101000001__create_tv1.sql"),
        "CREATE TABLE tv1 (id INTEGER)",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000002__create_tv2.sql"),
        "CREATE TABLE tv2 (id INTEGER)",
    )
    .expect("write");
    std::fs::write(
        dir.join("20230101000003__create_tv3.sql"),
        "CREATE TABLE tv3 (id INTEGER)",
    )
    .expect("write");

    let mut glue = Glue::new(MemoryStorage::default());
    let opts = MigrateOptions {
        target_version: Some(20_230_101_000_002_u64),
        ..MigrateOptions::default()
    };
    let runner = MigrationRunner::new_with_options(&dir, opts);

    let applied = runner.run_embedded(&mut glue).await.expect("run");
    assert_eq!(applied, 2, "only versions 1 and 2 should be applied");

    // Version 3 table must not exist.
    let result = glue.execute("SELECT * FROM tv3").await;
    assert!(
        result.is_err(),
        "tv3 must not exist when target_version stops at 2"
    );

    // Versions 1 and 2 must be present.
    glue.execute("SELECT * FROM tv1").await.expect("tv1 exists");
    glue.execute("SELECT * FROM tv2").await.expect("tv2 exists");

    std::fs::remove_dir_all(&dir).ok();
}

// ── TrackerBackend / read_sql / From<ParserError> tests ──────────────────────

#[test]
fn migration_file_read_sql_works() {
    use std::io::Write;
    let dir = std::env::temp_dir().join("oxisql_read_sql_test");
    std::fs::create_dir_all(&dir).ok();
    let sql = "CREATE TABLE t (id INT);";
    let path = dir.join("20230101000000__test_read.sql");
    let mut f = std::fs::File::create(&path).expect("create file");
    f.write_all(sql.as_bytes()).expect("write sql");

    let mf = oxisql_migrate::scanner::MigrationFile {
        version: 20230101000000,
        name: "test_read".to_string(),
        path,
        down_path: None,
    };
    let content = mf.read_sql().expect("read_sql should succeed");
    assert_eq!(content, sql);
}

#[test]
fn from_parser_error_works() {
    use sqlparser::dialect::GenericDialect;
    use sqlparser::parser::Parser;
    let err = Parser::parse_sql(&GenericDialect {}, "SELECTT BAD").unwrap_err();
    let migration_err = oxisql_migrate::MigrationError::from(err);
    let msg = migration_err.to_string();
    assert!(
        msg.contains("parse"),
        "error message should mention parse: {msg}"
    );
}

// ── Pool integration tests (Task A) ──────────────────────────────────────────

/// Verify that `run_pooled` applies migrations through an `EmbeddedPool`.
#[cfg(feature = "pool")]
#[tokio::test]
async fn test_run_pooled_embedded() {
    use oxisql_pool::embedded::EmbeddedPool;

    let dir = unique_temp_dir("oxisql_run_pooled");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20240101000001__create_pooled_test.sql"),
        "CREATE TABLE pooled_test (id INT)",
    )
    .expect("write migration 1");
    std::fs::write(
        dir.join("20240101000002__insert_pooled_test.sql"),
        "INSERT INTO pooled_test VALUES (42)",
    )
    .expect("write migration 2");

    let pool = EmbeddedPool::new();
    let runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_pooled(&pool)
        .await
        .expect("run_pooled should succeed");
    assert_eq!(applied, 2, "expected 2 migrations applied via pool");

    // Second run should be idempotent.
    let applied2 = runner
        .run_pooled(&pool)
        .await
        .expect("idempotent run_pooled");
    assert_eq!(applied2, 0, "second run should apply 0 (idempotent)");

    std::fs::remove_dir_all(&dir).ok();
}

// ── Concurrent migration run test (Task B) ───────────────────────────────────

/// Verify that two concurrent migration runners on the same shared connection
/// serialize correctly without panicking or deadlocking.
///
/// The `EmbeddedConnection` serializes access through its internal mutex, so
/// one runner wins the initialization lock and the other detects the tracker
/// already exists and proceeds idempotently.
#[tokio::test]
async fn test_concurrent_migration_runs() {
    use oxisql_embedded::EmbeddedConnection;
    use std::sync::Arc;

    let dir = unique_temp_dir("oxisql_concurrent_migrate");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20240201000001__create_concurrent_test.sql"),
        "CREATE TABLE concurrent_test (id INTEGER)",
    )
    .expect("write migration");

    // Share a single EmbeddedConnection between two runners.  The connection's
    // internal mutex ensures serialized access even when called concurrently.
    let conn: Arc<dyn oxisql_core::Connection> =
        Arc::new(EmbeddedConnection::open_memory().expect("open_memory"));

    let dir1 = dir.clone();
    let dir2 = dir.clone();
    let c1 = conn.clone();
    let c2 = conn.clone();

    let (r1, r2) = tokio::join!(
        async move {
            let runner = MigrationRunner::new(&dir1);
            runner.run_with_conn(c1.as_ref()).await
        },
        async move {
            let runner = MigrationRunner::new(&dir2);
            runner.run_with_conn(c2.as_ref()).await
        }
    );

    // At least one runner must succeed; the second may also succeed (idempotent).
    assert!(
        r1.is_ok() || r2.is_ok(),
        "at least one concurrent runner should succeed; r1={r1:?}, r2={r2:?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ── OxidbPool integration test ───────────────────────────────────────────────

/// Verify that `run_with_pool` applies migrations through the unified `OxidbPool` enum
/// (Embedded variant).
#[cfg(feature = "pool")]
#[tokio::test]
async fn test_run_with_oxidb_pool() {
    use oxisql_pool::{embedded::EmbeddedPool, OxidbPool};

    let dir = unique_temp_dir("oxisql_migrate_pool_test");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20240501000001__create_pool_test.sql"),
        "CREATE TABLE pool_test (id INT)",
    )
    .expect("write migration");

    let pool = OxidbPool::Embedded(EmbeddedPool::new());
    let mut runner = MigrationRunner::new(&dir);

    let applied = runner
        .run_with_pool(&pool)
        .await
        .expect("run_with_pool should succeed");
    assert_eq!(applied, 1, "expected 1 migration applied via OxidbPool");

    // Second run should be idempotent.
    let applied2 = runner
        .run_with_pool(&pool)
        .await
        .expect("idempotent run_with_pool");
    assert_eq!(applied2, 0, "second run should apply 0 (idempotent)");

    std::fs::remove_dir_all(&dir).ok();
}

// ── Migration file caching tests (Task C) ────────────────────────────────────

/// Verify that repeated `status()` calls use the cached migration list
/// without re-scanning the filesystem.
#[tokio::test]
async fn test_migration_caching() {
    let dir = unique_temp_dir("oxisql_caching");
    std::fs::create_dir_all(&dir).expect("create dir");

    std::fs::write(
        dir.join("20240301000001__create_cache_test.sql"),
        "CREATE TABLE cache_test (id INTEGER)",
    )
    .expect("write migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let mut runner = MigrationRunner::new(&dir);

    // First status call — populates the internal cache.
    let s1 = runner.status(&mut glue).await.expect("first status");
    assert_eq!(s1.len(), 1, "should find 1 migration on first status");

    // Add a new file — the cache should hide it from the runner.
    std::fs::write(
        dir.join("20240301000002__create_cache_test2.sql"),
        "CREATE TABLE cache_test2 (id INTEGER)",
    )
    .expect("write second migration");

    // Second status call — must return the cached list (only 1 file visible).
    let s2 = runner
        .status(&mut glue)
        .await
        .expect("second status (cached)");
    assert_eq!(
        s2.len(),
        1,
        "cached status should still show 1 migration (cache not yet invalidated)"
    );

    // After invalidating the cache the new file becomes visible.
    runner.invalidate_cache();
    let s3 = runner
        .status(&mut glue)
        .await
        .expect("status after cache invalidation");
    assert_eq!(
        s3.len(),
        2,
        "after cache invalidation both migrations should be visible"
    );

    std::fs::remove_dir_all(&dir).ok();
}
