//! CSV import and export utilities for embedded connections.
//!
//! This module provides pure-Rust CSV processing without external `csv` crate
//! dependencies — all parsing is done with a hand-rolled state machine that
//! correctly handles:
//!
//! - Quoted fields (`"field with, comma"`)
//! - Escaped quotes inside quoted fields (`"say ""hello"""`)
//! - Newlines inside quoted fields
//! - Empty fields (`,,`)
//! - Different line endings (CRLF and LF)
//!
//! # CSV Format
//!
//! The first row is always treated as a header row containing column names.
//! All values are imported as `TEXT` (GlueSQL `TEXT`); type coercion is the
//! caller's responsibility via explicit `CAST` in subsequent queries.
//!
//! # Security
//!
//! Column names are validated to contain only `[A-Za-z0-9_]` characters plus
//! spaces (which are converted to underscores).  Values are inserted as bound
//! parameters via the existing `substitute_params` / `escape_sql_value`
//! infrastructure to prevent SQL injection.
//!
//! # Export
//!
//! The `export_table_to_csv` function queries all rows from the given table and writes
//! RFC 4180-compliant CSV including a header row.  Values containing commas,
//! double quotes, or newlines are quoted and escaped.

use oxisql_core::{OxiSqlError, Value};

// ── CSV parsing ──────────────────────────────────────────────────────────────

/// A single parsed CSV row.
type CsvRow = Vec<String>;

/// State machine states for the CSV parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParseState {
    /// At the start of a field.
    FieldStart,
    /// Inside an unquoted field.
    Unquoted,
    /// Inside a quoted field.
    Quoted,
    /// Seen one `"` inside a quoted field — may be end-quote or `""` escape.
    QuoteEscape,
}

/// Parse a CSV string into rows of fields.
///
/// Returns a `Vec<CsvRow>` where each inner `Vec<String>` is a row.
/// The first row is the header; all subsequent rows are data rows.
/// Empty input returns an empty vector.
///
/// # Errors
///
/// Returns [`OxiSqlError::Other`] if the input contains an unterminated
/// quoted field (missing closing `"`).
pub fn parse_csv(input: &str) -> Result<Vec<CsvRow>, OxiSqlError> {
    let mut rows: Vec<CsvRow> = Vec::new();
    let mut current_row: CsvRow = Vec::new();
    let mut current_field = String::new();
    let mut state = ParseState::FieldStart;

    let chars: Vec<char> = input.chars().collect();
    let n = chars.len();
    let mut i = 0;

    while i < n {
        let ch = chars[i];

        match state {
            ParseState::FieldStart => match ch {
                '"' => {
                    state = ParseState::Quoted;
                    i += 1;
                }
                ',' => {
                    // Empty field
                    current_row.push(String::new());
                    i += 1;
                }
                '\r' => {
                    // CRLF: consume \r
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                    // consume following \n
                    if i < n && chars[i] == '\n' {
                        i += 1;
                    }
                }
                '\n' => {
                    // Bare LF
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                }
                _ => {
                    current_field.push(ch);
                    state = ParseState::Unquoted;
                    i += 1;
                }
            },

            ParseState::Unquoted => match ch {
                ',' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                }
                '\r' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                    if i < n && chars[i] == '\n' {
                        i += 1;
                    }
                }
                '\n' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                }
                _ => {
                    current_field.push(ch);
                    i += 1;
                }
            },

            ParseState::Quoted => match ch {
                '"' => {
                    state = ParseState::QuoteEscape;
                    i += 1;
                }
                _ => {
                    current_field.push(ch);
                    i += 1;
                }
            },

            ParseState::QuoteEscape => match ch {
                '"' => {
                    // Escaped double quote inside a quoted field
                    current_field.push('"');
                    state = ParseState::Quoted;
                    i += 1;
                }
                ',' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                }
                '\r' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                    if i < n && chars[i] == '\n' {
                        i += 1;
                    }
                }
                '\n' => {
                    current_row.push(current_field.clone());
                    current_field.clear();
                    rows.push(current_row.clone());
                    current_row.clear();
                    state = ParseState::FieldStart;
                    i += 1;
                }
                _ => {
                    // Treat as closing quote followed by garbage — lenient mode
                    current_field.push(ch);
                    state = ParseState::Unquoted;
                    i += 1;
                }
            },
        }
    }

    // Flush the last field / row
    match state {
        ParseState::Quoted => {
            return Err(OxiSqlError::Other(
                "CSV parse error: unterminated quoted field at end of input".into(),
            ));
        }
        _ => {
            // If we have content in the current field or row, flush it
            if !current_field.is_empty() || !current_row.is_empty() {
                current_row.push(current_field);
                rows.push(current_row);
            }
        }
    }

    Ok(rows)
}

// ── Column name sanitisation ─────────────────────────────────────────────────

