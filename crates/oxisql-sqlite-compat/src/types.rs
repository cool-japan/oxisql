//! Value conversions between `limbo` and `oxisql_core`.
//!
//! # Mapping table
//!
//! | Limbo [`limbo::Value`]  | OxiSQL [`oxisql_core::Value`] |
//! |-------------------------|-------------------------------|
//! | `Integer(i64)`          | `Value::I64(i64)`             |
//! | `Real(f64)`             | `Value::F64(f64)`             |
//! | `Text(String)`          | `Value::Text(String)`         |
//! | `Blob(Vec<u8>)`         | `Value::Blob(Vec<u8>)`        |
//! | `Null`                  | `Value::Null`                 |
//!
//! The reverse mapping (OxiSQL → Limbo) is used when binding `$N` parameters
//! after they have been rewritten to `?` placeholders by [`rewrite_params`].
//! Rich OxiSQL types that have no Limbo counterpart (Timestamp, Date, Time,
//! Uuid, Decimal, Json) are stored as their string or integer representations.

use limbo::Value as LimboValue;
use oxisql_core::Value as CoreValue;

use crate::error::SqliteCompatError;

/// Convert a single `limbo::Value` into an `oxisql_core::Value`.
///
/// # Errors
///
/// Returns [`SqliteCompatError::TypeMap`] for value variants that cannot be
/// represented (currently none — all five Limbo types are mapped).
pub fn limbo_to_core(val: LimboValue) -> Result<CoreValue, SqliteCompatError> {
    let v = match val {
        LimboValue::Null => CoreValue::Null,
        LimboValue::Integer(n) => CoreValue::I64(n),
        LimboValue::Real(f) => CoreValue::F64(f),
        LimboValue::Text(s) => CoreValue::Text(s),
        LimboValue::Blob(b) => CoreValue::Blob(b),
    };
    Ok(v)
}

/// Convert an `oxisql_core::Value` into a `limbo::Value`.
///
/// Rich OxiSQL types are coerced to the closest SQLite storage class:
/// - `Bool`      → `Integer` (0 / 1)
/// - `Timestamp` → `Integer` (microseconds since epoch)
/// - `Date`      → `Integer` (days since epoch)
/// - `Time`      → `Integer` (microseconds since midnight)
/// - `Uuid`      → `Text`    (hyphenated UUID string)
/// - `Decimal`   → `Text`    (decimal string)
/// - `Json`      → `Text`    (JSON string)
/// - `Array`     → `Text`    (debug representation — not round-trippable)
///
/// # Errors
///
/// Returns [`SqliteCompatError::TypeMap`] when a `Uuid` value cannot be
/// formatted (should never occur in practice).
pub fn core_to_limbo(val: &CoreValue) -> Result<LimboValue, SqliteCompatError> {
    let v = match val {
        CoreValue::Null => LimboValue::Null,
        CoreValue::Bool(b) => LimboValue::Integer(i64::from(*b)),
        CoreValue::I64(n) => LimboValue::Integer(*n),
        CoreValue::F64(f) => LimboValue::Real(*f),
        CoreValue::Text(s) => LimboValue::Text(s.clone()),
        CoreValue::Blob(b) => LimboValue::Blob(b.clone()),
        CoreValue::Timestamp(us) => LimboValue::Integer(*us),
        CoreValue::Date(days) => LimboValue::Integer(i64::from(*days)),
        CoreValue::Time(us) => LimboValue::Integer(*us),
        CoreValue::Uuid(u) => {
            // Format as hyphenated UUID string (16 bytes / u128).
            let hi = (u >> 64) as u64;
            let lo = *u as u64;
            let raw: [u8; 16] = {
                let mut buf = [0u8; 16];
                buf[..8].copy_from_slice(&hi.to_be_bytes());
                buf[8..].copy_from_slice(&lo.to_be_bytes());
                buf
            };
            let s = format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                u32::from_be_bytes(
                    raw[0..4]
                        .try_into()
                        .map_err(|_| SqliteCompatError::TypeMap("uuid slice error".into()))?
                ),
                u16::from_be_bytes(
                    raw[4..6]
                        .try_into()
                        .map_err(|_| SqliteCompatError::TypeMap("uuid slice error".into()))?
                ),
                u16::from_be_bytes(
                    raw[6..8]
                        .try_into()
                        .map_err(|_| SqliteCompatError::TypeMap("uuid slice error".into()))?
                ),
                u16::from_be_bytes(
                    raw[8..10]
                        .try_into()
                        .map_err(|_| SqliteCompatError::TypeMap("uuid slice error".into()))?
                ),
                {
                    let b = &raw[10..16];
                    ((b[0] as u64) << 40)
                        | ((b[1] as u64) << 32)
                        | ((b[2] as u64) << 24)
                        | ((b[3] as u64) << 16)
                        | ((b[4] as u64) << 8)
                        | (b[5] as u64)
                }
            );
            LimboValue::Text(s)
        }
        CoreValue::Decimal(s) | CoreValue::Json(s) => LimboValue::Text(s.clone()),
        CoreValue::Array(arr) => LimboValue::Text(format!("{arr:?}")),
    };
    Ok(v)
}

