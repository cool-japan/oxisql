//! `oxisql-repl` — Interactive SQL REPL for OxiSQL.
//!
//! Provides an interactive Read-Eval-Print Loop for executing SQL statements
//! against any OxiSQL-supported database backend.
//!
//! # Usage
//!
//! ```text
//! # Connect to an in-memory database (default)
//! oxisql-repl
//!
//! # Connect via URI
//! oxisql-repl memory://
//! oxisql-repl postgres://user:pass@localhost/db
//! oxisql-repl mysql://user:pass@localhost/db
//! oxisql-repl sqlite://path/to/db.sqlite
//! ```
//!
//! # Interactive commands
//!
//! - `.help` — show help
//! - `.tables` — list all tables in the current database
//! - `.schema <table>` — show CREATE TABLE DDL for a table
//! - `.mode table|csv|json` — set output format
//! - `.timer on|off` — toggle query timing
//! - `.read <file>` — execute SQL from a file
//! - `.history` — print command history
//! - `.quit` / `.exit` — exit the REPL
//! - Any other input is treated as SQL and executed.
//!
//! Multi-line SQL statements are accumulated until a line ending with `;` is
//! entered or a blank line is entered after content.

use std::io::{self, BufRead, IsTerminal, Write};
use std::time::Instant;

use anyhow::{Context as _, Result};
use oxisql::{Connection, Row, Value};

/// Column separator used when rendering query results in table mode.
const COL_SEP: &str = " | ";
/// Row separator width cap (terminal width estimate).
const MAX_COL_WIDTH: usize = 40;

/// Output format for query results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Traditional ASCII table (the default).
    #[default]
    Table,
    /// Comma-separated values: one header row then data rows.
    Csv,
    /// Array of JSON objects, one per row.
    Json,
}

impl Mode {
    /// Parse a mode string (case-insensitive).
    fn from_str(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "table" => Some(Mode::Table),
            "csv" => Some(Mode::Csv),
            "json" => Some(Mode::Json),
            _ => None,
        }
    }
}

// ── Table formatter ───────────────────────────────────────────────────────────

/// Display a query result table on stdout.
fn display_rows_table(rows: &[Row]) {
    if rows.is_empty() {
        println!("(0 rows)");
        return;
    }

    // Collect column names from first row
    let headers: Vec<&str> = rows[0].columns().iter().map(|s| s.as_str()).collect();
    let col_count = headers.len();
    let mut col_widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    // Render all cell values first so we can measure them
    let cell_values: Vec<Vec<String>> = rows
        .iter()
        .map(|row| {
            (0..col_count)
                .map(|i| {
                    let v = row.get_by_index(i).unwrap_or(&Value::Null);
                    format_value(v)
                })
                .collect()
        })
        .collect();

    // Update column widths
    for row_vals in &cell_values {
        for (i, cell) in row_vals.iter().enumerate() {
            let w = cell.len().min(MAX_COL_WIDTH);
            if w > col_widths[i] {
                col_widths[i] = w;
            }
        }
    }

    // Header line
    let header_line: Vec<String> = headers
        .iter()
        .enumerate()
        .map(|(i, h)| format!("{:width$}", h, width = col_widths[i]))
        .collect();
    println!("{}", header_line.join(COL_SEP));

    // Separator
    let sep_line: Vec<String> = col_widths.iter().map(|&w| "-".repeat(w)).collect();
    println!("{}", sep_line.join("-+-"));

    // Data rows
    for row_vals in &cell_values {
        let cells: Vec<String> = row_vals
            .iter()
            .enumerate()
            .map(|(i, cell)| {
                let w = col_widths[i];
                if cell.len() > w {
                    format!("{}…", &cell[..w.saturating_sub(1)])
                } else {
                    format!("{:width$}", cell, width = w)
                }
            })
            .collect();
        println!("{}", cells.join(COL_SEP));
    }

    println!(
        "({} row{})",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    );
}

// ── CSV formatter ─────────────────────────────────────────────────────────────

