//! Named-parameter rewriting for OxiSQL.
//!
//! SQL dialects that use `:name`, `$name`, or `@name` placeholders are
//! translated to OxiSQL's canonical positional form (`$1`, `$2`, …).
//!
//! The scanner is single-pass and quote/comment–aware so that placeholder
//! characters inside string literals, double-quoted identifiers, and SQL
//! comments are left untouched.

use crate::traits::ToSqlValue;
use crate::OxiSqlError;

// ── State machine ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanState {
    Normal,
    InSingleQuote,
    InDoubleQuote,
    InLineComment,
    InBlockComment,
}

// ── rewrite_named_params ─────────────────────────────────────────────────────

/// Rewrite named SQL placeholders (`:name`, `$name`, `@name`) to OxiSQL's
/// positional form (`$1`, `$2`, …).
///
/// Returns `(rewritten_sql, ordered_names)` where `ordered_names` lists
/// each distinct name in order of first appearance.  Repeated uses of the
/// same name emit the same positional index.
///
/// The scanner is single-pass and handles:
/// - `'...'` string literals (including `''` escape sequences)
/// - `"..."` double-quoted identifiers
/// - `--` line comments
/// - `/* ... */` block comments
/// - `::` PostgreSQL cast operator (passed through verbatim)
/// - `$N` numeric positionals (left unchanged)
/// - `?` positional wildcards (left unchanged)
///
/// # Errors
///
/// Currently this function is infallible for well-formed SQL; the `Result`
/// return type is provided for forward compatibility if validation is added.
pub fn rewrite_named_params(sql: &str) -> Result<(String, Vec<String>), OxiSqlError> {
    let bytes = sql.as_bytes();
    let len = bytes.len();

    let mut out = String::with_capacity(len + 16);
    let mut names: Vec<String> = Vec::new();
    let mut state = ScanState::Normal;
    let mut i = 0usize;

    while i < len {
        let ch = bytes[i] as char;

        match state {
            // ── Inside '...' string literal ──────────────────────────────────
            ScanState::InSingleQuote => {
                out.push(ch);
                if ch == '\'' {
                    // Check for doubled-quote escape: '' inside a string.
                    if i + 1 < len && bytes[i + 1] == b'\'' {
                        out.push('\'');
                        i += 2;
                    } else {
                        state = ScanState::Normal;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }

            // ── Inside "..." double-quoted identifier ────────────────────────
            ScanState::InDoubleQuote => {
                out.push(ch);
                if ch == '"' {
                    state = ScanState::Normal;
                }
                i += 1;
            }

            // ── Inside -- line comment ────────────────────────────────────────
            ScanState::InLineComment => {
                out.push(ch);
                if ch == '\n' {
                    state = ScanState::Normal;
                }
                i += 1;
            }

            // ── Inside /* ... */ block comment ────────────────────────────────
            ScanState::InBlockComment => {
                if ch == '*' && i + 1 < len && bytes[i + 1] == b'/' {
                    out.push('*');
                    out.push('/');
                    state = ScanState::Normal;
                    i += 2;
                } else {
                    out.push(ch);
                    i += 1;
                }
            }

            // ── Normal scanning ───────────────────────────────────────────────
            ScanState::Normal => {
                match ch {
                    // Enter single-quote string
                    '\'' => {
                        out.push('\'');
                        state = ScanState::InSingleQuote;
                        i += 1;
                    }

                    // Enter double-quote identifier
                    '"' => {
                        out.push('"');
                        state = ScanState::InDoubleQuote;
                        i += 1;
                    }

                    // Possible line comment (--)
                    '-' if i + 1 < len && bytes[i + 1] == b'-' => {
                        out.push('-');
                        out.push('-');
                        state = ScanState::InLineComment;
                        i += 2;
                    }

                    // Possible block comment (/*)
                    '/' if i + 1 < len && bytes[i + 1] == b'*' => {
                        out.push('/');
                        out.push('*');
                        state = ScanState::InBlockComment;
                        i += 2;
                    }

                    // Colon — could be `::` cast or `:name` placeholder
                    ':' => {
                        if i + 1 < len && bytes[i + 1] == b':' {
                            // PostgreSQL cast `::` — emit verbatim
                            out.push(':');
                            out.push(':');
                            i += 2;
                        } else {
                            // Possible `:name` placeholder
                            let name_start = i + 1;
                            i = advance_identifier(bytes, name_start);
                            if i > name_start {
                                let name = &sql[name_start..i];
                                let idx = assign_index(&mut names, name);
                                push_positional(&mut out, idx);
                            } else {
                                // Lone `:` with no identifier — pass through
                                out.push(':');
                            }
                        }
                    }

                    // Dollar — could be `$name`, `$N` numeric positional, or bare `$`
                    '$' => {
                        let rest_start = i + 1;
                        if rest_start < len {
                            let next = bytes[rest_start];
                            if is_ident_start(next) {
                                // `$name` placeholder
                                let name_end = advance_identifier(bytes, rest_start);
                                let name = &sql[rest_start..name_end];
                                let idx = assign_index(&mut names, name);
                                push_positional(&mut out, idx);
                                i = name_end;
                            } else if next.is_ascii_digit() {
                                // `$N` numeric positional — leave as-is
                                let num_end = advance_digits(bytes, rest_start);
                                out.push_str(&sql[i..num_end]);
                                i = num_end;
                            } else {
                                // Bare `$` with no following identifier or digit
                                out.push('$');
                                i += 1;
                            }
                        } else {
                            // `$` at end of string
                            out.push('$');
                            i += 1;
                        }
                    }

                    // At-sign — `@name` placeholder
                    '@' => {
                        let name_start = i + 1;
                        let name_end = advance_identifier(bytes, name_start);
                        if name_end > name_start {
                            let name = &sql[name_start..name_end];
                            let idx = assign_index(&mut names, name);
                            push_positional(&mut out, idx);
                            i = name_end;
                        } else {
                            out.push('@');
                            i += 1;
                        }
                    }

                    // Any other character — pass through
                    _ => {
                        out.push(ch);
                        i += 1;
                    }
                }
            }
        }
    }

    Ok((out, names))
}

// ── bind_named_params ────────────────────────────────────────────────────────

/// Resolve positional bindings from a named-parameter map.
///
/// `ordered_names` is produced by [`rewrite_named_params`] and lists each
/// distinct name in the order it first appeared in the SQL.  `params` is a
/// caller-supplied slice of `(name, value)` pairs.
///
/// Returns a `Vec<&dyn ToSqlValue>` where the i-th element is the value for
/// `ordered_names[i]`, suitable for passing directly to
/// [`Connection::execute`][crate::traits::Connection::execute] or
/// [`Connection::query`][crate::traits::Connection::query].
///
/// # Errors
///
/// Returns [`OxiSqlError::Params`] if any name in `ordered_names` has no
/// corresponding entry in `params`.
pub fn bind_named_params<'a>(
    ordered_names: &[String],
    params: &'a [(&str, &'a dyn ToSqlValue)],
) -> Result<Vec<&'a dyn ToSqlValue>, OxiSqlError> {
    let mut result: Vec<&'a dyn ToSqlValue> = Vec::with_capacity(ordered_names.len());
    for name in ordered_names {
        match params.iter().find(|(k, _)| *k == name.as_str()) {
            Some((_, val)) => result.push(*val),
            None => return Err(OxiSqlError::Params(format!("missing named param: :{name}"))),
        }
    }
    Ok(result)
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return `true` if `b` is a valid start byte for a SQL identifier (`[A-Za-z_]`).
#[inline]
fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Return `true` if `b` is a valid continuation byte for a SQL identifier
/// (`[A-Za-z0-9_]`).
#[inline]
fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scan `bytes` from `start` while bytes are valid identifier characters.
/// Returns the index one past the last identifier byte.
#[inline]
fn advance_identifier(bytes: &[u8], start: usize) -> usize {
    if start >= bytes.len() || !is_ident_start(bytes[start]) {
        return start;
    }
    let mut i = start + 1;
    while i < bytes.len() && is_ident_continue(bytes[i]) {
        i += 1;
    }
    i
}

