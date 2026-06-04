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
//! - `.quit` / `.exit` — exit the REPL
//! - Any other input is treated as SQL and executed.
//!
//! Multi-line SQL statements are accumulated until a line ending with `;` is
//! entered or a blank line is entered after content.

use std::io::{self, BufRead, IsTerminal, Write};

use anyhow::{Context as _, Result};
use oxisql::{Connection, Row, Value};

/// Column separator used when rendering query results.
const COL_SEP: &str = " | ";
/// Row separator width cap (terminal width estimate).
const MAX_COL_WIDTH: usize = 40;

/// Display a query result table on stdout.
fn display_rows(rows: &[Row]) {
    if rows.is_empty() {
        println!("(0 rows)");
        return;
    }

    // Collect column names from first row
    let headers: Vec<&str> = rows[0].columns().iter().map(|s| s.as_str()).collect();

    // Compute column widths: max of header name and value display width
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
                // Truncate long values
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

/// Handle a special `.command`.
///
/// Returns `true` when the caller should exit the REPL, `false` otherwise.
async fn handle_dot_command(cmd: &str, conn: &dyn Connection, is_tty: bool) -> Result<bool> {
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
  .help                 Show this help
  .tables               List all tables
  .schema <table>       Show columns for <table>
  .quit / .exit / .q    Exit the REPL

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
        unknown => {
            eprintln!("Unknown command '{unknown}'. Type .help for help.");
        }
    }
    Ok(false)
}

/// Run the REPL loop.
///
/// Reads SQL input line-by-line.  Multi-line statements are accumulated
/// until either a `;` is seen at the end of a line or a blank line is
/// entered.  Single-line statements ending with `;` are executed
/// immediately.
async fn run_repl(conn: &dyn Connection, uri: &str) -> Result<()> {
    let stdin = io::stdin();
    let is_tty = stdin.is_terminal();

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
                    execute_sql(conn, buffer.trim()).await;
                }
                if is_tty {
                    println!("\nBye.");
                }
                break;
            }
        };

        let trimmed = line.trim();

        // Dot commands
        if trimmed.starts_with('.') && buffer.is_empty() {
            let should_exit = handle_dot_command(trimmed, conn, is_tty)
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
                execute_sql(conn, buffer.trim()).await;
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
            execute_sql(conn, buffer.trim()).await;
            buffer.clear();
        }
    }

    Ok(())
}

/// Execute one SQL statement (or batch), printing results or error.
async fn execute_sql(conn: &dyn Connection, sql: &str) {
    // Determine if this is likely a read-only query
    let upper = sql.trim_start().to_ascii_uppercase();
    let is_query =
        upper.starts_with("SELECT") || upper.starts_with("WITH") || upper.starts_with("EXPLAIN");

    if is_query {
        match conn.query(sql, &[]).await {
            Ok(rows) => display_rows(&rows),
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
            }
            Err(e) => eprintln!("Error: {e}"),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Parse the optional URI argument (default: memory://)
    let args: Vec<String> = std::env::args().collect();
    let uri = args.get(1).map(|s| s.as_str()).unwrap_or("memory://");

    // Connect via oxisql facade
    let conn = oxisql::connect(uri)
        .await
        .with_context(|| format!("failed to connect to '{uri}'"))?;

    run_repl(conn.as_ref(), uri).await
}
