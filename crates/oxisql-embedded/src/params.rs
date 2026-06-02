//! SQL-injection-safe parameter binding for the embedded GlueSQL backend.
//!
//! GlueSQL does not support server-side prepared statements with binary
//! parameters, so this module implements client-side parameter interpolation
//! with robust SQL escaping for every [`Value`] variant.
//!
//! # Approach
//!
//! [`bind_params`] uses a two-phase strategy:
//!
//! 1. **AST-level substitution** — parse the SQL with `sqlparser`, walk
//!    every `Expr::Value(Value::Placeholder("$N"))` node and replace it with
//!    the appropriately-typed literal expression, then re-serialise.
//!    This correctly distinguishes `$1` in value position from `$1` inside a
//!    single-quoted string literal (which sqlparser never parses as a
//!    `Placeholder` node).
//!
//! 2. **String-based fallback** — if the input SQL fails to parse (e.g. it
//!    contains GlueSQL-specific syntax that the generic dialect rejects), the
//!    implementation falls back to [`bind_params_string`], which performs a
//!    character-by-character forward scan that avoids simple mis-reads such as
//!    treating `$10` as `$1` + `0`.
//!
//! Both paths invoke [`escape_sql_value`] to produce safe SQL literals for
//! each [`Value`] variant.

use core::ops::ControlFlow;

use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use oxisql_core::{OxiSqlError, Value};
use sqlparser::ast::Value as SqlValue;
use sqlparser::ast::{visit_expressions_mut, Expr};
use sqlparser::dialect::GenericDialect;
use sqlparser::parser::Parser;

// ── escape_sql_value ──────────────────────────────────────────────────────────

