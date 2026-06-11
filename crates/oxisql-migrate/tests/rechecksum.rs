//! Integration tests for `MigrationRunner::force_rechecksum`.

#![cfg(feature = "migrate")]

use gluesql::prelude::{Glue, MemoryStorage};
use oxisql_migrate::runner::{MigrationRunner, MigrationState};

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

/// Apply a migration, modify the file, confirm `Modified` state, then call
/// `force_rechecksum` and confirm the state returns to `Applied`.
#[tokio::test]
async fn test_force_rechecksum() {
    let dir = unique_temp_dir("oxisql_force_rechecksum");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let migration_path = dir.join("20240701000001__rechecksum_test.sql");
    let original_sql = "CREATE TABLE rechecksum_tbl (id INTEGER)";
    std::fs::write(&migration_path, original_sql).expect("write migration");

    let mut glue = Glue::new(MemoryStorage::default());
    let mut runner = MigrationRunner::new(&dir);

    // Apply the migration.
    let applied = runner
        .run_embedded(&mut glue)
        .await
        .expect("run_embedded should succeed");
    assert_eq!(applied, 1, "migration should be applied");

    // Verify it shows as Applied.
    let statuses = runner.status(&mut glue).await.expect("status");
    let (_, state) = statuses
        .iter()
        .find(|(mf, _)| mf.version == 20_240_701_000_001_u64)
        .expect("migration entry not found in status");
    assert_eq!(
        *state,
        MigrationState::Applied,
        "migration should be Applied before modification"
    );

    // Modify the migration file on disk.
    let modified_sql = "CREATE TABLE rechecksum_tbl (id INTEGER, extra TEXT)";
    std::fs::write(&migration_path, modified_sql).expect("modify migration");

    // Confirm the runner now reports Modified.
    // `invalidate_cache` is not needed because only content changed, not files.
    let statuses2 = runner.status(&mut glue).await.expect("status after modify");
    let (_, state2) = statuses2
        .iter()
        .find(|(mf, _)| mf.version == 20_240_701_000_001_u64)
        .expect("migration entry not found after modify");
    assert_eq!(
        *state2,
        MigrationState::Modified,
        "migration should be Modified after file content changed"
    );

    // Force-rechecksum: updates tracker to match the new file content.
    runner
        .force_rechecksum(&mut glue, 20_240_701_000_001_u64)
        .await
        .expect("force_rechecksum should succeed");

    // Status should now show Applied again.
    let statuses3 = runner
        .status(&mut glue)
        .await
        .expect("status after rechecksum");
    let (_, state3) = statuses3
        .iter()
        .find(|(mf, _)| mf.version == 20_240_701_000_001_u64)
        .expect("migration entry not found after rechecksum");
    assert_eq!(
        *state3,
        MigrationState::Applied,
        "migration should be Applied after force_rechecksum"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `force_rechecksum` on a version that does not exist on disk returns an I/O
/// error rather than panicking or silently succeeding.
#[tokio::test]
async fn test_force_rechecksum_missing_version_returns_error() {
    let dir = unique_temp_dir("oxisql_rechecksum_missing");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let mut glue = Glue::new(MemoryStorage::default());
    let runner = MigrationRunner::new(&dir);

    let result = runner
        .force_rechecksum(&mut glue, 99_999_999_000_001_u64)
        .await;
    assert!(
        result.is_err(),
        "force_rechecksum on missing version should return an error"
    );
}

/// Multiple migrations: rechecksum only the Modified one; the Applied one stays
/// Applied and the Pending one stays Pending.
#[tokio::test]
async fn test_force_rechecksum_selective() {
    let dir = unique_temp_dir("oxisql_rechecksum_selective");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let path1 = dir.join("20240702000001__sel_tbl1.sql");
    let path2 = dir.join("20240702000002__sel_tbl2.sql");
    let path3 = dir.join("20240702000003__sel_tbl3.sql");

    std::fs::write(&path1, "CREATE TABLE sel1 (id INTEGER)").expect("write 1");
    std::fs::write(&path2, "CREATE TABLE sel2 (id INTEGER)").expect("write 2");
    // path3 is written AFTER the initial run so it's pending.

    let mut glue = Glue::new(MemoryStorage::default());
    let mut runner = MigrationRunner::new(&dir);

    // Apply migrations 1 and 2.
    let applied = runner.run_embedded(&mut glue).await.expect("initial run");
    assert_eq!(applied, 2);

    // Add migration 3 now (it becomes Pending).
    std::fs::write(&path3, "CREATE TABLE sel3 (id INTEGER)").expect("write 3");
    runner.invalidate_cache();

    // Modify only migration 2.
    std::fs::write(&path2, "CREATE TABLE sel2 (id INTEGER, extra TEXT)").expect("modify 2");

    // Confirm states: 1=Applied, 2=Modified, 3=Pending.
    let statuses = runner.status(&mut glue).await.expect("status");
    let find = |ver: u64| {
        statuses
            .iter()
            .find(|(mf, _)| mf.version == ver)
            .map(|(_, s)| s.clone())
    };
    assert_eq!(find(20_240_702_000_001_u64), Some(MigrationState::Applied));
    assert_eq!(find(20_240_702_000_002_u64), Some(MigrationState::Modified));
    assert_eq!(find(20_240_702_000_003_u64), Some(MigrationState::Pending));

    // Rechecksum only migration 2.
    runner
        .force_rechecksum(&mut glue, 20_240_702_000_002_u64)
        .await
        .expect("force_rechecksum on migration 2");

    // Re-check: 1=Applied, 2=Applied, 3=Pending.
    let statuses2 = runner
        .status(&mut glue)
        .await
        .expect("status after rechecksum");
    let find2 = |ver: u64| {
        statuses2
            .iter()
            .find(|(mf, _)| mf.version == ver)
            .map(|(_, s)| s.clone())
    };
    assert_eq!(find2(20_240_702_000_001_u64), Some(MigrationState::Applied));
    assert_eq!(find2(20_240_702_000_002_u64), Some(MigrationState::Applied));
    assert_eq!(find2(20_240_702_000_003_u64), Some(MigrationState::Pending));

    std::fs::remove_dir_all(&dir).ok();
}