/// Format `rows` as CSV and return the result as a `String`.
///
/// Produces one header row followed by one data row per result row.  Fields
/// containing a comma, a double-quote, or a newline are quoted per RFC 4180.
pub fn format_rows_csv(rows: &[Row]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let headers = rows[0].columns();
    let col_count = headers.len();
    let mut out = String::new();

    // Header row
    for (i, h) in headers.iter().enumerate() {
        out.push_str(&csv_escape(h));
        if i + 1 < col_count {
            out.push(',');
        }
    }
    out.push('\n');

    // Data rows
    for row in rows {
        for i in 0..col_count {
            let v = row.get_by_index(i).unwrap_or(&Value::Null);
            let cell = format_value(v);
            out.push_str(&csv_escape(&cell));
            if i + 1 < col_count {
                out.push(',');
            }
        }
        out.push('\n');
    }

    out
}

/// Escape a single CSV field according to RFC 4180.
fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        // Wrap in double-quotes and escape embedded double-quotes
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

/// Print `rows` in CSV format to stdout.
fn display_rows_csv(rows: &[Row]) {
    print!("{}", format_rows_csv(rows));
}

// ── JSON formatter ────────────────────────────────────────────────────────────

/// Format `rows` as a JSON array of objects and return the result as a `String`.
///
/// Each row becomes one JSON object with column names as keys.  Values are
/// serialised without any external crate dependency:
///
/// * `NULL` → `null`
/// * booleans → `true` / `false`
/// * integers and floats → bare numbers
/// * everything else → JSON string (double-quote escaped)
pub fn format_rows_json(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "[]\n".to_string();
    }

    let headers = rows[0].columns();
    let col_count = headers.len();
    let mut out = String::from("[\n");

    for (ri, row) in rows.iter().enumerate() {
        out.push_str("  {");
        for (i, h) in headers.iter().enumerate() {
            let v = row.get_by_index(i).unwrap_or(&Value::Null);
            let json_val = value_to_json(v);
            out.push_str(&format!("\"{}\":{}", json_escape_str(h), json_val));
            if i + 1 < col_count {
                out.push(',');
            }
        }
        out.push('}');
        if ri + 1 < rows.len() {
            out.push(',');
        }
        out.push('\n');
    }

    out.push_str("]\n");
    out
}

/// Serialise a [`Value`] to its JSON representation (no external deps).
fn value_to_json(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::I64(n) => n.to_string(),
        Value::F64(f) => {
            if f.is_finite() {
                format!("{f}")
            } else {
                "null".to_string() // JSON has no Inf/NaN
            }
        }
        Value::Text(s) => format!("\"{}\"", json_escape_str(s)),
        Value::Blob(b) => format!("\"\\x{}\"", hex_encode(b)),
        Value::Timestamp(ts) => format!("\"{}\"", json_escape_str(&ts.to_string())),
        Value::Date(d) => format!("\"{}\"", json_escape_str(&d.to_string())),
        Value::Time(t) => format!("\"{}\"", json_escape_str(&t.to_string())),
        Value::Uuid(u) => format!("\"{}\"", format_uuid(*u)),
        Value::Json(s) => s.clone(), // already valid JSON
        Value::Decimal(d) => format!("\"{}\"", d),
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        Value::TypedArray { values: arr, .. } => {
            let inner: Vec<String> = arr.iter().map(value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
    }
}

/// Escape a string for embedding inside a JSON double-quoted value.
fn json_escape_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Print `rows` in JSON format to stdout.
fn display_rows_json(rows: &[Row]) {
    print!("{}", format_rows_json(rows));
}

// ── Shared helpers ────────────────────────────────────────────────────────────

/// Format a [`Value`] for terminal display.
fn format_value(v: &Value) -> String {
    match v {
        Value::Null => "NULL".to_string(),
        Value::Bool(b) => if *b { "t" } else { "f" }.to_string(),
        Value::I64(n) => n.to_string(),
        Value::F64(f) => format!("{f:.6}"),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => format!("\\x{}", hex_encode(b)),
        Value::Timestamp(ts) => ts.to_string(),
        Value::Date(d) => d.to_string(),
        Value::Time(t) => t.to_string(),
        Value::Uuid(u) => format_uuid(*u),
        Value::Json(s) => s.clone(),
        Value::Decimal(d) => d.to_string(),
        Value::Array(arr) => {
            let inner: Vec<String> = arr.iter().map(format_value).collect();
            format!("{{{}}}", inner.join(","))
        }
        Value::TypedArray { values: arr, .. } => {
            let inner: Vec<String> = arr.iter().map(format_value).collect();
            format!("{{{}}}", inner.join(","))
        }
    }
}

/// Hex-encode a byte slice (no `0x` prefix).
fn hex_encode(b: &[u8]) -> String {
    b.iter().fold(String::new(), |mut acc, byte| {
        acc.push_str(&format!("{byte:02x}"));
        acc
    })
}

/// Format a UUID `u128` as the standard 8-4-4-4-12 hyphenated form.
fn format_uuid(u: u128) -> String {
    let hi = (u >> 64) as u64;
    let lo = u as u64;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        (hi >> 32) as u32,
        (hi >> 16) as u16,
        hi as u16,
        (lo >> 48) as u16,
        lo & 0x0000_ffff_ffff_ffff,
    )
}