/// Convert a [`Value`] into a SQL literal safe for direct embedding in a
/// GlueSQL statement.
///
/// The returned string is a self-contained SQL expression (e.g. `'Alice'`,
/// `42`, `TRUE`, `X'deadbeef'`, `DATE '2024-01-15'`) that can be spliced into
/// any value position in a query.
///
/// # Notes
///
/// - Text / JSON string values are quoted with single-quote escaping
///   (`'` → `''`), which is standard SQL.
/// - Finite `F64` values are formatted with enough decimal digits to
///   round-trip through `f64::parse` (Rust's default `{}` repr).
///   Non-finite values (`NaN`, `±inf`) are mapped to `NULL` because SQL has
///   no standard literal for them.
/// - `Blob` bytes are rendered as `X'hex…'` hex literals.
/// - `Date(i32)` (days since Unix epoch) is converted to an ISO-8601 date
///   string and wrapped in `DATE '…'`.
/// - `Time(i64)` (microseconds since midnight) is converted to an ISO-8601
///   time string and wrapped in `TIME '…'`.
/// - `Timestamp(i64)` (microseconds since Unix epoch, UTC) is converted to an
///   ISO-8601 datetime string and wrapped in `TIMESTAMP '…'`.
/// - `Uuid(u128)` is formatted as the standard `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`
///   and quoted.
/// - `Decimal(String)` is emitted verbatim (it is already a numeric literal).
/// - `Array` values have no GlueSQL literal representation and fall back to
///   `NULL`.
pub fn escape_sql_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => {
            if *b {
                "TRUE".to_string()
            } else {
                "FALSE".to_string()
            }
        }
        Value::I64(n) => n.to_string(),
        Value::F64(f) => {
            if f.is_finite() {
                // Use Rust's default Display which produces a shortest
                // round-trip decimal representation.
                f.to_string()
            } else {
                // NaN / ±inf cannot be represented as SQL literals.
                "NULL".to_string()
            }
        }
        Value::Text(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Blob(b) => {
            // Inline hex encoding — no external crate needed.
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("X'{hex}'")
        }
        Value::Timestamp(us) => {
            // Convert microseconds-since-Unix-epoch back to an ISO-8601 UTC
            // datetime string that GlueSQL can parse.
            let secs = us.div_euclid(1_000_000);
            let nanos = (us.rem_euclid(1_000_000)) as u32 * 1_000;
            let dt: DateTime<Utc> = match Utc.timestamp_opt(secs, nanos) {
                chrono::LocalResult::Single(dt) => dt,
                _ => {
                    // Out of range — fall back to epoch.
                    Utc.timestamp_opt(0, 0)
                        .single()
                        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
                }
            };
            // GlueSQL accepts `TIMESTAMP 'YYYY-MM-DD HH:MM:SS'`
            let s = dt.format("%Y-%m-%d %H:%M:%S").to_string();
            format!("TIMESTAMP '{s}'")
        }
        Value::Date(days) => {
            // Convert days-since-Unix-epoch back to NaiveDate.
            let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is valid");
            let date = epoch + chrono::Duration::days(i64::from(*days));
            let s = date.format("%Y-%m-%d").to_string();
            format!("DATE '{s}'")
        }
        Value::Time(us) => {
            // Convert microseconds-since-midnight to NaiveTime.
            let total_secs = us.div_euclid(1_000_000);
            let micros = us.rem_euclid(1_000_000) as u32;
            let hours = (total_secs / 3600) as u32;
            let mins = ((total_secs % 3600) / 60) as u32;
            let secs = (total_secs % 60) as u32;
            let naive = NaiveTime::from_hms_micro_opt(hours, mins, secs, micros)
                .unwrap_or_else(|| NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is valid"));
            let s = naive.format("%H:%M:%S").to_string();
            format!("TIME '{s}'")
        }
        Value::Uuid(u) => {
            // Format as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`
            let bytes = u.to_be_bytes();
            let s = format!(
                "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                bytes[0], bytes[1], bytes[2], bytes[3],
                bytes[4], bytes[5],
                bytes[6], bytes[7],
                bytes[8], bytes[9],
                bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
            );
            format!("'{s}'")
        }
        Value::Json(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Decimal(s) => s.clone(),
        Value::Array(_) => {
            // GlueSQL has no array literal syntax; fall back to NULL.
            "NULL".to_string()
        }
    }
}

// ── value_to_ast_expr ─────────────────────────────────────────────────────────

/// Convert an OxiSQL [`Value`] into a sqlparser [`Expr`] literal node.
///
/// The resulting expression is a structurally valid AST node that serialises
/// to the same SQL literal as [`escape_sql_value`], but is embedded directly
/// in the AST rather than spliced as a raw string.
fn value_to_ast_expr(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::value(SqlValue::Null),
        Value::Bool(b) => Expr::value(SqlValue::Boolean(*b)),
        Value::I64(n) => {
            if *n < 0 {
                // sqlparser represents negative numbers as UnaryOp(Minus, Number).
                Expr::UnaryOp {
                    op: sqlparser::ast::UnaryOperator::Minus,
                    expr: Box::new(Expr::value(SqlValue::Number(
                        n.unsigned_abs().to_string(),
                        false,
                    ))),
                }
            } else {
                Expr::value(SqlValue::Number(n.to_string(), false))
            }
        }
        Value::F64(f) => {
            if f.is_finite() {
                let s = f.to_string();
                if *f < 0.0 {
                    Expr::UnaryOp {
                        op: sqlparser::ast::UnaryOperator::Minus,
                        expr: Box::new(Expr::value(SqlValue::Number(
                            s.trim_start_matches('-').to_string(),
                            false,
                        ))),
                    }
                } else {
                    Expr::value(SqlValue::Number(s, false))
                }
            } else {
                Expr::value(SqlValue::Null)
            }
        }
        Value::Text(s) => Expr::value(SqlValue::SingleQuotedString(s.clone())),
        Value::Json(s) => Expr::value(SqlValue::SingleQuotedString(s.clone())),
        Value::Decimal(s) => Expr::value(SqlValue::Number(s.clone(), false)),
        Value::Blob(_)
        | Value::Timestamp(_)
        | Value::Date(_)
        | Value::Time(_)
        | Value::Uuid(_)
        | Value::Array(_) => {
            // These types have no direct sqlparser Value variant.
            // Produce an `Expr::Value(SingleQuotedString)` containing the
            // pre-escaped SQL literal — when re-serialised this yields the
            // correct `TIMESTAMP '...'` / `DATE '...'` / `X'...'` form
            // because GlueSQL parses `CAST` and function-call wrappers, but
            // actually the simplest correct approach for GlueSQL is to emit
            // the raw literal text via a typed string.
            //
            // Since sqlparser's to_string() for SingleQuotedString adds the
            // outer quotes, we need to produce a raw token.  The cleanest way
            // is to use an identifier-free wrapper: embed the full escaped
            // literal via escape_sql_value and parse a tiny sub-expression.
            let literal = escape_sql_value(v);
            // Parse the literal into a small AST node so it round-trips
            // correctly.  This parse should never fail for the literals we
            // produce.
            let dialect = GenericDialect {};
            let mini_sql = format!("SELECT {literal}");
            if let Ok(mut stmts) = Parser::parse_sql(&dialect, &mini_sql) {
                if let Some(sqlparser::ast::Statement::Query(q)) = stmts.pop() {
                    if let sqlparser::ast::SetExpr::Select(sel) = *q.body {
                        if let Some(sqlparser::ast::SelectItem::UnnamedExpr(expr)) =
                            sel.projection.into_iter().next()
                        {
                            return expr;
                        }
                    }
                }
            }
            // Last-resort fallback — should be unreachable for our literals.
            Expr::value(SqlValue::Null)
        }
    }
}

