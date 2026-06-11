//! SQL literal parameterization: replace all literal values with `?`.
//!
//! The `parameterize` function is a lexical (not AST-level) transformation
//! that replaces numeric literals, string literals, `NULL`, `TRUE`, and `FALSE`
//! with positional `?` placeholders.  Quoted identifiers (`"name"`) are
//! deliberately left untouched.
//!
//! The resulting `ParameterizedSql::template` can be used as a stable cache key
//! that is independent of the concrete literal values.

// ── Output type ───────────────────────────────────────────────────────────────

/// The result of `parameterize(sql)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterizedSql {
    /// The SQL text with every literal replaced by `?`.
    pub template: String,
    /// The original literal values in order of replacement.
    pub literals: Vec<String>,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Replace all SQL literals in `sql` with `?` placeholders.
///
/// Handles:
/// * Integer, decimal, and scientific-notation numeric literals (`42`, `3.14`, `1e10`)
/// * Negative numeric literals when preceded by an operator (`= -5`)
/// * Single-quoted string literals, including `''`-escaped quotes (`'it''s'`)
/// * Double-quoted identifiers are **not** parameterized (kept as-is)
/// * `NULL`, `TRUE`, `FALSE` keywords (case-insensitive, word-boundary aware)
/// * Line comments (`-- …`) and block comments (`/* … */`) are passed through
///   unchanged (their content is never parameterized)
///
/// # Examples
///
/// ```rust
/// use oxisql_parse::parameterize;
///
/// let p = parameterize("SELECT * FROM t WHERE id = 42 AND name = 'Alice'");
/// assert_eq!(p.template, "SELECT * FROM t WHERE id = ? AND name = ?");
/// assert_eq!(p.literals, vec!["42", "'Alice'"]);
/// ```
pub fn parameterize(sql: &str) -> ParameterizedSql {
    let bytes = sql.as_bytes();
    let n = bytes.len();
    let mut template = String::with_capacity(n);
    let mut literals: Vec<String> = Vec::new();
    let mut i = 0usize;

    while i < n {
        // ── Line comment: -- until newline ────────────────────────────────
        if i + 1 < n && bytes[i] == b'-' && bytes[i + 1] == b'-' {
            while i < n && bytes[i] != b'\n' {
                template.push(bytes[i] as char);
                i += 1;
            }
            continue;
        }

        // ── Block comment: /* … */ ────────────────────────────────────────
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            template.push('/');
            template.push('*');
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                template.push(bytes[i] as char);
                i += 1;
            }
            if i + 1 < n {
                template.push('*');
                template.push('/');
                i += 2;
            }
            continue;
        }

        // ── Single-quoted string literal ──────────────────────────────────
        if bytes[i] == b'\'' {
            let start = i;
            i += 1;
            while i < n {
                if bytes[i] == b'\'' {
                    i += 1;
                    if i < n && bytes[i] == b'\'' {
                        // Escaped '' — continue inside the literal.
                        i += 1;
                    } else {
                        break; // Closing quote.
                    }
                } else {
                    i += 1;
                }
            }
            literals.push(sql[start..i].to_string());
            template.push('?');
            continue;
        }

        // ── Double-quoted identifier (keep as-is) ─────────────────────────
        if bytes[i] == b'"' {
            template.push('"');
            i += 1;
            while i < n && bytes[i] != b'"' {
                template.push(bytes[i] as char);
                i += 1;
            }
            if i < n {
                template.push('"');
                i += 1;
            }
            continue;
        }

        // ── NULL keyword ──────────────────────────────────────────────────
        if keyword_at(bytes, i, b"NULL") {
            literals.push("NULL".to_string());
            template.push('?');
            i += 4;
            continue;
        }

        // ── TRUE keyword ──────────────────────────────────────────────────
        if keyword_at(bytes, i, b"TRUE") {
            literals.push("TRUE".to_string());
            template.push('?');
            i += 4;
            continue;
        }

        // ── FALSE keyword ─────────────────────────────────────────────────
        if keyword_at(bytes, i, b"FALSE") {
            literals.push("FALSE".to_string());
            template.push('?');
            i += 5;
            continue;
        }

        // ── Positive numeric literal ──────────────────────────────────────
        if bytes[i].is_ascii_digit() {
            let start = i;
            i = consume_number(bytes, i);
            literals.push(sql[start..i].to_string());
            template.push('?');
            continue;
        }

        // ── Negative numeric literal: -<digit> in a value context ─────────
        if bytes[i] == b'-'
            && i + 1 < n
            && bytes[i + 1].is_ascii_digit()
            && in_value_context(&template)
        {
            let start = i;
            i += 1; // skip '-'
            i = consume_number(bytes, i);
            literals.push(sql[start..i].to_string());
            template.push('?');
            continue;
        }

        // ── Pass through everything else ──────────────────────────────────
        template.push(bytes[i] as char);
        i += 1;
    }

    ParameterizedSql { template, literals }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Consume a number starting at `i` (digits, optional `.digits`, optional `e[+-]digits`).
fn consume_number(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    while i < n && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i < n && bytes[i] == b'.' {
        i += 1;
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    if i < n && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < n && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        while i < n && bytes[i].is_ascii_digit() {
            i += 1;
        }
    }
    i
}