// ── Display dispatcher ────────────────────────────────────────────────────────

/// Dispatch `rows` to the appropriate formatter given `mode`.
fn display_rows(rows: &[Row], mode: Mode) {
    match mode {
        Mode::Table => display_rows_table(rows),
        Mode::Csv => display_rows_csv(rows),
        Mode::Json => display_rows_json(rows),
    }
}

// ── REPL state ────────────────────────────────────────────────────────────────

/// Mutable state shared across the REPL loop.
struct ReplState {
    /// Current output mode.
    mode: Mode,
    /// Whether to print query execution time after each statement.
    timer_on: bool,
    /// In-memory history: one entry per user input (excluding blank lines).
    history: Vec<String>,
}

impl ReplState {
    fn new() -> Self {
        Self {
            mode: Mode::default(),
            timer_on: false,
            history: Vec::new(),
        }
    }

    /// Record a non-empty input line into the history buffer.
    fn push_history(&mut self, line: &str) {
        if !line.trim().is_empty() {
            self.history.push(line.to_string());
        }
    }
}

// ── Dot command handler ───────────────────────────────────────────────────────

/// Handle a special `.command`.
///
/// Returns `true` when the caller should exit the REPL, `false` otherwise.
async fn handle_dot_command(
    cmd: &str,
    conn: &dyn Connection,
    is_tty: bool,
    state: &mut ReplState,
) -> Result<bool> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    match parts[0] {
        ".quit" | ".exit" | ".q" => {
            if is_tty {
                println!("Bye.");
            }
            return Ok(true);
        }

        ".help" | ".h" => {
            println!(
                r#"OxiSQL REPL commands:
  .help                     Show this help
  .tables                   List all tables
  .schema <table>           Show columns for <table>
  .mode table|csv|json      Set output format (default: table)
  .timer on|off             Toggle query execution timing
  .read <file>              Execute SQL from a file
  .history                  Print command history
  .quit / .exit / .q        Exit the REPL

SQL statements end with ';' or a blank line."#
            );
        }

        ".tables" => {
            let tables = conn.tables().await.context("failed to list tables")?;
            if tables.is_empty() {
                println!("(no tables)");
            } else {
                for t in &tables {
                    println!("{}", t.name);
                }
            }
        }

        ".schema" => {
            let table_name = parts.get(1).unwrap_or(&"").trim();
            if table_name.is_empty() {
                eprintln!("Usage: .schema <table_name>");
            } else {
                let columns = conn
                    .columns(table_name)
                    .await
                    .context("failed to describe columns")?;
                if columns.is_empty() {
                    println!("(table '{table_name}' not found or has no columns)");
                } else {
                    println!("Table: {table_name}");
                    println!("{:<30} {:<20} {:<10}", "COLUMN", "TYPE", "NULLABLE");
                    println!("{}", "-".repeat(62));
                    for col in &columns {
                        let nullable = if col.nullable { "YES" } else { "NO" };
                        println!("{:<30} {:<20} {:<10}", col.name, col.data_type, nullable);
                    }
                }
            }
        }

        ".mode" => {
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            match Mode::from_str(arg) {
                Some(m) => {
                    state.mode = m;
                    if is_tty {
                        println!("Output mode: {arg}");
                    }
                }
                None => {
                    eprintln!("Usage: .mode table|csv|json");
                }
            }
        }

        ".timer" => {
            let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");
            match arg.to_ascii_lowercase().as_str() {
                "on" => {
                    state.timer_on = true;
                    if is_tty {
                        println!("Timer: on");
                    }
                }
                "off" => {
                    state.timer_on = false;
                    if is_tty {
                        println!("Timer: off");
                    }
                }
                _ => {
                    eprintln!("Usage: .timer on|off");
                }
            }
        }

        ".read" => {
            let file_path = parts.get(1).map(|s| s.trim()).unwrap_or("");
            if file_path.is_empty() {
                eprintln!("Usage: .read <file>");
            } else {
                execute_file(conn, file_path, state).await;
            }
        }

        ".history" => {
            if state.history.is_empty() {
                println!("(no history)");
            } else {
                for (i, entry) in state.history.iter().enumerate() {
                    println!("{:>4}  {entry}", i + 1);
                }
            }
        }

        unknown => {
            eprintln!("Unknown command '{unknown}'. Type .help for help.");
        }
    }
    Ok(false)
}