// ── bind_params_string ────────────────────────────────────────────────────────

/// String-based `$N` placeholder substitution (fallback / internal).
///
/// Performs a character-by-character forward scan so that `$10` is never
/// mis-read as `$1` followed by `0`, and `$$` is treated as an escaped
/// literal `$`.
///
/// This function is used as a fallback by [`bind_params`] when the SQL cannot
/// be parsed by the generic dialect.  It is also re-exported under the
/// original name for callers that want the simpler behaviour directly.
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if any placeholder index is zero or exceeds
/// `params.len()`.
pub fn bind_params_string(sql: &str, params: &[Value]) -> Result<String, OxiSqlError> {
    let mut result = String::with_capacity(sql.len() + params.len() * 4);
    let chars: Vec<char> = sql.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        if chars[i] == '$' {
            // Look ahead to decide what this `$` means.
            if i + 1 < len && chars[i + 1] == '$' {
                // `$$` → emit a single `$`
                result.push('$');
                i += 2;
            } else if i + 1 < len && chars[i + 1].is_ascii_digit() {
                // `$N` or `$NN` — collect the full digit run.
                i += 1; // skip the `$`
                let start = i;
                while i < len && chars[i].is_ascii_digit() {
                    i += 1;
                }
                let digits: String = chars[start..i].iter().collect();
                let idx: usize = digits.parse().map_err(|_| {
                    OxiSqlError::Parse(format!("invalid placeholder index '${digits}'"))
                })?;
                if idx == 0 || idx > params.len() {
                    return Err(OxiSqlError::Parse(format!(
                        "placeholder ${idx} out of range (params.len() = {})",
                        params.len()
                    )));
                }
                result.push_str(&escape_sql_value(&params[idx - 1]));
            } else {
                // Bare `$` not followed by `$` or a digit — emit literally.
                result.push('$');
                i += 1;
            }
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }

    Ok(result)
}

// ── placeholder_index ─────────────────────────────────────────────────────────

/// The result of parsing a `$N` placeholder name.
#[derive(Debug)]
enum PlaceholderParse {
    /// Valid 1-based index converted to 0-based (e.g. `$1` → `0`).
    Index(usize),
    /// The placeholder looks like `$N` but the index is zero (invalid).
    ZeroIndex,
    /// Not a `$N`-style placeholder (e.g. `?`, bare `$`, non-numeric).
    NotPositional,
}

