//! Integration tests for the PostgreSQL COPY protocol implementation.
//!
//! These tests require a live PostgreSQL server.  They are gated with
//! `#[ignore]` so they are skipped in CI unless explicitly opted in with
//! `cargo test -- --ignored`.
//!
//! To run:
//! ```text
//! cargo test -p oxisql-postgres --test copy -- --ignored
//! ```
//!
//! The tests connect to `host=localhost port=5432 user=postgres`.
//! Adjust `PG_CONN_STR` below if your server uses different credentials.

use oxisql_postgres::{PgConnection, TlsMode};

const PG_CONN_STR: &str = "host=localhost port=5432 user=postgres";

/// Round-trip test: copy 3 rows into a temp table and copy them back out.
///
/// Verifies:
/// - `copy_in_text` inserts the correct number of rows.
/// - `copy_out_text` retrieves the same rows.
/// - Values containing special characters (tab, newline, backslash) survive
///   the escape/unescape cycle intact.
#[tokio::test]
#[ignore = "requires a live PostgreSQL server"]
async fn test_copy_in_and_out() {
    let conn = PgConnection::connect(PG_CONN_STR, TlsMode::Disabled)
        .await
        .expect("connect");

    // Create a temporary table (auto-dropped when session ends).
    conn.copy_in_text("", &[], std::iter::empty::<Vec<String>>())
        .await
        .expect_err("empty columns should fail");

    // Use oxisql_core Connection trait for DDL.
    use oxisql_core::Connection;
    conn.execute_batch(
        "CREATE TEMP TABLE oxisql_copy_test (\
            id   TEXT NOT NULL,\
            name TEXT NOT NULL,\
            note TEXT NOT NULL\
        )",
    )
    .await
    .expect("CREATE TEMP TABLE");

    let rows: Vec<Vec<String>> = vec![
        vec!["1".into(), "Alice".into(), "plain text".into()],
        vec!["2".into(), "Bob".into(), "with\nnewline".into()],
        vec![
            "3".into(),
            "Carol".into(),
            "with\ttab and \\backslash".into(),
        ],
    ];

    let count = conn
        .copy_in_text(
            "oxisql_copy_test",
            &["id", "name", "note"],
            rows.clone().into_iter(),
        )
        .await
        .expect("copy_in_text");

    assert_eq!(count, 3, "expected 3 rows inserted");

    let out = conn
        .copy_out_text("oxisql_copy_test", &["id", "name", "note"])
        .await
        .expect("copy_out_text");

    assert_eq!(out.len(), 3, "expected 3 rows returned");
    // Rows may be returned in insertion order (temp table, no ORDER BY) but
    // text COPY outputs in storage order — for a freshly inserted temp table
    // this matches insertion order.  Sort both to be safe.
    let mut sorted_in = rows;
    sorted_in.sort();
    let mut sorted_out = out;
    sorted_out.sort();
    assert_eq!(sorted_in, sorted_out, "round-trip mismatch");
}