// ── `.read <file>` implementation ─────────────────────────────────────────────

/// Read `path`, submit each non-empty, non-comment line as a SQL statement.
async fn execute_file(conn: &dyn Connection, path: &str, state: &mut ReplState) {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error reading '{path}': {e}");
            return;
        }
    };

    let mut buffer = String::new();
    for line in content.lines() {
        let trimmed = line.trim();
        // Skip blank lines and SQL-style comments
        if trimmed.is_empty() || trimmed.starts_with("--") {
            continue;
        }
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(trimmed);
        if trimmed.ends_with(';') {
            execute_sql(conn, buffer.trim(), state).await;
            buffer.clear();
        }
    }
    // Execute any remaining buffered content
    if !buffer.trim().is_empty() {
        execute_sql(conn, buffer.trim(), state).await;
    }
}

// ── REPL main loop ────────────────────────────────────────────────────────────

/// Run the REPL loop.
///
/// Reads SQL input line-by-line.  Multi-line statements are accumulated
/// until either a `;` is seen at the end of a line or a blank line is
/// entered.  Single-line statements ending with `;` are executed
/// immediately.
async fn run_repl(conn: &dyn Connection, uri: &str) -> Result<()> {
    let stdin = io::stdin();
    let is_tty = stdin.is_terminal();
    let mut state = ReplState::new();

    if is_tty {
        println!("OxiSQL REPL — connected to {uri}");
        println!("Type .help for commands, .quit to exit.");
        println!();
    }

    let mut buffer = String::new();
    let reader = io::BufReader::new(stdin.lock());
    let mut lines = reader.lines();

    loop {
        // Print prompt
        if is_tty {
            if buffer.is_empty() {
                print!("oxisql> ");
            } else {
                print!("      > ");
            }
            io::stdout().flush()?;
        }

        // Read next line
        let line = match lines.next() {
            Some(l) => l.context("failed to read stdin")?,
            None => {
                // EOF — execute anything buffered, then exit
                if !buffer.trim().is_empty() {
                    execute_sql(conn, buffer.trim(), &mut state).await;
                }
                if is_tty {
                    println!("\nBye.");
                }
                break;
            }
        };

        let trimmed = line.trim();

        // Record non-empty input in history
        if !trimmed.is_empty() {
            state.push_history(trimmed);
        }

        // Dot commands
        if trimmed.starts_with('.') && buffer.is_empty() {
            let should_exit = handle_dot_command(trimmed, conn, is_tty, &mut state)
                .await
                .unwrap_or_else(|e| {
                    eprintln!("Error: {e}");
                    false
                });
            if should_exit {
                break;
            }
            continue;
        }

        // Blank line: flush buffered SQL if any
        if trimmed.is_empty() {
            if !buffer.trim().is_empty() {
                execute_sql(conn, buffer.trim(), &mut state).await;
                buffer.clear();
            }
            continue;
        }

        // Accumulate
        if !buffer.is_empty() {
            buffer.push('\n');
        }
        buffer.push_str(trimmed);

        // Execute if line ends with ';'
        if trimmed.ends_with(';') {
            execute_sql(conn, buffer.trim(), &mut state).await;
            buffer.clear();
        }
    }

    Ok(())
}