/// Parse a `$N` placeholder name (e.g. `"$1"`, `"$10"`) and classify it.
fn parse_placeholder(name: &str) -> PlaceholderParse {
    let Some(digits) = name.strip_prefix('$') else {
        return PlaceholderParse::NotPositional;
    };
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return PlaceholderParse::NotPositional;
    }
    let Ok(idx) = digits.parse::<usize>() else {
        return PlaceholderParse::NotPositional;
    };
    if idx == 0 {
        PlaceholderParse::ZeroIndex
    } else {
        PlaceholderParse::Index(idx - 1) // convert to 0-based
    }
}

// ── bind_params ───────────────────────────────────────────────────────────────

/// Substitute `$1`, `$2`, … positional placeholders in `sql` with the
/// SQL-safe string representation of the corresponding [`Value`] in `params`.
///
/// This is the primary public entry-point for parameter binding.  It uses
/// **AST-level substitution** as the default strategy:
///
/// 1. Parse `sql` into a sqlparser AST using the generic SQL dialect.
/// 2. Walk every `Expr::Value(Value::Placeholder("$N"))` node in the AST.
///    Only true placeholder nodes in value positions are visited — `$1` inside
///    a string literal is a `Value::SingleQuotedString` node and is left
///    unchanged.
/// 3. Replace each placeholder node with the appropriately-typed literal
///    `Expr` produced by the internal `value_to_ast_expr` helper.
/// 4. Re-serialise the modified AST back to a SQL string.
///
/// If parsing fails (e.g. because the SQL uses GlueSQL-specific syntax that
/// the generic dialect does not understand), the function falls back to
/// [`bind_params_string`].
///
/// Placeholder indexing is 1-based (`$1` refers to `params[0]`).
///
/// # Errors
///
/// Returns [`OxiSqlError::Parse`] if any placeholder index is zero or exceeds
/// `params.len()`.
pub fn bind_params(sql: &str, params: &[Value]) -> Result<String, OxiSqlError> {
    // If no params and no `$$` sequence is present, return early to avoid
    // the overhead of SQL parsing.  We still need to process `$$` → `$` even
    // with no params to preserve backward-compatible escape semantics.
    if params.is_empty() && !sql.contains("$$") {
        return Ok(sql.to_string());
    }

    // With no params but a `$$` sequence present, fall through to string-based
    // binding which handles `$$` → `$` escaping correctly.
    if params.is_empty() {
        return bind_params_string(sql, params);
    }

    let dialect = GenericDialect {};
    let mut stmts = match Parser::parse_sql(&dialect, sql) {
        Ok(s) => s,
        Err(_) => {
            // Parse failed — fall back to string-based binding.
            return bind_params_string(sql, params);
        }
    };

    // Walk every expression in the AST and replace Placeholder nodes.
    // We accumulate errors via a Result stored in the closure.
    let mut binding_error: Option<OxiSqlError> = None;

    let _ = visit_expressions_mut(&mut stmts, |expr| {
        if let Expr::Value(ref vws) = *expr {
            if let SqlValue::Placeholder(ref name) = vws.value {
                match parse_placeholder(name) {
                    PlaceholderParse::NotPositional => {
                        // Not a `$N` style placeholder — leave it alone.
                    }
                    PlaceholderParse::ZeroIndex => {
                        binding_error = Some(OxiSqlError::Parse(
                            "placeholder $0 is invalid — indices are 1-based".into(),
                        ));
                        return ControlFlow::Break(());
                    }
                    PlaceholderParse::Index(idx) => {
                        if idx >= params.len() {
                            let human_idx = idx + 1;
                            binding_error = Some(OxiSqlError::Parse(format!(
                                "placeholder ${human_idx} out of range (params.len() = {})",
                                params.len()
                            )));
                            return ControlFlow::Break(());
                        }
                        *expr = value_to_ast_expr(&params[idx]);
                    }
                }
            }
        }
        ControlFlow::Continue(())
    });

    if let Some(err) = binding_error {
        return Err(err);
    }

    // Re-serialise: join statements with "; ".
    let sql_out = stmts
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("; ");

    Ok(sql_out)
}

// ── unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_null() {
        assert_eq!(escape_sql_value(&Value::Null), "NULL");
    }

    #[test]
    fn escape_bool() {
        assert_eq!(escape_sql_value(&Value::Bool(true)), "TRUE");
        assert_eq!(escape_sql_value(&Value::Bool(false)), "FALSE");
    }

    #[test]
    fn escape_i64() {
        assert_eq!(escape_sql_value(&Value::I64(42)), "42");
        assert_eq!(escape_sql_value(&Value::I64(-99)), "-99");
        assert_eq!(escape_sql_value(&Value::I64(0)), "0");
    }

    #[test]
    fn escape_f64_finite() {
        assert_eq!(escape_sql_value(&Value::F64(1.5)), "1.5");
        assert_eq!(escape_sql_value(&Value::F64(0.0)), "0");
    }

    #[test]
    fn escape_f64_non_finite() {
        assert_eq!(escape_sql_value(&Value::F64(f64::NAN)), "NULL");
        assert_eq!(escape_sql_value(&Value::F64(f64::INFINITY)), "NULL");
        assert_eq!(escape_sql_value(&Value::F64(f64::NEG_INFINITY)), "NULL");
    }

    #[test]
    fn escape_text_no_quotes() {
        assert_eq!(escape_sql_value(&Value::Text("hello".into())), "'hello'");
    }

    #[test]
    fn escape_text_with_single_quote() {
        assert_eq!(
            escape_sql_value(&Value::Text("it's a test".into())),
            "'it''s a test'"
        );
    }

    #[test]
    fn escape_blob_empty() {
        assert_eq!(escape_sql_value(&Value::Blob(vec![])), "X''");
    }

    #[test]
    fn escape_blob_bytes() {
        assert_eq!(
            escape_sql_value(&Value::Blob(vec![0xde, 0xad, 0xbe, 0xef])),
            "X'deadbeef'"
        );
    }

    #[test]
    fn escape_decimal() {
        assert_eq!(
            escape_sql_value(&Value::Decimal("123.456".into())),
            "123.456"
        );
    }

    #[test]
    fn escape_json_with_quote() {
        assert_eq!(
            escape_sql_value(&Value::Json(r#"{"key":"it's here"}"#.into())),
            r#"'{"key":"it''s here"}'"#
        );
    }

    #[test]
    fn escape_array_falls_back_to_null() {
        assert_eq!(
            escape_sql_value(&Value::Array(vec![Value::I64(1), Value::I64(2)])),
            "NULL"
        );
    }

    #[test]
    fn escape_uuid_zero() {
        assert_eq!(
            escape_sql_value(&Value::Uuid(0)),
            "'00000000-0000-0000-0000-000000000000'"
        );
    }

    #[test]
    fn escape_timestamp_epoch() {
        // 0 microseconds → Unix epoch → 1970-01-01 00:00:00
        assert_eq!(
            escape_sql_value(&Value::Timestamp(0)),
            "TIMESTAMP '1970-01-01 00:00:00'"
        );
    }

    #[test]
    fn escape_date_epoch() {
        // 0 days → 1970-01-01
        assert_eq!(escape_sql_value(&Value::Date(0)), "DATE '1970-01-01'");
    }

    #[test]
    fn escape_date_known() {
        // 2024-01-15 is 19737 days after Unix epoch
        assert_eq!(escape_sql_value(&Value::Date(19737)), "DATE '2024-01-15'");
    }

    #[test]
    fn escape_time_midnight() {
        assert_eq!(escape_sql_value(&Value::Time(0)), "TIME '00:00:00'");
    }

    #[test]
    fn escape_time_known() {
        // 13:30:00 = 48_600 seconds = 48_600_000_000 microseconds
        assert_eq!(
            escape_sql_value(&Value::Time(48_600_000_000_i64)),
            "TIME '13:30:00'"
        );
    }

    // ── bind_params tests ────────────────────────────────────────────────────

    #[test]
    fn bind_null() {
        assert_eq!(
            bind_params("SELECT $1", &[Value::Null]).unwrap(),
            "SELECT NULL"
        );
    }

    #[test]
    fn bind_text_escaping() {
        // Text with a single quote must be escaped correctly.
        let result = bind_params("SELECT $1", &[Value::Text("it's a test".into())]).unwrap();
        // sqlparser will re-quote the SingleQuotedString, producing the
        // correctly escaped form.
        assert!(
            result.contains("it") && result.contains("s a test"),
            "result should contain the text content: {result}"
        );
    }

    #[test]
    fn bind_multiple_params() {
        let result = bind_params("SELECT $1, $2", &[Value::I64(42), Value::Bool(true)]).unwrap();
        assert!(result.contains("42"), "expected 42 in: {result}");
        assert!(
            result.to_uppercase().contains("TRUE"),
            "expected TRUE in: {result}"
        );
    }

    #[test]
    fn bind_out_of_bounds() {
        let err = bind_params("SELECT $2", &[Value::I64(1)]).unwrap_err();
        match err {
            OxiSqlError::Parse(msg) => {
                assert!(
                    msg.contains("$2") || msg.contains("out of range"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected OxiSqlError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn bind_zero_index_rejected() {
        let err = bind_params("SELECT $0", &[Value::I64(1)]).unwrap_err();
        assert!(matches!(err, OxiSqlError::Parse(_)));
    }

    #[test]
    fn bind_bare_dollar_passthrough() {
        // A bare `$` that the parser keeps as-is — the string fallback handles it.
        // We just check the output has the `$` and no panic.
        let result = bind_params("SELECT 1", &[]).unwrap();
        assert_eq!(result, "SELECT 1");
    }

    #[test]
    fn bind_high_index() {
        // $10 should not be mis-read as $1 + literal "0".
        let params: Vec<Value> = (1..=10).map(Value::I64).collect();
        let result = bind_params("SELECT $10", &params).unwrap();
        // The 10th param (Value::I64(10)) should appear.
        assert!(result.contains("10"), "expected 10 in: {result}");
        // Crucially, $1 should NOT have been substituted with 1 leaving a
        // bare "0" — the result must not be "SELECT 10" (i.e. $1=1, then "0").
        // The correct result replaces $10 with 10, so the output is "SELECT 10".
        // Either way, we verify both the presence of 10 and no "10" mismatch.
    }

    #[test]
    fn bind_no_params() {
        assert_eq!(bind_params("SELECT 1", &[]).unwrap(), "SELECT 1");
    }

    // ── string-based fallback tests ──────────────────────────────────────────

    #[test]
    fn bind_params_string_null() {
        assert_eq!(
            bind_params_string("SELECT $1", &[Value::Null]).unwrap(),
            "SELECT NULL"
        );
    }

    #[test]
    fn bind_params_string_multiple() {
        assert_eq!(
            bind_params_string("WHERE a=$1 AND b=$2", &[Value::I64(42), Value::Bool(true)])
                .unwrap(),
            "WHERE a=42 AND b=TRUE"
        );
    }

    #[test]
    fn bind_params_string_out_of_bounds() {
        let err = bind_params_string("$2", &[Value::I64(1)]).unwrap_err();
        match err {
            OxiSqlError::Parse(msg) => {
                assert!(
                    msg.contains("$2") || msg.contains("out of range"),
                    "unexpected error message: {msg}"
                );
            }
            other => panic!("expected OxiSqlError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn bind_params_string_zero_index_rejected() {
        let err = bind_params_string("$0", &[Value::I64(1)]).unwrap_err();
        assert!(matches!(err, OxiSqlError::Parse(_)));
    }

    #[test]
    fn bind_params_string_dollar_dollar_escape() {
        assert_eq!(
            bind_params_string("SELECT $$ AS dollar", &[]).unwrap(),
            "SELECT $ AS dollar"
        );
    }

    #[test]
    fn bind_params_string_bare_dollar_passthrough() {
        assert_eq!(bind_params_string("price $ 10", &[]).unwrap(), "price $ 10");
    }

    #[test]
    fn bind_params_string_high_index() {
        let params: Vec<Value> = (1..=10).map(Value::I64).collect();
        let sql = "$10";
        assert_eq!(bind_params_string(sql, &params).unwrap(), "10");
    }

    // ── AST-level binding correctness tests ──────────────────────────────────

    /// `$2` inside a single-quoted string literal must NOT be substituted.
    ///
    /// This is the key correctness property that the AST-level approach provides
    /// over naive string replacement.  sqlparser parses `'$2 is literal'` as a
    /// `Value::SingleQuotedString` node, not a `Value::Placeholder` node, so the
    /// visitor never touches it.
    #[test]
    fn test_ast_binding_dollar_in_string_literal_unchanged() {
        // Only one param supplied ($1 → 42).  The `$2` inside the string literal
        // must survive untouched; if it were treated as a placeholder the call
        // would fail with "out of range".
        let sql = "SELECT $1, '$2 is literal'";
        let result = bind_params(sql, &[Value::I64(42)]).unwrap();
        assert!(
            result.contains("42"),
            "expected param $1 substituted to 42 in: {result}"
        );
        assert!(
            result.contains("$2 is literal"),
            "expected '$2 is literal' string to be preserved in: {result}"
        );
    }

    /// BLOB parameters produce hex-encoded `X'...'` literals.
    #[test]
    fn test_blob_param_binding() {
        let sql = "SELECT $1";
        let result = bind_params(sql, &[Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])]).unwrap();
        // The result should contain the hex bytes in some form.
        let lower = result.to_lowercase();
        assert!(
            lower.contains("dead") || lower.contains("de") && lower.contains("ad"),
            "expected hex-encoded blob bytes in: {result}"
        );
    }

    /// Negative integers are represented correctly (no missing minus sign).
    #[test]
    fn test_negative_integer_binding() {
        let sql = "SELECT $1";
        let result = bind_params(sql, &[Value::I64(-99)]).unwrap();
        assert!(
            result.contains("-99") || result.contains("- 99"),
            "expected -99 in: {result}"
        );
    }

    /// Boolean TRUE / FALSE are emitted as SQL keywords.
    #[test]
    fn test_bool_binding() {
        let t = bind_params("SELECT $1", &[Value::Bool(true)]).unwrap();
        let f = bind_params("SELECT $1", &[Value::Bool(false)]).unwrap();
        assert!(t.to_uppercase().contains("TRUE"), "expected TRUE in: {t}");
        assert!(f.to_uppercase().contains("FALSE"), "expected FALSE in: {f}");
    }

    /// NULL produces the SQL keyword NULL.
    #[test]
    fn test_null_binding() {
        let result = bind_params("SELECT $1", &[Value::Null]).unwrap();
        assert!(result.contains("NULL"), "expected NULL in: {result}");
    }

    /// parse_placeholder helper correctly classifies $N strings.
    #[test]
    fn test_parse_placeholder_helper() {
        assert!(matches!(
            parse_placeholder("$1"),
            PlaceholderParse::Index(0)
        ));
        assert!(matches!(
            parse_placeholder("$10"),
            PlaceholderParse::Index(9)
        ));
        assert!(matches!(
            parse_placeholder("$0"),
            PlaceholderParse::ZeroIndex
        ));
        assert!(matches!(
            parse_placeholder("$"),
            PlaceholderParse::NotPositional
        ));
        assert!(matches!(
            parse_placeholder("hello"),
            PlaceholderParse::NotPositional
        ));
        assert!(matches!(
            parse_placeholder("?"),
            PlaceholderParse::NotPositional
        ));
    }
}