/// Scan `bytes` from `start` while bytes are ASCII digits.
/// Returns the index one past the last digit byte.
#[inline]
fn advance_digits(bytes: &[u8], start: usize) -> usize {
    let mut i = start;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// Look up `name` in `names`.  If found, return its 1-based index.
/// If not found, push `name` and return the new 1-based index.
#[inline]
fn assign_index(names: &mut Vec<String>, name: &str) -> usize {
    match names.iter().position(|n| n == name) {
        Some(pos) => pos + 1,
        None => {
            names.push(name.to_string());
            names.len()
        }
    }
}

/// Push `$<idx>` to `out`.
#[inline]
fn push_positional(out: &mut String, idx: usize) {
    out.push('$');
    out.push_str(&idx.to_string());
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;

    // Helper to call rewrite and assert on both outputs.
    fn rw(sql: &str) -> (String, Vec<String>) {
        rewrite_named_params(sql).expect("rewrite_named_params should not fail")
    }

    // A zero-sized marker type implementing ToSqlValue for binding tests.
    struct V(Value);
    impl ToSqlValue for V {
        fn to_value(&self) -> Value {
            self.0.clone()
        }
    }

    // ── rewrite_named_params tests ────────────────────────────────────────────

    #[test]
    fn test_single_named_colon() {
        let (sql, names) = rw(":name");
        assert_eq!(sql, "$1");
        assert_eq!(names, vec!["name".to_string()]);
    }

    #[test]
    fn test_single_named_dollar() {
        let (sql, names) = rw("$name");
        assert_eq!(sql, "$1");
        assert_eq!(names, vec!["name".to_string()]);
    }

    #[test]
    fn test_single_named_at() {
        let (sql, names) = rw("@name");
        assert_eq!(sql, "$1");
        assert_eq!(names, vec!["name".to_string()]);
    }

    #[test]
    fn test_repeated_name() {
        let (sql, names) = rw(":x AND :x");
        assert_eq!(sql, "$1 AND $1");
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn test_multiple_names() {
        let (sql, names) = rw(":a AND :b");
        assert_eq!(sql, "$1 AND $2");
        assert_eq!(names, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn test_double_colon_cast() {
        let (sql, names) = rw("col::TEXT");
        assert_eq!(sql, "col::TEXT");
        assert!(names.is_empty());
    }

    #[test]
    fn test_in_single_quote_ignored() {
        let (sql, names) = rw("':name'");
        assert_eq!(sql, "':name'");
        assert!(names.is_empty());
    }

    #[test]
    fn test_in_double_quote_ignored() {
        let (sql, names) = rw("\":name\"");
        assert_eq!(sql, "\":name\"");
        assert!(names.is_empty());
    }

    #[test]
    fn test_in_line_comment_ignored() {
        let (sql, names) = rw("-- :skip\nSELECT :x");
        assert_eq!(sql, "-- :skip\nSELECT $1");
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn test_in_block_comment_ignored() {
        let (sql, names) = rw("/* :skip */ :x");
        assert_eq!(sql, "/* :skip */ $1");
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn test_numeric_positional_untouched() {
        let (sql, names) = rw("SELECT $1, $2");
        assert_eq!(sql, "SELECT $1, $2");
        assert!(names.is_empty());
    }

    #[test]
    fn test_mixed() {
        // Query mixing colon, dollar-name, at-sign, a repeated name, a
        // PostgreSQL cast, a numeric positional, single-quoted string, and
        // a block comment.
        let input = "SELECT :a::INT, $b, @c, :a, $1 FROM t WHERE s = 'he said :skip' /* @skip */";
        let (sql, names) = rw(input);
        // :a → $1, ::INT stays, $b → $2, @c → $3, :a again → $1, $1 positional stays
        assert_eq!(
            sql,
            "SELECT $1::INT, $2, $3, $1, $1 FROM t WHERE s = 'he said :skip' /* @skip */"
        );
        assert_eq!(
            names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn test_escaped_single_quote_in_string() {
        // The doubled-quote '' inside a string should not end the string state.
        let (sql, names) = rw("SELECT ':don''t' = :x");
        assert_eq!(sql, "SELECT ':don''t' = $1");
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn test_double_colon_inside_double_quoted_ident() {
        // "my::col" — inside double-quote state, no name extraction.
        let (sql, names) = rw("\"my::col\"");
        assert_eq!(sql, "\"my::col\"");
        assert!(names.is_empty());
    }

    // ── bind_named_params tests ───────────────────────────────────────────────

    #[test]
    fn test_bind_named_params_ok() {
        let v1 = V(Value::I64(1));
        let v2 = V(Value::Text("hello".into()));
        let names = vec!["a".to_string(), "b".to_string()];
        let params: &[(&str, &dyn ToSqlValue)] = &[("a", &v1), ("b", &v2)];
        let result = bind_named_params(&names, params).expect("bind should succeed");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].to_value(), Value::I64(1));
        assert_eq!(result[1].to_value(), Value::Text("hello".into()));
    }

    #[test]
    fn test_bind_named_params_missing() {
        let v1 = V(Value::I64(1));
        let names = vec!["a".to_string(), "b".to_string()];
        let params: &[(&str, &dyn ToSqlValue)] = &[("a", &v1)];
        match bind_named_params(&names, params) {
            Ok(_) => panic!("expected Err for missing param 'b', got Ok"),
            Err(e) => {
                let msg = e.to_string();
                assert!(
                    msg.contains("b"),
                    "error message should mention missing name 'b': {msg}"
                );
            }
        }
    }

    #[test]
    fn test_bind_named_params_repeated() {
        // names vec with repeated entry (same name appears twice → same value used twice)
        let v1 = V(Value::I64(42));
        let names = vec!["x".to_string(), "x".to_string()];
        let params: &[(&str, &dyn ToSqlValue)] = &[("x", &v1)];
        let result = bind_named_params(&names, params).expect("bind should succeed");
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].to_value(), Value::I64(42));
        assert_eq!(result[1].to_value(), Value::I64(42));
    }

    #[test]
    fn test_bind_named_params_empty() {
        let names: Vec<String> = vec![];
        let params: &[(&str, &dyn ToSqlValue)] = &[];
        let result = bind_named_params(&names, params).expect("empty bind should succeed");
        assert!(result.is_empty());
    }
}
