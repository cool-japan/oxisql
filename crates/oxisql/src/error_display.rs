//! Clean, user-facing rendering of [`OxiSqlError`].
//!
//! Extracted out of `crates/oxisql/src/lib.rs` to keep that file under the
//! workspace's 2000-line-per-file guideline; [`display_error`] is
//! re-exported at the crate root as `oxisql::display_error`, so this split
//! is purely organisational and does not change the public API.

use crate::OxiSqlError;

// ── Clean error display ─────────────────────────────────────────────────────

/// Render `err` for end-user display (e.g. a REPL's error line), cleaning up
/// a known upstream quirk in the embedded (GlueSQL) backend's SQL-parser
/// bridge.
///
/// GlueSQL's own `parse_sql` helper builds its `Error::Parser` variant via
/// `format!("{:#?}", inner_sqlparser_error)` — Rust's *pretty* `Debug`
/// formatter — instead of using `inner_sqlparser_error`'s own single-line
/// `Display` message. That leaks a multi-line Rust enum-variant dump (e.g.
/// `` ParserError(\n    "Expected: an SQL statement, found: SELEC at Line: 1,
/// Column: 1",\n) `` ) into what [`OxiSqlError::Execution`] then renders as
/// `"execution error: parser: <that dump>"`.
///
/// GlueSQL bakes this into a flat `String` before the error ever reaches
/// OxiSQL, so it cannot be fixed by reformatting a structured error here.
/// Instead, this function recognises the "`execution error: parser: ` +
/// debug-dump" shape specifically and rewrites it to `"execution error:
/// parser: <clean message>"`, extracting just the quoted message from the
/// dump. Every other error — including every other [`OxiSqlError`] variant,
/// and any `Execution` message that is not shaped like a debug dump (e.g.
/// the working `"execution error: fetch: table not found: …"` case) — is
/// rendered identically to `err.to_string()`.
///
/// # Example
///
/// ```rust
/// use oxisql::{display_error, OxiSqlError};
///
/// let leaky = OxiSqlError::Execution(
///     "parser: ParserError(\n    \"Expected: an SQL statement, found: SELEC at Line: 1, Column: 1\",\n)"
///         .to_string(),
/// );
/// assert_eq!(
///     display_error(&leaky),
///     "execution error: parser: Expected: an SQL statement, found: SELEC at Line: 1, Column: 1"
/// );
///
/// // Unrelated errors pass through unchanged.
/// let clean = OxiSqlError::Execution("fetch: table not found: does_not_exist".to_string());
/// assert_eq!(display_error(&clean), clean.to_string());
/// ```
#[must_use]
pub fn display_error(err: &OxiSqlError) -> String {
    let rendered = err.to_string();
    const MARKER: &str = "execution error: parser: ";
    match rendered.strip_prefix(MARKER) {
        Some(dump) => match unwrap_debug_tuple_string(dump) {
            Some(clean) => format!("{MARKER}{clean}"),
            None => rendered,
        },
        None => rendered,
    }
}

/// Recognise a Rust *pretty* `{:#?}` debug-dump of a single-field tuple
/// struct/enum-variant wrapping a string (e.g. `` ParserError(\n    "message",\n) ``,
/// or the compact `{:?}` form `` ParserError("message") ``) and, when `input`
/// consists of *only* that shape, return the unescaped inner message.
///
/// Returns `None` for any input that is not exactly one such dump (no
/// partial matches, no trailing/leading extra text), so callers can safely
/// fall back to the original text on a non-match. The identifier is not
/// hardcoded to any particular variant name (e.g. `ParserError`), so this
/// also recognises `sqlparser`'s sibling `TokenizerError` shape.
fn unwrap_debug_tuple_string(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let body = trimmed.strip_suffix(')')?;
    let open = body.find('(')?;
    let (ident, field) = (&body[..open], &body[open + 1..]);

    // The identifier must look like a Rust type/variant name: non-empty,
    // starts with an ASCII letter or underscore, and contains only
    // alphanumerics/underscores — this rejects arbitrary text that merely
    // contains a stray "(...)" and isn't actually a debug-formatted tuple.
    let first_ok = match ident.chars().next() {
        Some(c) => c.is_ascii_alphabetic() || c == '_',
        None => false,
    };
    if !first_ok || !ident.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }

    // `field` is the tuple's single-field text, e.g. `\n    "msg",\n` (pretty)
    // or `"msg"` (compact). Strip a trailing comma (present only in the
    // pretty form) and surrounding whitespace/newlines.
    let field = field.trim();
    let field = field.strip_suffix(',').unwrap_or(field).trim();

    let quoted = field.strip_prefix('"')?.strip_suffix('"')?;
    Some(unescape_debug_str(quoted))
}

