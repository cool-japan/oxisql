//! Migration tracking — records applied migrations in `_oxisql_migrations`.
//!
//! The tracker creates a dedicated table on first use and provides helpers
//! for querying which migrations have been applied and marking new ones.
//!
//! This implementation targets `gluesql::Glue<MemoryStorage>` for in-process
//! testing.  Future milestones will add Postgres and MySQL tracker backends.

use gluesql::prelude::{Glue, MemoryStorage, Payload};

use crate::MigrationError;

/// The name of the migration tracking table.
pub const TRACKER_TABLE: &str = "_oxisql_migrations";

/// DDL statement to create the tracking table.
const CREATE_TRACKER_SQL: &str = "
CREATE TABLE IF NOT EXISTS _oxisql_migrations (
    version  INTEGER,
    name     TEXT,
    applied_at TEXT,
    checksum TEXT
)
";

/// Create the `_oxisql_migrations` table if it does not already exist.
///
/// Safe to call multiple times — the `IF NOT EXISTS` clause makes it
/// idempotent.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the DDL fails.
pub async fn initialize_tracker(glue: &mut Glue<MemoryStorage>) -> Result<(), MigrationError> {
    glue.execute(CREATE_TRACKER_SQL)
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;
    Ok(())
}

/// Return the set of migration versions that have already been applied.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the SELECT fails.
pub async fn applied_versions(glue: &mut Glue<MemoryStorage>) -> Result<Vec<u64>, MigrationError> {
    let payloads = glue
        .execute(&format!("SELECT version FROM {TRACKER_TABLE}"))
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;

    let mut versions = Vec::new();
    for payload in payloads {
        if let Payload::Select { rows, .. } = payload {
            for row in rows {
                if let Some(gluesql::prelude::Value::I64(v)) = row.into_iter().next() {
                    versions.push(v as u64);
                }
            }
        }
    }
    Ok(versions)
}

/// Return the stored checksum for a specific migration version, if any.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the SELECT fails.
pub async fn get_checksum(
    glue: &mut Glue<MemoryStorage>,
    version: u64,
) -> Result<Option<String>, MigrationError> {
    let sql = format!("SELECT checksum FROM {TRACKER_TABLE} WHERE version = {version}");
    let payloads = glue
        .execute(&sql)
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;

    for payload in payloads {
        if let Payload::Select { rows, .. } = payload {
            for row in rows {
                if let Some(gluesql::prelude::Value::Str(s)) = row.into_iter().next() {
                    return Ok(Some(s));
                }
            }
        }
    }
    Ok(None)
}

/// Generate a real UTC timestamp in ISO 8601 format.
///
/// Uses `std::time::SystemTime` to capture the current wall-clock time,
/// formatted as `YYYY-MM-DDThh:mm:ssZ`.
pub(crate) fn now_utc_iso8601() -> String {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();

    let total_secs = duration.as_secs();
    // Break into date/time components using basic arithmetic
    let days = total_secs / 86400;
    let time_secs = total_secs % 86400;
    let hours = time_secs / 3600;
    let mins = (time_secs % 3600) / 60;
    let secs = time_secs % 60;

    // Convert days since epoch to YYYY-MM-DD using a civil calendar algorithm
    // Based on Howard Hinnant's algorithm
    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hours:02}:{mins:02}:{secs:02}Z")
}

/// Compute a checksum of the given data using FNV-1a (64-bit) combined with
/// data length.
///
/// This is a non-cryptographic hash sufficient for detecting accidental
/// modifications to migration files.  It does not provide cryptographic
/// integrity guarantees.
pub(crate) fn fnv1a_checksum(data: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
    for &byte in data {
        h ^= u64::from(byte);
        h = h.wrapping_mul(0x0100_0000_01b3); // FNV prime
    }
    format!("{:016x}:{}", h, data.len())
}

/// Record that `version` / `name` was applied at the current UTC timestamp
/// with a checksum of the migration SQL.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the INSERT fails.
pub async fn mark_applied(
    glue: &mut Glue<MemoryStorage>,
    version: u64,
    name: &str,
    checksum: &str,
) -> Result<(), MigrationError> {
    let applied_at = now_utc_iso8601();
    let escaped_name = name.replace('\'', "''");
    let escaped_checksum = checksum.replace('\'', "''");
    let sql = format!(
        "INSERT INTO {TRACKER_TABLE} VALUES ({version}, '{escaped_name}', '{applied_at}', '{escaped_checksum}')"
    );
    glue.execute(&sql)
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;
    Ok(())
}

/// Compute a checksum for a migration file's SQL content.
pub fn compute_checksum(sql: &str) -> String {
    fnv1a_checksum(sql.as_bytes())
}

/// Update the stored checksum for `version` in `_oxisql_migrations`.
///
/// Called by [`MigrationRunner::force_rechecksum`](crate::runner::MigrationRunner::force_rechecksum)
/// when a migration file has been intentionally edited and the tracker row
/// needs to reflect the new on-disk content.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the UPDATE fails.
pub async fn update_checksum(
    glue: &mut Glue<MemoryStorage>,
    version: u64,
    new_checksum: &str,
) -> Result<(), MigrationError> {
    let escaped = new_checksum.replace('\'', "''");
    let sql =
        format!("UPDATE {TRACKER_TABLE} SET checksum = '{escaped}' WHERE version = {version}");
    glue.execute(&sql)
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;
    Ok(())
}

/// Remove the tracking record for `version` from `_oxisql_migrations`.
///
/// Called after a down-migration SQL has been executed successfully to mark
/// the migration as no longer applied.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the DELETE fails.
pub async fn mark_reverted(
    glue: &mut Glue<MemoryStorage>,
    version: u64,
) -> Result<(), MigrationError> {
    let sql = format!("DELETE FROM {TRACKER_TABLE} WHERE version = {version}");
    glue.execute(&sql)
        .await
        .map_err(|e| MigrationError::Execution(e.to_string()))?;
    Ok(())
}

/// Return the total number of rows in the tracking table.
///
/// Useful for assertions in tests.
///
/// # Errors
///
/// Returns [`MigrationError::Execution`] if the query fails.
pub async fn applied_count(glue: &mut Glue<MemoryStorage>) -> Result<usize, MigrationError> {
    let versions = applied_versions(glue).await?;
    Ok(versions.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_utc_iso8601_format() {
        let ts = now_utc_iso8601();
        // Should look like YYYY-MM-DDThh:mm:ssZ
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 20);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        assert_eq!(&ts[16..17], ":");
    }

    #[test]
    fn checksum_deterministic() {
        let a = compute_checksum("CREATE TABLE t (id INT)");
        let b = compute_checksum("CREATE TABLE t (id INT)");
        assert_eq!(a, b);
    }

    #[test]
    fn checksum_different_for_different_input() {
        let a = compute_checksum("CREATE TABLE t (id INT)");
        let b = compute_checksum("CREATE TABLE t (id TEXT)");
        assert_ne!(a, b);
    }
}
