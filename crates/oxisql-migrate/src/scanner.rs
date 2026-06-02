//! Migration file scanner.
//!
//! Scans a directory for `.sql` files with names matching the pattern
//! `<14-digit timestamp>__<name>.sql`, parses the version and name, and
//! returns them sorted in ascending version order.
//!
//! Companion down-migration files (`<14-digit timestamp>__<name>.down.sql`)
//! are paired with their forward-migration counterparts automatically.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::MigrationError;

/// A discovered migration file with its parsed version and descriptive name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationFile {
    /// The 14-digit timestamp parsed as a `u64`, used as the migration version.
    pub version: u64,
    /// The descriptive part of the filename (after the `__` separator, without
    /// the `.sql` extension).
    pub name: String,
    /// The full path to the forward-migration file.
    pub path: PathBuf,
    /// The full path to the companion down-migration file, if it exists.
    ///
    /// Present when a `<version>__<name>.down.sql` file exists in the same
    /// directory as the forward-migration file.
    pub down_path: Option<PathBuf>,
}

impl MigrationFile {
    /// Read and return the SQL content of the forward-migration file.
    ///
    /// # Errors
    ///
    /// Returns [`crate::MigrationError::Io`] if the file cannot be read.
    pub fn read_sql(&self) -> Result<String, crate::MigrationError> {
        std::fs::read_to_string(&self.path).map_err(crate::MigrationError::Io)
    }
}

impl PartialOrd for MigrationFile {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MigrationFile {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.version.cmp(&other.version)
    }
}

/// Scan `dir` for migration files and return them sorted by version.
///
/// Only files matching the pattern `<14 digits>__<name>.sql` are included.
/// Files whose names do not match are silently skipped.
///
/// Down-migration companion files (`<14 digits>__<name>.down.sql`) are
/// discovered in the same pass and paired with their forward counterparts via
/// [`MigrationFile::down_path`].
///
/// # Errors
///
/// Returns [`MigrationError::Io`] if the directory cannot be read.
/// Returns [`MigrationError::InvalidFilename`] if a file appears to match but
/// its version digits overflow `u64`.
pub fn scan_migrations(dir: &Path) -> Result<Vec<MigrationFile>, MigrationError> {
    let mut forward: Vec<MigrationFile> = Vec::new();
    // Map from version -> down-migration path for later pairing.
    let mut down_map: HashMap<u64, PathBuf> = HashMap::new();

    let entries = std::fs::read_dir(dir)?;
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();

        // Only process .sql files
        if !name_str.ends_with(".sql") {
            continue;
        }

        // Check for down-migration first (ends with ".down.sql").
        if name_str.ends_with(".down.sql") {
            if let Some(version) = parse_down_migration_version(&name_str) {
                down_map.insert(version, entry.path());
            }
            continue;
        }

        if let Some(mf) = parse_migration_filename(&name_str, entry.path())? {
            forward.push(mf);
        }
    }

    // Pair forward migrations with their down companions.
    for mf in &mut forward {
        mf.down_path = down_map.remove(&mf.version);
    }

    forward.sort();
    Ok(forward)
}

/// Extract the version from a `.down.sql` filename.
///
/// Expected pattern: `<14 digits>__<name>.down.sql`.
/// Returns `None` if the filename does not match.
fn parse_down_migration_version(name: &str) -> Option<u64> {
    // Strip the ".down.sql" suffix.
    let without_ext = name.strip_suffix(".down.sql")?;

    // Split at the first "__".
    let sep_pos = without_ext.find("__")?;
    let version_str = &without_ext[..sep_pos];
    let migration_name = &without_ext[sep_pos + 2..];

    // Version must be exactly 14 decimal digits.
    if version_str.len() != 14 || !version_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }

    // Name must be non-empty.
    if migration_name.is_empty() {
        return None;
    }

    version_str.parse().ok()
}

/// Attempt to parse a migration filename.
///
/// Returns `Ok(None)` if the name does not match the expected pattern.
/// Returns `Ok(Some(MigrationFile))` on success.
/// Returns `Err(MigrationError::InvalidFilename)` if the digit prefix overflows
/// `u64`.
pub(crate) fn parse_migration_filename(
    name: &str,
    path: PathBuf,
) -> Result<Option<MigrationFile>, MigrationError> {
    // Pattern: exactly 14 digits, then "__", then non-empty name, then ".sql"
    // Strip the .sql suffix first.
    let without_ext = match name.strip_suffix(".sql") {
        Some(s) => s,
        None => return Ok(None),
    };

    // Split at the first "__".
    let sep = "__";
    let sep_pos = match without_ext.find(sep) {
        Some(p) => p,
        None => return Ok(None),
    };

    let version_str = &without_ext[..sep_pos];
    let migration_name = &without_ext[sep_pos + sep.len()..];

    // Version must be exactly 14 decimal digits.
    if version_str.len() != 14 || !version_str.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }

    // Name must be non-empty.
    if migration_name.is_empty() {
        return Ok(None);
    }

    let version: u64 = version_str
        .parse()
        .map_err(|_| MigrationError::InvalidFilename(name.to_string()))?;

    Ok(Some(MigrationFile {
        version,
        name: migration_name.to_string(),
        path,
        down_path: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parse_valid_filename() {
        let path = std::env::temp_dir().join("20230101000000__create_users.sql");
        let mf = parse_migration_filename("20230101000000__create_users.sql", path)
            .unwrap()
            .unwrap();

        assert_eq!(mf.version, 20_230_101_000_000_u64);
        assert_eq!(mf.name, "create_users");
    }

    #[test]
    fn parse_invalid_no_separator() {
        let path = std::env::temp_dir().join("x.sql");
        let result = parse_migration_filename("20230101000000create_users.sql", path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_invalid_version_length() {
        let path = std::env::temp_dir().join("x.sql");
        let result = parse_migration_filename(
            "202301010000__create_users.sql", // only 12 digits
            path,
        )
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_invalid_no_sql_extension() {
        let path = std::env::temp_dir().join("x.txt");
        let result = parse_migration_filename("20230101000000__create_users.txt", path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn parse_empty_name() {
        let path = std::env::temp_dir().join("x.sql");
        let result = parse_migration_filename("20230101000000__.sql", path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn sort_order() {
        let a = MigrationFile {
            version: 20230101000000,
            name: "a".to_string(),
            path: PathBuf::from("a.sql"),
            down_path: None,
        };
        let b = MigrationFile {
            version: 20230101000001,
            name: "b".to_string(),
            path: PathBuf::from("b.sql"),
            down_path: None,
        };
        let c = MigrationFile {
            version: 20230101000002,
            name: "c".to_string(),
            path: PathBuf::from("c.sql"),
            down_path: None,
        };

        let mut files = vec![c.clone(), a.clone(), b.clone()];
        files.sort();
        assert_eq!(files, vec![a, b, c]);
    }
}
