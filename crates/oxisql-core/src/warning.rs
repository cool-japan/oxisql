//! SQL warning types surfaced by database backends.
//!
//! MySQL (and some other databases) return a warning count in every `OkPacket`.
//! Callers can retrieve the full list of warnings via
//! [`Connection::last_warnings`][crate::Connection::last_warnings] after any
//! `execute` or `query` call.

use std::fmt;

// ── SqlWarningLevel ──────────────────────────────────────────────────────────

/// Severity level of a SQL warning.
///
/// Maps directly to MySQL's `Level` column in `SHOW WARNINGS`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlWarningLevel {
    /// Informational message — no data modification occurred.
    Note,
    /// A value was adjusted or truncated; data was still stored.
    Warning,
    /// The operation failed with an error surfaced as a warning
    /// (seen inside stored procedures or `IGNORE`-mode DML).
    Error,
}

impl fmt::Display for SqlWarningLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Note => write!(f, "Note"),
            Self::Warning => write!(f, "Warning"),
            Self::Error => write!(f, "Error"),
        }
    }
}

// ── SqlWarning ───────────────────────────────────────────────────────────────

/// A SQL warning returned by the database server after a statement.
///
/// Obtain a `Vec<SqlWarning>` by calling
/// [`Connection::last_warnings`][crate::Connection::last_warnings] immediately
/// after an `execute` or `query` call.  The list is cleared before each
/// statement and repopulated afterwards (backends that support warnings).
///
/// # Example
///
/// ```rust
/// use oxisql_core::{SqlWarning, SqlWarningLevel};
///
/// let w = SqlWarning {
///     code: 1292,
///     level: SqlWarningLevel::Warning,
///     message: "Incorrect date value: '2024-13-01'".to_string(),
/// };
/// assert!(w.to_string().contains("1292"));
/// assert!(w.to_string().contains("Warning"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SqlWarning {
    /// MySQL server error/warning code (e.g. 1292 for `ER_TRUNCATED_WRONG_VALUE`).
    pub code: u16,
    /// Severity level of the warning.
    pub level: SqlWarningLevel,
    /// Human-readable message from the server.
    pub message: String,
}

impl fmt::Display for SqlWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.level, self.code, self.message)
    }
}

// ── parse helper ─────────────────────────────────────────────────────────────

/// Parse a MySQL warning level string into a [`SqlWarningLevel`].
///
/// Comparison is case-insensitive.  Any unrecognised string maps to
/// [`SqlWarningLevel::Warning`] as a safe default.
pub fn parse_warning_level(s: &str) -> SqlWarningLevel {
    match s.to_ascii_lowercase().as_str() {
        "note" => SqlWarningLevel::Note,
        "error" => SqlWarningLevel::Error,
        _ => SqlWarningLevel::Warning,
    }
}

// ── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sql_warning_display() {
        let w = SqlWarning {
            code: 1292,
            level: SqlWarningLevel::Warning,
            message: "Incorrect date value".to_string(),
        };
        let s = w.to_string();
        assert!(s.contains("1292"), "code missing: {s}");
        assert!(s.contains("Incorrect date"), "message missing: {s}");
        assert!(s.contains("Warning"), "level missing: {s}");
    }

    #[test]
    fn sql_warning_level_display() {
        assert_eq!(SqlWarningLevel::Note.to_string(), "Note");
        assert_eq!(SqlWarningLevel::Warning.to_string(), "Warning");
        assert_eq!(SqlWarningLevel::Error.to_string(), "Error");
    }

    #[test]
    fn parse_warning_level_variants() {
        assert_eq!(parse_warning_level("note"), SqlWarningLevel::Note);
        assert_eq!(parse_warning_level("Note"), SqlWarningLevel::Note);
        assert_eq!(parse_warning_level("NOTE"), SqlWarningLevel::Note);
        assert_eq!(parse_warning_level("warning"), SqlWarningLevel::Warning);
        assert_eq!(parse_warning_level("Warning"), SqlWarningLevel::Warning);
        assert_eq!(parse_warning_level("WARNING"), SqlWarningLevel::Warning);
        assert_eq!(parse_warning_level("error"), SqlWarningLevel::Error);
        assert_eq!(parse_warning_level("Error"), SqlWarningLevel::Error);
        assert_eq!(parse_warning_level("ERROR"), SqlWarningLevel::Error);
        // Unknown defaults to Warning
        assert_eq!(parse_warning_level("unknown"), SqlWarningLevel::Warning);
        assert_eq!(parse_warning_level(""), SqlWarningLevel::Warning);
    }

    #[test]
    fn sql_warning_debug_clone_eq() {
        let w = SqlWarning {
            code: 1000,
            level: SqlWarningLevel::Note,
            message: "test note".to_string(),
        };
        let w2 = w.clone();
        assert_eq!(w, w2);
        // Debug doesn't panic
        let _ = format!("{:?}", w);
    }
}