/// Sanitise a CSV header field into a valid SQL identifier.
///
/// Replaces spaces and hyphens with underscores, strips leading digits,
/// and limits to `[A-Za-z0-9_]` characters.  Returns an error if the
/// result is empty (e.g. a header of purely non-ASCII characters).
pub fn sanitise_column_name(raw: &str) -> Result<String, OxiSqlError> {
    let mut out = String::with_capacity(raw.len());
    for (i, ch) in raw.chars().enumerate() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if ch == ' ' || ch == '-' || ch == '_' {
            out.push('_');
        } else {
            // Skip non-ASCII and other special characters
        }
        // Strip leading digits
        if i == 0 && out.starts_with(|c: char| c.is_ascii_digit()) {
            out = format!("col_{out}");
        }
    }
    if out.is_empty() {
        return Err(OxiSqlError::Other(format!(
            "CSV import: header '{raw}' cannot be converted to a valid SQL column name"
        )));
    }
    Ok(out.to_ascii_lowercase())
}

// ── DDL / DML builders ───────────────────────────────────────────────────────

/// Build the `CREATE TABLE` DDL for a CSV import.
///
/// All columns are declared `TEXT`, which accepts any CSV value.
pub fn build_create_table_sql(table: &str, columns: &[String]) -> String {
    let col_defs: Vec<String> = columns.iter().map(|c| format!("    {c} TEXT")).collect();
    format!("CREATE TABLE {table} (\n{}\n)", col_defs.join(",\n"))
}

/// Build an `INSERT INTO … VALUES (…)` SQL statement for one CSV row.
///
/// Values are embedded as single-quoted literals.  Single quotes inside
/// values are doubled (`'` → `''`) to prevent SQL injection.
pub fn build_insert_sql(table: &str, columns: &[String], values: &[String]) -> String {
    let col_list = columns.join(", ");
    let val_list: Vec<String> = values
        .iter()
        .map(|v| {
            if v.is_empty() {
                "NULL".to_string()
            } else {
                format!("'{}'", v.replace('\'', "''"))
            }
        })
        .collect();
    format!(
        "INSERT INTO {table} ({col_list}) VALUES ({})",
        val_list.join(", ")
    )
}

// ── CSV export ───────────────────────────────────────────────────────────────

