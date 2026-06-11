//! Tests for `SqlWarning` and the `last_warnings()` interface.
//!
//! Unit tests here require no live MySQL server.  The integration test at the
//! bottom is `#[ignore]`-gated and requires a running MySQL 8.x instance.

use oxisql_core::{SqlWarning, SqlWarningLevel};

// ── Unit tests (no server) ───────────────────────────────────────────────────

#[test]
fn sql_warning_display() {
    let w = SqlWarning {
        code: 1292,
        level: SqlWarningLevel::Warning,
        message: "Incorrect date value".to_string(),
    };
    let s = w.to_string();
    assert!(s.contains("1292"), "code missing in display: {s}");
    assert!(
        s.contains("Incorrect date"),
        "message fragment missing: {s}"
    );
    assert!(s.contains("Warning"), "level missing in display: {s}");
}

#[test]
fn sql_warning_level_display() {
    assert_eq!(SqlWarningLevel::Note.to_string(), "Note");
    assert_eq!(SqlWarningLevel::Warning.to_string(), "Warning");
    assert_eq!(SqlWarningLevel::Error.to_string(), "Error");
}

#[test]
fn sql_warning_clone_and_eq() {
    let w = SqlWarning {
        code: 1000,
        level: SqlWarningLevel::Note,
        message: "informational".to_string(),
    };
    let w2 = w.clone();
    assert_eq!(w, w2);
}

#[test]
fn sql_warning_error_level_display() {
    let w = SqlWarning {
        code: 1048,
        level: SqlWarningLevel::Error,
        message: "Column 'x' cannot be null".to_string(),
    };
    let s = w.to_string();
    assert!(s.contains("Error"), "level missing: {s}");
    assert!(s.contains("1048"), "code missing: {s}");
}

/// Verify that `parse_warning_level` correctly round-trips all known variants
/// including case-insensitive matching and the unknown-fallback.
#[test]
fn parse_warning_level_round_trips() {
    use oxisql_core::parse_warning_level;
    assert_eq!(parse_warning_level("Note"), SqlWarningLevel::Note);
    assert_eq!(parse_warning_level("note"), SqlWarningLevel::Note);
    assert_eq!(parse_warning_level("NOTE"), SqlWarningLevel::Note);
    assert_eq!(parse_warning_level("Warning"), SqlWarningLevel::Warning);
    assert_eq!(parse_warning_level("warning"), SqlWarningLevel::Warning);
    assert_eq!(parse_warning_level("Error"), SqlWarningLevel::Error);
    assert_eq!(parse_warning_level("error"), SqlWarningLevel::Error);
    // Unknown → Warning (safe default)
    assert_eq!(parse_warning_level("info"), SqlWarningLevel::Warning);
    assert_eq!(parse_warning_level(""), SqlWarningLevel::Warning);
}

// ── Integration test (requires live MySQL — #[ignore]) ───────────────────────

/// Verify that inserting a value that gets truncated produces a non-empty
/// `last_warnings()` list with an appropriate warning code.
///
/// MySQL error 1265 = `WARN_DATA_TRUNCATED` (value truncated when inserting
/// into a column with `STRICT_ALL_TABLES` mode *off*, which is the default for
/// `mysql:8` Docker images when using `NO_ENGINE_SUBSTITUTION` only).
///
/// # Run with a live server
///
/// ```sh
/// docker run --rm -e MYSQL_ALLOW_EMPTY_PASSWORD=yes -p 3306:3306 mysql:8
/// cargo test -p oxisql-mysql --features integration-mysql -- --ignored
/// ```
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL server"]
async fn mysql_last_warnings_after_truncation() {
    use oxisql_core::Connection;
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    // Create a narrow VARCHAR column to force truncation warnings.
    conn.execute("DROP TABLE IF EXISTS _warn_test", &[])
        .await
        .expect("drop");
    conn.execute(
        "CREATE TABLE _warn_test (id INT PRIMARY KEY, short_str VARCHAR(5))",
        &[],
    )
    .await
    .expect("create");

    // Disable strict mode so the server produces a warning instead of an error.
    conn.execute("SET SESSION sql_mode = ''", &[])
        .await
        .expect("set sql_mode");

    // Insert a value that is longer than VARCHAR(5) → triggers warning 1265.
    conn.execute(
        "INSERT INTO _warn_test (id, short_str) VALUES (?, ?)",
        &[&1i64, &"toolongvalue"],
    )
    .await
    .expect("insert");

    let warnings = conn.last_warnings();
    assert!(
        !warnings.is_empty(),
        "expected at least one warning after truncating insert, got none"
    );
    // MySQL 1265 = WARN_DATA_TRUNCATED; 1406 = ER_DATA_TOO_LONG.
    let codes: Vec<u16> = warnings.iter().map(|w| w.code).collect();
    assert!(
        codes.contains(&1265) || codes.contains(&1406),
        "expected warning code 1265 or 1406, got: {codes:?}"
    );

    conn.execute("DROP TABLE IF EXISTS _warn_test", &[])
        .await
        .expect("cleanup");
}

/// Verify that `last_warnings()` returns an empty list after a clean statement.
#[cfg(feature = "integration-mysql")]
#[tokio::test]
#[ignore = "requires live MySQL server"]
async fn mysql_last_warnings_empty_on_clean_statement() {
    use oxisql_core::Connection;
    use oxisql_mysql::{MyConnection, TlsMode};

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect");

    conn.query("SELECT 1", &[]).await.expect("select 1");

    let warnings = conn.last_warnings();
    assert!(
        warnings.is_empty(),
        "expected no warnings after clean SELECT 1, got: {warnings:?}"
    );
}
