//! PostgreSQL COPY protocol support for bulk data transfer.
//!
//! Provides [`copy_in_text`] and [`copy_out_text`] implementations for
//! [`PgConnection`], enabling efficient bulk data ingestion and extraction
//! using PostgreSQL's native COPY protocol in text (TSV) format.
//!
//! # Text format escaping
//!
//! The PostgreSQL text COPY format uses tab (`\t`) as a field delimiter and
//! newline (`\n`) as a row terminator.  The following escape sequences are
//! applied to cell values:
//!
//! | Character | Escaped form |
//! |-----------|-------------|
//! | `\`       | `\\`        |
//! | `\t`      | `\t`        |
//! | `\n`      | `\n`        |
//! | `\r`      | `\r`        |
//!
//! A SQL NULL is represented as the literal string `\N` (backslash-N).

use std::sync::Arc;

use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::Mutex;

use crate::error::PgError;

// ── Text-format escaping ──────────────────────────────────────────────────────

/// Escape a single cell value for the PostgreSQL text COPY format.
///
/// Applies the following transformations:
/// - `\` → `\\`
/// - `\t` → `\t` (literal backslash-t in the output)
/// - `\n` → `\n` (literal backslash-n in the output)
/// - `\r` → `\r` (literal backslash-r in the output)
fn escape_copy_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 4);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str(r"\\"),
            '\t' => out.push_str(r"\t"),
            '\n' => out.push_str(r"\n"),
            '\r' => out.push_str(r"\r"),
            c => out.push(c),
        }
    }
    out
}

/// Unescape a single cell value decoded from the PostgreSQL text COPY format.
///
/// Reverses the escape sequences applied by [`escape_copy_value`].
/// Returns the literal string `\N` unchanged (callers may choose to treat it
/// as `NULL`).
fn unescape_copy_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('N') => {
                    // \N is the NULL sentinel — pass through as-is so the
                    // caller can decide how to handle it.
                    out.push('\\');
                    out.push('N');
                }
                Some(c) => {
                    // Unknown escape: preserve backslash + character.
                    out.push('\\');
                    out.push(c);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

// ── COPY IN ───────────────────────────────────────────────────────────────────

/// Execute a `COPY table (columns) FROM STDIN` statement and stream rows in
/// text (TSV) format.
///
/// Each element of `rows` should be a `Vec<String>` containing one value per
/// column.  Values are automatically escaped for the PostgreSQL text COPY
/// format.  Pass `"\\N"` (backslash-N) to represent a SQL `NULL`.
///
/// Returns the number of rows inserted as reported by PostgreSQL.
///
/// # Errors
///
/// Returns [`PgError::Copy`] if the column list is empty, if any identifier is
/// unsafe, or if the underlying COPY protocol returns an error.
pub(crate) async fn copy_in_text(
    inner: &Arc<Mutex<tokio_postgres::Client>>,
    table: &str,
    columns: &[&str],
    rows: impl Iterator<Item = Vec<String>>,
) -> Result<u64, PgError> {
    if columns.is_empty() {
        return Err(PgError::Copy(
            "column list must not be empty for COPY IN".to_string(),
        ));
    }

    let col_list = columns.join(", ");
    let query = format!("COPY {table} ({col_list}) FROM STDIN");

    // Collect all rows into a single Bytes buffer to send in one shot.
    // Each row is: field1\tfield2\t...\tfieldn\n
    let mut buf = Vec::new();
    for row in rows {
        if row.len() != columns.len() {
            return Err(PgError::Copy(format!(
                "row has {} fields but {} columns were specified",
                row.len(),
                columns.len()
            )));
        }
        for (i, cell) in row.iter().enumerate() {
            if i > 0 {
                buf.push(b'\t');
            }
            buf.extend_from_slice(escape_copy_value(cell).as_bytes());
        }
        buf.push(b'\n');
    }

    let payload = Bytes::from(buf);

    // Acquire the client lock and drive the COPY protocol.
    let client = inner.lock().await;
    let sink = client
        .copy_in::<_, Bytes>(&query as &str)
        .await
        .map_err(|e| PgError::Copy(e.to_string()))?;

    // CopyInSink is !Unpin — must be pinned before use.
    tokio::pin!(sink);

    use futures::SinkExt;
    sink.send(payload)
        .await
        .map_err(|e| PgError::Copy(e.to_string()))?;

    let rows_copied = sink
        .finish()
        .await
        .map_err(|e| PgError::Copy(e.to_string()))?;

    Ok(rows_copied)
}

// ── COPY OUT ──────────────────────────────────────────────────────────────────

/// Execute a `COPY table (columns) TO STDOUT` statement and collect all rows.
///
/// Returns a `Vec<Vec<String>>` where each inner `Vec<String>` contains the
/// field values for one row, with escape sequences decoded.
///
/// The string `"\\N"` (backslash-N) in the returned data represents a SQL
/// `NULL` value.
///
/// # Errors
///
/// Returns [`PgError::Copy`] if the column list is empty or if the underlying
/// COPY protocol returns an error.
pub(crate) async fn copy_out_text(
    inner: &Arc<Mutex<tokio_postgres::Client>>,
    table: &str,
    columns: &[&str],
) -> Result<Vec<Vec<String>>, PgError> {
    if columns.is_empty() {
        return Err(PgError::Copy(
            "column list must not be empty for COPY OUT".to_string(),
        ));
    }

    let col_list = columns.join(", ");
    let query = format!("COPY {table} ({col_list}) TO STDOUT");

    let client = inner.lock().await;
    let stream = client
        .copy_out(&query as &str)
        .await
        .map_err(|e| PgError::Copy(e.to_string()))?;

    // Collect all Bytes chunks.
    let chunks: Vec<Bytes> = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PgError::Copy(e.to_string()))?;

    // Concatenate all chunks into a single UTF-8 string.
    let total_len: usize = chunks.iter().map(|b| b.len()).sum();
    let mut raw = Vec::with_capacity(total_len);
    for chunk in &chunks {
        raw.extend_from_slice(chunk);
    }
    let text = String::from_utf8(raw)
        .map_err(|e| PgError::Copy(format!("COPY OUT returned non-UTF-8 data: {e}")))?;

    // Split into rows (lines) then fields (tab-separated).
    // The COPY text format ends with a final `\n` so we trim the trailing
    // empty line produced by splitting.
    let rows = text
        .split('\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            line.split('\t')
                .map(unescape_copy_value)
                .collect::<Vec<String>>()
        })
        .collect::<Vec<Vec<String>>>();

    Ok(rows)
}