/// Execute one SQL statement (or batch), printing results or error.
async fn execute_sql(conn: &dyn Connection, sql: &str, state: &mut ReplState) {
    let upper = sql.trim_start().to_ascii_uppercase();
    let is_query =
        upper.starts_with("SELECT") || upper.starts_with("WITH") || upper.starts_with("EXPLAIN");

    let start = state.timer_on.then(Instant::now);

    if is_query {
        match conn.query(sql, &[]).await {
            Ok(rows) => {
                display_rows(&rows, state.mode);
                if let Some(t) = start {
                    println!("Time: {:.3}ms", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    } else {
        match conn.execute(sql, &[]).await {
            Ok(n) => {
                if n > 0 {
                    println!("({n} row{} affected)", if n == 1 { "" } else { "s" });
                } else {
                    println!("OK");
                }
                if let Some(t) = start {
                    println!("Time: {:.3}ms", t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let uri = args.get(1).map(|s| s.as_str()).unwrap_or("memory://");

    let conn = oxisql::connect(uri)
        .await
        .with_context(|| format!("failed to connect to '{uri}'"))?;

    run_repl(conn.as_ref(), uri).await
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use oxisql::Row;

    // ── Helper: build a Row from column names and string values ───────────────

    /// Build a minimal `Vec<Row>` from column name / value pairs for tests.
    /// Uses `Value::Text` for all non-null values; `Value::Null` for empty str.
    fn make_rows(headers: &[&str], data: &[&[&str]]) -> Vec<Row> {
        data.iter()
            .map(|row_vals| {
                let cols: Vec<String> = headers.iter().map(|h| h.to_string()).collect();
                let vals: Vec<Value> = row_vals
                    .iter()
                    .map(|v| {
                        if v.is_empty() {
                            Value::Null
                        } else {
                            Value::Text(v.to_string())
                        }
                    })
                    .collect();
                Row::new(cols, vals)
            })
            .collect()
    }

    // ── CSV formatter tests ───────────────────────────────────────────────────

    /// CSV output has a header row plus one data row per result row.
    #[test]
    fn csv_format_rows() {
        let rows = make_rows(&["id", "name"], &[&["1", "Alice"], &["2", "Bob"]]);
        let csv = format_rows_csv(&rows);
        let mut lines = csv.lines();
        assert_eq!(lines.next(), Some("id,name"));
        assert_eq!(lines.next(), Some("1,Alice"));
        assert_eq!(lines.next(), Some("2,Bob"));
        assert!(lines.next().is_none());
    }

    /// CSV fields containing commas are double-quoted.
    #[test]
    fn csv_format_comma_in_field() {
        let rows = make_rows(&["v"], &[&["hello,world"]]);
        let csv = format_rows_csv(&rows);
        assert!(
            csv.contains("\"hello,world\""),
            "comma in field should be quoted; got: {csv}"
        );
    }

    /// An empty row set produces an empty string.
    #[test]
    fn csv_format_empty() {
        let csv = format_rows_csv(&[]);
        assert_eq!(csv, "");
    }

    // ── JSON formatter tests ──────────────────────────────────────────────────

    /// JSON output is a valid array of objects.
    #[test]
    fn json_format_rows() {
        let rows = make_rows(&["id", "name"], &[&["1", "Alice"]]);
        let json = format_rows_json(&rows);
        assert!(json.starts_with('['), "should start with '['");
        assert!(json.contains("\"id\""), "should contain 'id' key");
        assert!(json.contains("\"name\""), "should contain 'name' key");
        assert!(json.contains("\"Alice\""), "should contain 'Alice' value");
    }

    /// Empty row set → `[]\n`.
    #[test]
    fn json_format_empty() {
        let json = format_rows_json(&[]);
        assert_eq!(json, "[]\n");
    }

    /// Null values render as JSON `null`.
    #[test]
    fn json_format_null_value() {
        let rows = make_rows(&["v"], &[&[""]]);
        let json = format_rows_json(&rows);
        assert!(
            json.contains(":null"),
            "NULL should render as json null; got: {json}"
        );
    }

    /// Strings containing double-quotes are properly escaped.
    #[test]
    fn json_format_escaped_string() {
        let rows = make_rows(&["v"], &[&["say \"hi\""]]);
        let json = format_rows_json(&rows);
        assert!(
            json.contains("say \\\"hi\\\""),
            "double-quotes should be escaped; got: {json}"
        );
    }

    // ── .timer tests ─────────────────────────────────────────────────────────

    /// Timer state toggles without panicking, and elapsed is always ≥ 0.
    #[test]
    fn timer_toggle() {
        let mut state = ReplState::new();
        assert!(!state.timer_on);

        state.timer_on = true;
        let t = Instant::now();
        let elapsed_ns = t.elapsed().as_nanos();
        assert!(elapsed_ns < u128::MAX, "elapsed should fit in u128");

        state.timer_on = false;
        assert!(!state.timer_on);
    }

    // ── .read <file> test ────────────────────────────────────────────────────

    /// `.read` processes SQL from a file and executes each statement.
    #[tokio::test]
    #[cfg(feature = "embedded")]
    async fn dot_read_tempfile() {
        use std::io::Write as _;

        // Write SQL to a temp file
        let mut path = std::env::temp_dir();
        path.push(format!("oxisql_dot_read_test_{}.sql", std::process::id()));

        {
            let mut f = std::fs::File::create(&path).expect("create temp file");
            writeln!(f, "CREATE TABLE dot_read_tbl (id INTEGER, v TEXT);").unwrap();
            writeln!(f, "INSERT INTO dot_read_tbl VALUES (1, 'hello');").unwrap();
        }

        let conn = oxisql::connect("memory://")
            .await
            .expect("embedded connect");
        let mut state = ReplState::new();
        execute_file(conn.as_ref(), path.to_str().expect("path utf8"), &mut state).await;

        // Verify the table was created and populated
        let rows = conn
            .query("SELECT id, v FROM dot_read_tbl", &[])
            .await
            .expect("SELECT after .read");
        assert_eq!(rows.len(), 1, "one row should be present after .read");

        let _ = std::fs::remove_file(&path);
    }

    // ── Mode parsing test ────────────────────────────────────────────────────

    #[test]
    fn mode_from_str_valid() {
        assert_eq!(Mode::from_str("table"), Some(Mode::Table));
        assert_eq!(Mode::from_str("csv"), Some(Mode::Csv));
        assert_eq!(Mode::from_str("json"), Some(Mode::Json));
        assert_eq!(Mode::from_str("TABLE"), Some(Mode::Table)); // case-insensitive
    }

    #[test]
    fn mode_from_str_invalid() {
        assert_eq!(Mode::from_str("xml"), None);
        assert_eq!(Mode::from_str(""), None);
    }

    // ── History test ──────────────────────────────────────────────────────────

    #[test]
    fn history_accumulates() {
        let mut state = ReplState::new();
        state.push_history("SELECT 1");
        state.push_history("SELECT 2");
        assert_eq!(state.history.len(), 2);
        assert_eq!(state.history[0], "SELECT 1");
        assert_eq!(state.history[1], "SELECT 2");
    }

    #[test]
    fn history_skips_blank_lines() {
        let mut state = ReplState::new();
        state.push_history("  ");
        state.push_history("");
        assert!(
            state.history.is_empty(),
            "blank lines should not be recorded"
        );
    }
}