/// Split a SQL string containing multiple statements separated by `;` into
/// individual statement slices.
///
/// The splitter is token-aware: semicolons that appear inside single-quoted
/// string literals (`'...'`), double-quoted identifiers (`"..."`),
/// backtick-quoted identifiers (`` `...` ``), block comments (`/* ... */`),
/// or line comments (`-- ...`) are **not** treated as statement boundaries.
///
/// # Behaviour
///
/// - SQL-standard `''` escape sequences inside single-quoted strings are
///   recognised, so `'it''s ok; really'` is treated as one token.
/// - Unterminated strings or comments at end-of-input are passed through
///   without error; the SQL engine will surface a syntax error if needed.
/// - Empty statements (e.g. `;;`) are silently dropped.
/// - The returned slices borrow from `sql` and are already trimmed of
///   leading/trailing whitespace.
pub(crate) fn split_statements(sql: &str) -> Vec<&str> {
    let mut stmts = Vec::new();
    let bytes = sql.as_bytes();
    let len = bytes.len();
    let mut i = 0usize;
    let mut stmt_start = 0usize;

    while i < len {
        match bytes[i] {
            b'\'' => {
                // Single-quoted string: advance past the closing `'`, handling
                // `''` escape sequences.
                i += 1;
                while i < len {
                    if bytes[i] == b'\'' {
                        if i + 1 < len && bytes[i + 1] == b'\'' {
                            i += 2; // escaped ''
                        } else {
                            i += 1; // closing quote
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                // Double-quoted identifier — advance to the matching `"`.
                i += 1;
                while i < len && bytes[i] != b'"' {
                    i += 1;
                }
                if i < len {
                    i += 1; // consume closing "
                }
            }
            b'`' => {
                // Backtick-quoted identifier (MySQL-style; SQLite accepts it).
                i += 1;
                while i < len && bytes[i] != b'`' {
                    i += 1;
                }
                if i < len {
                    i += 1; // consume closing `
                }
            }
            b'-' if i + 1 < len && bytes[i + 1] == b'-' => {
                // Line comment: skip to the end of the line.
                while i < len && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < len && bytes[i + 1] == b'*' => {
                // Block comment: skip to the closing `*/`.
                i += 2;
                while i + 1 < len {
                    if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            b';' => {
                let stmt = sql[stmt_start..i].trim();
                if !stmt.is_empty() {
                    stmts.push(stmt);
                }
                i += 1;
                stmt_start = i;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Handle a trailing statement that has no terminating `;`.
    let tail = sql[stmt_start..].trim();
    if !tail.is_empty() {
        stmts.push(tail);
    }

    stmts
}

/// Rewrite OxiSQL-style `$N` positional placeholders to SQLite `?` placeholders,
/// returning the reordered parameter list.
///
/// # Behaviour
///
/// - `$1`, `$2`, … are replaced left-to-right with `?`.
/// - Parameters inside single-quoted string literals (`'...'`) are preserved.
/// - Double-quoted identifiers (`"..."`) are similarly skipped.
/// - The returned `params` vec is ordered by `$N` index (1-based), so `$2, $1`
///   in the SQL results in `[params[1], params[0]]` in the output.
///
/// # Errors
///
/// Returns [`SqliteCompatError::TypeMap`] if a placeholder references a parameter
/// index that is out of range for the supplied `params` slice.
pub fn rewrite_params(
    sql: &str,
    params: &[&dyn oxisql_core::ToSqlValue],
) -> Result<(String, Vec<LimboValue>), SqliteCompatError> {
    let mut out = String::with_capacity(sql.len());
    let mut ordered: Vec<LimboValue> = Vec::new();
    let chars: Vec<char> = sql.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        match chars[i] {
            // Single-quoted string literal — copy verbatim.
            '\'' => {
                out.push('\'');
                i += 1;
                while i < n {
                    let c = chars[i];
                    out.push(c);
                    i += 1;
                    if c == '\'' {
                        // Escaped quote ''
                        if i < n && chars[i] == '\'' {
                            out.push('\'');
                            i += 1;
                        } else {
                            break;
                        }
                    }
                }
            }
            // Double-quoted identifier — copy verbatim.
            '"' => {
                out.push('"');
                i += 1;
                while i < n && chars[i] != '"' {
                    out.push(chars[i]);
                    i += 1;
                }
                if i < n {
                    out.push('"');
                    i += 1;
                }
            }
            // Potential positional parameter $N.
            '$' => {
                i += 1;
                // Collect digits.
                let start = i;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i > start {
                    let idx_str: String = chars[start..i].iter().collect();
                    let idx: usize = idx_str.parse::<usize>().map_err(|_| {
                        SqliteCompatError::TypeMap(format!(
                            "invalid parameter placeholder: ${idx_str}"
                        ))
                    })?;
                    if idx == 0 || idx > params.len() {
                        return Err(SqliteCompatError::TypeMap(format!(
                            "parameter ${idx} is out of range (have {} params)",
                            params.len()
                        )));
                    }
                    let limbo_val = core_to_limbo(&params[idx - 1].to_value())?;
                    ordered.push(limbo_val);
                    out.push('?');
                } else {
                    // Bare `$` with no digits — pass through unchanged.
                    out.push('$');
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }

    Ok((out, ordered))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── split_statements ──────────────────────────────────────────────────────

    #[test]
    fn test_split_basic() {
        let stmts = split_statements("SELECT 1; SELECT 2");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn test_split_trailing_semicolon() {
        let stmts = split_statements("SELECT 1; SELECT 2;");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn test_split_single_statement_no_semicolon() {
        let stmts = split_statements("SELECT 1");
        assert_eq!(stmts, vec!["SELECT 1"]);
    }

    #[test]
    fn test_split_empty_statements() {
        let stmts = split_statements(";;;");
        assert!(stmts.is_empty(), "expected 0 stmts, got {stmts:?}");
    }

    #[test]
    fn test_split_whitespace_only() {
        let stmts = split_statements("   \n  ");
        assert!(stmts.is_empty());
    }

    #[test]
    fn test_split_semicolon_in_single_quoted_string() {
        let stmts = split_statements("INSERT INTO t VALUES ('a;b')");
        assert_eq!(stmts, vec!["INSERT INTO t VALUES ('a;b')"]);
    }

    #[test]
    fn test_split_escaped_single_quotes() {
        // 'it''s ok;really' contains a '' escape and a ; — neither should split.
        let stmts = split_statements("INSERT INTO t VALUES ('it''s ok;really')");
        assert_eq!(stmts, vec!["INSERT INTO t VALUES ('it''s ok;really')"]);
    }

    #[test]
    fn test_split_double_quoted_identifier() {
        let stmts = split_statements(r#"SELECT "col;name" FROM t"#);
        assert_eq!(stmts, vec![r#"SELECT "col;name" FROM t"#]);
    }

    #[test]
    fn test_split_backtick_quoted_identifier() {
        let stmts = split_statements("SELECT `col;name` FROM t");
        assert_eq!(stmts, vec!["SELECT `col;name` FROM t"]);
    }

    #[test]
    fn test_split_line_comment() {
        // The ; after -- must not be a statement boundary.
        let stmts = split_statements("SELECT 1 -- ; this is a comment\n");
        assert_eq!(stmts, vec!["SELECT 1 -- ; this is a comment"]);
    }

    #[test]
    fn test_split_line_comment_between_stmts() {
        let sql = "SELECT 1; -- comment with ; semicolon\nSELECT 2";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2, "got {stmts:?}");
        assert_eq!(stmts[0], "SELECT 1");
        assert_eq!(stmts[1], "-- comment with ; semicolon\nSELECT 2");
    }

    #[test]
    fn test_split_block_comment() {
        let stmts = split_statements("SELECT /* ; */ 1");
        assert_eq!(stmts, vec!["SELECT /* ; */ 1"]);
    }

    #[test]
    fn test_split_block_comment_spanning_stmts() {
        let sql = "SELECT 1; /* comment; with semicolons */ SELECT 2";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2, "got {stmts:?}");
        assert_eq!(stmts[0], "SELECT 1");
        assert_eq!(stmts[1], "/* comment; with semicolons */ SELECT 2");
    }

    #[test]
    fn test_split_multiple_with_trailing_no_semicolon() {
        let sql = "CREATE TABLE t (id INT);\nINSERT INTO t VALUES (1)";
        let stmts = split_statements(sql);
        assert_eq!(stmts.len(), 2, "got {stmts:?}");
        assert_eq!(stmts[0], "CREATE TABLE t (id INT)");
        assert_eq!(stmts[1], "INSERT INTO t VALUES (1)");
    }

    #[test]
    fn test_split_trims_whitespace() {
        let stmts = split_statements("  SELECT 1  ;  SELECT 2  ");
        assert_eq!(stmts, vec!["SELECT 1", "SELECT 2"]);
    }

    // ── limbo_to_core ─────────────────────────────────────────────────────────

    #[test]
    fn test_limbo_to_core_all_types() {
        assert_eq!(limbo_to_core(LimboValue::Null).unwrap(), CoreValue::Null);
        assert_eq!(
            limbo_to_core(LimboValue::Integer(42)).unwrap(),
            CoreValue::I64(42)
        );
        assert_eq!(
            limbo_to_core(LimboValue::Real(1.5)).unwrap(),
            CoreValue::F64(1.5)
        );
        assert_eq!(
            limbo_to_core(LimboValue::Text("hello".into())).unwrap(),
            CoreValue::Text("hello".into())
        );
        assert_eq!(
            limbo_to_core(LimboValue::Blob(vec![1, 2, 3])).unwrap(),
            CoreValue::Blob(vec![1, 2, 3])
        );
    }

    #[test]
    fn test_core_to_limbo_basic() {
        assert_eq!(core_to_limbo(&CoreValue::Null).unwrap(), LimboValue::Null);
        assert_eq!(
            core_to_limbo(&CoreValue::I64(7)).unwrap(),
            LimboValue::Integer(7)
        );
        assert_eq!(
            core_to_limbo(&CoreValue::F64(1.5)).unwrap(),
            LimboValue::Real(1.5)
        );
        assert_eq!(
            core_to_limbo(&CoreValue::Bool(true)).unwrap(),
            LimboValue::Integer(1)
        );
    }

    #[test]
    fn test_rewrite_params_basic() {
        let params: Vec<&dyn oxisql_core::ToSqlValue> = vec![&42i64, &"hello"];
        let (sql, vals) = rewrite_params("SELECT $1, $2", &params).unwrap();
        assert_eq!(sql, "SELECT ?, ?");
        assert_eq!(vals.len(), 2);
        assert_eq!(vals[0], LimboValue::Integer(42));
        assert_eq!(vals[1], LimboValue::Text("hello".into()));
    }

    #[test]
    fn test_rewrite_params_skips_string_literals() {
        let params: Vec<&dyn oxisql_core::ToSqlValue> = vec![&99i64];
        let (sql, vals) = rewrite_params("SELECT '$1' WHERE id = $1", &params).unwrap();
        assert_eq!(sql, "SELECT '$1' WHERE id = ?");
        assert_eq!(vals.len(), 1);
        assert_eq!(vals[0], LimboValue::Integer(99));
    }

    #[test]
    fn test_rewrite_params_out_of_range() {
        let params: Vec<&dyn oxisql_core::ToSqlValue> = vec![&1i64];
        assert!(rewrite_params("SELECT $2", &params).is_err());
    }

    #[test]
    fn test_rewrite_params_no_params() {
        let params: &[&dyn oxisql_core::ToSqlValue] = &[];
        let (sql, vals) = rewrite_params("SELECT 1", params).unwrap();
        assert_eq!(sql, "SELECT 1");
        assert!(vals.is_empty());
    }
}