/// Check whether `keyword` (uppercase) appears at position `i` in `bytes`,
/// case-insensitively and at a word boundary.
fn keyword_at(bytes: &[u8], i: usize, keyword: &[u8]) -> bool {
    let n = keyword.len();
    if i + n > bytes.len() {
        return false;
    }
    // Case-insensitive match.
    for (j, &k) in keyword.iter().enumerate() {
        if bytes[i + j].to_ascii_uppercase() != k {
            return false;
        }
    }
    // Must not be a continuation of a longer identifier.
    let after = i + n;
    if after < bytes.len() && (bytes[after].is_ascii_alphanumeric() || bytes[after] == b'_') {
        return false;
    }
    // Must not follow an identifier character (avoid matching mid-word).
    if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
        return false;
    }
    true
}

/// Return `true` when the last non-whitespace character in `template` is an
/// operator or opening delimiter, indicating that a literal (possibly negative)
/// is expected next.
fn in_value_context(template: &str) -> bool {
    let last = template
        .bytes()
        .rev()
        .find(|&b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r'));
    matches!(
        last,
        None | Some(b'=')
            | Some(b'<')
            | Some(b'>')
            | Some(b'(')
            | Some(b',')
            | Some(b'[')
            | Some(b'+')
            | Some(b'*')
            | Some(b'/')
            | Some(b'!')
    )
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_integer_literal() {
        let p = parameterize("SELECT * FROM t WHERE id = 42");
        assert_eq!(p.template, "SELECT * FROM t WHERE id = ?");
        assert_eq!(p.literals, vec!["42"]);
    }

    #[test]
    fn test_string_literal() {
        let p = parameterize("SELECT * FROM t WHERE name = 'Alice'");
        assert_eq!(p.template, "SELECT * FROM t WHERE name = ?");
        assert_eq!(p.literals, vec!["'Alice'"]);
    }

    #[test]
    fn test_float_literal() {
        let p = parameterize("WHERE x = 3.14");
        assert_eq!(p.template, "WHERE x = ?");
        assert_eq!(p.literals, vec!["3.14"]);
    }

    #[test]
    fn test_scientific_notation() {
        let p = parameterize("WHERE x = 1.5e10");
        assert_eq!(p.template, "WHERE x = ?");
        assert_eq!(p.literals, vec!["1.5e10"]);
    }

    #[test]
    fn test_null_keyword() {
        let p = parameterize("WHERE x IS NULL");
        assert_eq!(p.template, "WHERE x IS ?");
        assert_eq!(p.literals, vec!["NULL"]);
    }

    #[test]
    fn test_true_false_keywords() {
        let p = parameterize("WHERE active = TRUE AND deleted = FALSE");
        assert_eq!(p.template, "WHERE active = ? AND deleted = ?");
        assert_eq!(p.literals, vec!["TRUE", "FALSE"]);
    }

    #[test]
    fn test_negative_integer() {
        let p = parameterize("WHERE x = -5");
        assert_eq!(p.template, "WHERE x = ?");
        assert_eq!(p.literals, vec!["-5"]);
    }

    #[test]
    fn test_escaped_quote_in_string() {
        // 'it''s' is a single string literal with an escaped apostrophe.
        let p = parameterize("WHERE name = 'it''s'");
        assert_eq!(p.template, "WHERE name = ?");
        assert_eq!(p.literals, vec!["'it''s'"]);
    }

    #[test]
    fn test_double_quoted_identifier_not_parameterized() {
        let p = parameterize(r#"SELECT "id" FROM t WHERE "id" = 1"#);
        assert!(
            p.template.contains(r#""id""#),
            "double-quoted identifier should be preserved"
        );
        assert_eq!(p.literals, vec!["1"]);
    }

    #[test]
    fn test_multiple_literals() {
        let p = parameterize("SELECT * FROM t WHERE x = 1 AND y = 'foo' AND z = NULL");
        assert_eq!(
            p.template,
            "SELECT * FROM t WHERE x = ? AND y = ? AND z = ?"
        );
        assert_eq!(p.literals.len(), 3);
    }

    #[test]
    fn test_parameterize_is_fixpoint() {
        // Parameterizing the template again should produce the same template.
        let sql = "SELECT id FROM t WHERE x = 42 AND name = 'Bob' AND active = TRUE";
        let first = parameterize(sql);
        let second = parameterize(&first.template);
        assert_eq!(
            first.template, second.template,
            "parameterize should be a fixpoint"
        );
    }

    #[test]
    fn test_line_comment_not_parameterized() {
        let p = parameterize("SELECT 1 -- this is 42\nFROM t");
        // The 1 in SELECT is parameterized; the 42 in the comment is not.
        assert_eq!(
            p.literals.len(),
            1,
            "only the numeric literal 1 should be replaced"
        );
    }

    #[test]
    fn test_subtraction_not_negative_number() {
        // "x - 5": the - follows the identifier x (not a value context).
        let p = parameterize("WHERE x - 5 > 0");
        // The 5 after - should still be parameterized (positive literal), and 0 too.
        // The - itself stays as operator.
        assert!(
            p.template.contains('-'),
            "subtraction operator should not be consumed as negative literal"
        );
    }
}