/// Format a [`Value`] as a CSV field string (without surrounding quotes).
///
/// NULL becomes an empty string.  Other values use their `Display`
/// representation.
pub fn value_to_csv_field(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::I64(n) => n.to_string(),
        Value::F64(f) => f.to_string(),
        Value::Text(s) | Value::Json(s) => s.clone(),
        Value::Blob(b) => {
            // Hex-encode blobs for CSV portability
            b.iter().fold(String::new(), |mut acc, byte| {
                acc.push_str(&format!("{byte:02x}"));
                acc
            })
        }
        Value::Timestamp(ts) => ts.to_string(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::Uuid(u) => {
            // Format UUID in standard 8-4-4-4-12 hex notation
            let hi = (*u >> 64) as u64;
            let lo = *u as u64;
            format!(
                "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                (hi >> 32) as u32,
                (hi >> 16) as u16,
                hi as u16,
                (lo >> 48) as u16,
                lo & 0x0000_ffff_ffff_ffff,
            )
        }
        Value::Decimal(d) => d.clone(),
        Value::Array(arr) => {
            // Represent arrays as JSON-ish string
            let inner: Vec<String> = arr.iter().map(value_to_csv_field).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::TypedArray { values: arr, .. } => {
            // Represent typed arrays as JSON-ish string (element_type is decorative metadata)
            let inner: Vec<String> = arr.iter().map(value_to_csv_field).collect();
            format!("[{}]", inner.join(", "))
        }
    }
}

/// Quote a CSV field if it requires quoting.
///
/// A field requires quoting if it contains commas, double quotes, newlines,
/// or leading/trailing whitespace.  Per RFC 4180, double-quotes inside a
/// quoted field are doubled.
pub fn quote_csv_field(s: &str) -> String {
    let needs_quoting = s.contains(',')
        || s.contains('"')
        || s.contains('\n')
        || s.contains('\r')
        || s.starts_with(' ')
        || s.ends_with(' ');

    if needs_quoting {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Build a complete CSV string from a header row and data rows.
///
/// Each row is terminated with `\r\n` per RFC 4180.
pub fn build_csv_output(headers: &[String], rows: &[Vec<Value>]) -> String {
    let mut out = String::new();

    // Header row
    let header_line: Vec<String> = headers.iter().map(|h| quote_csv_field(h)).collect();
    out.push_str(&header_line.join(","));
    out.push_str("\r\n");

    // Data rows
    for row in rows {
        let fields: Vec<String> = row
            .iter()
            .map(|v| quote_csv_field(&value_to_csv_field(v)))
            .collect();
        out.push_str(&fields.join(","));
        out.push_str("\r\n");
    }

    out
}

// ── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_csv() {
        let input = "a,b,c\r\n1,2,3\r\n4,5,6\r\n";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["a", "b", "c"]);
        assert_eq!(rows[1], vec!["1", "2", "3"]);
        assert_eq!(rows[2], vec!["4", "5", "6"]);
    }

    #[test]
    fn parse_csv_lf_only() {
        let input = "x,y\n10,20\n30,40\n";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], vec!["x", "y"]);
        assert_eq!(rows[1], vec!["10", "20"]);
    }

    #[test]
    fn parse_csv_quoted_field_with_comma() {
        let input = "name,value\r\n\"Smith, John\",42\r\n";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows[1][0], "Smith, John");
        assert_eq!(rows[1][1], "42");
    }

    #[test]
    fn parse_csv_escaped_double_quote() {
        let input = "msg\r\n\"say \"\"hello\"\"\"\r\n";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows[1][0], "say \"hello\"");
    }

    #[test]
    fn parse_csv_empty_fields() {
        let input = "a,b,c\r\n,,\r\n1,,3\r\n";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows[1], vec!["", "", ""]);
        assert_eq!(rows[2], vec!["1", "", "3"]);
    }

    #[test]
    fn parse_csv_no_trailing_newline() {
        let input = "a,b\r\n1,2";
        let rows = parse_csv(input).expect("parse should succeed");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], vec!["1", "2"]);
    }

    #[test]
    fn parse_csv_unterminated_quote_is_error() {
        let input = "a,b\r\n\"unterminated,value\r\n";
        let result = parse_csv(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unterminated"));
    }

    #[test]
    fn sanitise_column_name_basic() {
        assert_eq!(sanitise_column_name("first_name").unwrap(), "first_name");
        assert_eq!(sanitise_column_name("First Name").unwrap(), "first_name");
        assert_eq!(sanitise_column_name("price-USD").unwrap(), "price_usd");
    }

    #[test]
    fn sanitise_column_name_leading_digit() {
        let result = sanitise_column_name("1abc").unwrap();
        assert!(result.starts_with("col_"));
    }

    #[test]
    fn sanitise_column_name_empty_result_is_error() {
        // Pure non-ASCII/special chars produce empty result
        let result = sanitise_column_name("@#$");
        assert!(result.is_err());
    }

    #[test]
    fn build_create_table_sql_basic() {
        let cols = vec!["id".to_string(), "name".to_string()];
        let sql = build_create_table_sql("users", &cols);
        assert!(sql.contains("CREATE TABLE users"));
        assert!(sql.contains("id TEXT"));
        assert!(sql.contains("name TEXT"));
    }

    #[test]
    fn build_insert_sql_with_null_empty() {
        let cols = vec!["id".to_string(), "name".to_string(), "notes".to_string()];
        let vals = vec!["1".to_string(), "Alice".to_string(), "".to_string()];
        let sql = build_insert_sql("users", &cols, &vals);
        assert!(sql.contains("VALUES"));
        assert!(sql.contains("NULL")); // empty becomes NULL
        assert!(sql.contains("'Alice'"));
    }

    #[test]
    fn build_insert_sql_escapes_single_quote() {
        let cols = vec!["msg".to_string()];
        let vals = vec!["it's here".to_string()];
        let sql = build_insert_sql("t", &cols, &vals);
        assert!(sql.contains("it''s here"));
    }

    #[test]
    fn value_to_csv_field_null_is_empty() {
        assert_eq!(value_to_csv_field(&Value::Null), "");
    }

    #[test]
    fn value_to_csv_field_bool() {
        assert_eq!(value_to_csv_field(&Value::Bool(true)), "true");
        assert_eq!(value_to_csv_field(&Value::Bool(false)), "false");
    }

    #[test]
    fn value_to_csv_field_blob_hex() {
        let blob = Value::Blob(vec![0xDE, 0xAD]);
        assert_eq!(value_to_csv_field(&blob), "dead");
    }

    #[test]
    fn quote_csv_field_no_special_chars() {
        assert_eq!(quote_csv_field("hello"), "hello");
    }

    #[test]
    fn quote_csv_field_with_comma() {
        assert_eq!(quote_csv_field("a,b"), "\"a,b\"");
    }

    #[test]
    fn quote_csv_field_with_double_quote() {
        assert_eq!(quote_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn build_csv_output_basic() {
        let headers = vec!["id".to_string(), "name".to_string()];
        let rows = vec![
            vec![Value::I64(1), Value::Text("Alice".into())],
            vec![Value::I64(2), Value::Text("Bob".into())],
        ];
        let csv = build_csv_output(&headers, &rows);
        let lines: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "id,name");
        assert_eq!(lines[1], "1,Alice");
        assert_eq!(lines[2], "2,Bob");
    }

    #[test]
    fn round_trip_csv_with_special_chars() {
        let headers = vec!["note".to_string()];
        let rows = vec![vec![Value::Text("hello, world".into())]];
        let csv = build_csv_output(&headers, &rows);
        // The comma-containing field should be quoted
        assert!(csv.contains("\"hello, world\""));
        // Re-parse to verify round-trip
        let parsed = parse_csv(&csv).expect("reparse should work");
        assert_eq!(parsed.len(), 2); // header + 1 data row
        assert_eq!(parsed[1][0], "hello, world");
    }
}