/// Reverse Rust's `Debug` escaping for the small, well-known set of escapes
/// (`\"`, `\\`, `\n`, `\r`, `\t`, `\0`) that the standard library's
/// `str`/`String` `Debug` impl emits for ordinary error-message text. Any
/// other backslash escape (e.g. a `\u{..}` sequence for an unprintable
/// character) is passed through verbatim rather than mis-decoded, since
/// those do not occur in the parser-error messages this function targets.
fn unescape_debug_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('0') => out.push('\0'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod display_error_tests {
    use super::{display_error, OxiSqlError};

    /// The exact shape reported in the bug: GlueSQL's `{:#?}`-formatted
    /// `sqlparser::parser::ParserError::ParserError(String)` variant leaking
    /// through `OxiSqlError::Execution`'s "execution error: parser: " prefix.
    #[test]
    fn cleans_pretty_debug_parser_error_dump() {
        let leaky = OxiSqlError::Execution(
            "parser: ParserError(\n    \"Expected: an SQL statement, found: SELEC at Line: 1, Column: 1\",\n)"
                .to_string(),
        );
        assert_eq!(
            display_error(&leaky),
            "execution error: parser: Expected: an SQL statement, found: SELEC at Line: 1, Column: 1"
        );
    }

    /// A `TokenizerError` (the sibling `sqlparser` variant) is cleaned the
    /// same way — the matcher is not hardcoded to the `ParserError` name.
    #[test]
    fn cleans_pretty_debug_tokenizer_error_dump() {
        let leaky = OxiSqlError::Execution(
            "parser: TokenizerError(\n    \"Unterminated string literal at Line: 2, Column: 5\",\n)"
                .to_string(),
        );
        assert_eq!(
            display_error(&leaky),
            "execution error: parser: Unterminated string literal at Line: 2, Column: 5"
        );
    }

    /// The compact (non-pretty) `{:?}` single-line form is also recognised.
    #[test]
    fn cleans_compact_debug_dump() {
        let leaky = OxiSqlError::Execution(r#"parser: ParserError("boom")"#.to_string());
        assert_eq!(display_error(&leaky), "execution error: parser: boom");
    }

    /// A clean (already-`Display`-formatted) parser message is left untouched.
    #[test]
    fn leaves_clean_parser_message_untouched() {
        let clean = OxiSqlError::Execution("parser: sql parser error: bad token".to_string());
        assert_eq!(display_error(&clean), clean.to_string());
    }

    /// A working example already in the codebase (table-not-found) must
    /// render identically to plain `Display` — this function must never
    /// touch execution errors that are not shaped like a parser debug dump.
    #[test]
    fn leaves_unrelated_execution_errors_untouched() {
        let err = OxiSqlError::Execution("fetch: table not found: does_not_exist".to_string());
        assert_eq!(
            display_error(&err),
            "execution error: fetch: table not found: does_not_exist"
        );
    }

    /// Non-`Execution` variants are always passed through unchanged.
    #[test]
    fn leaves_other_variants_untouched() {
        assert_eq!(display_error(&OxiSqlError::NotConnected), "not connected");
        assert_eq!(
            display_error(&OxiSqlError::Timeout("5s".into())),
            "timeout: 5s"
        );
    }

    /// Escaped quotes/backslashes inside the dumped message are unescaped
    /// correctly, not left with stray backslashes.
    #[test]
    fn unescapes_embedded_quotes_and_backslashes() {
        let dump = r#"ParserError(
    "bad token: \"SELEC\" (path C:\\db)",
)"#;
        let leaky = OxiSqlError::Execution(format!("parser: {dump}"));
        assert_eq!(
            display_error(&leaky),
            "execution error: parser: bad token: \"SELEC\" (path C:\\db)"
        );
    }
}
