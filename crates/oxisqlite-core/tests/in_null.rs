//! Integration tests for three-valued-logic correctness of `IN (SELECT …)` / `NOT IN (SELECT …)`.
//!
//! SQLite rules:
//!   x IN  (set): 1 if match; NULL if no match and set contains NULL; else 0.
//!   x NOT IN (set): 0 if match; NULL if no match and set contains NULL; else 1.
//!   NULL IN/NOT IN (any): always NULL.
//!
//! These tests use WHERE to distinguish NULL from 0/1: WHERE NULL → row excluded.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_in_null_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn row_count(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> usize {
    let mut stmt = conn
        .query(sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement produced for {sql}"));
    let mut count = 0usize;
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => count += 1,
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    count
}

/// Create a small DB:
///   outer(x INTEGER)          — one row: x = 5
///   outer_match(x INTEGER)    — one row: x = 1 (matches the inner set)
///   outer_null(x INTEGER)     — one row: x = NULL
///   inner_with_null(v INTEGER) — two rows: 1, NULL
///   inner_no_null(v INTEGER)   — three rows: 1, 2, 3
fn setup() -> (Arc<dyn limbo_core::IO>, Arc<Connection>, std::path::PathBuf) {
    let path = temp_db_path("setup");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();

    exec(&conn, "CREATE TABLE outer_t (x INTEGER)");
    exec(&conn, "INSERT INTO outer_t VALUES (5)");

    exec(&conn, "CREATE TABLE outer_match (x INTEGER)");
    exec(&conn, "INSERT INTO outer_match VALUES (1)");

    exec(&conn, "CREATE TABLE outer_null (x INTEGER)");
    exec(&conn, "INSERT INTO outer_null VALUES (NULL)");

    exec(&conn, "CREATE TABLE inner_with_null (v INTEGER)");
    exec(&conn, "INSERT INTO inner_with_null VALUES (1)");
    exec(&conn, "INSERT INTO inner_with_null VALUES (NULL)");

    exec(&conn, "CREATE TABLE inner_no_null (v INTEGER)");
    exec(&conn, "INSERT INTO inner_no_null VALUES (1)");
    exec(&conn, "INSERT INTO inner_no_null VALUES (2)");
    exec(&conn, "INSERT INTO inner_no_null VALUES (3)");

    (io, conn, path)
}

/// `5 NOT IN {1, NULL}` = NULL  →  WHERE NULL  →  row excluded  →  0 rows.
/// With the bug (returns 1 instead of NULL), this would have returned 1 row.
#[test]
fn not_in_set_with_null_no_match_is_null() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_t WHERE x NOT IN (SELECT v FROM inner_with_null)",
    );
    assert_eq!(n, 0, "5 NOT IN {{1, NULL}} should be NULL, not 1");
    cleanup(&path);
}

/// `5 IN {1, NULL}` = NULL  →  WHERE NULL  →  row excluded  →  0 rows.
#[test]
fn in_set_with_null_no_match_is_null() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_t WHERE x IN (SELECT v FROM inner_with_null)",
    );
    assert_eq!(n, 0, "5 IN {{1, NULL}} should be NULL, not 0");
    cleanup(&path);
}

/// `1 NOT IN {1, NULL}` = 0 (match found)  →  WHERE 0  →  row excluded  →  0 rows.
#[test]
fn not_in_set_with_null_but_lhs_matches_is_false() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_match WHERE x NOT IN (SELECT v FROM inner_with_null)",
    );
    assert_eq!(n, 0, "1 NOT IN {{1, NULL}} should be 0 (match found)");
    cleanup(&path);
}

/// `5 IN {1, 2, 3}` = 0 (no match, no NULL)  →  WHERE 0  →  row excluded  →  0 rows.
/// Regression: definite-false path must still work.
#[test]
fn in_set_no_null_no_match_is_false_definite() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_t WHERE x IN (SELECT v FROM inner_no_null)",
    );
    assert_eq!(n, 0, "5 IN {{1,2,3}} should be definite 0");
    cleanup(&path);
}

/// `5 NOT IN {1, 2, 3}` = 1 (no match, no NULL)  →  WHERE 1  →  row included  →  1 row.
/// Regression: definite-true path must still work.
#[test]
fn not_in_set_no_null_no_match_is_true_definite() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_t WHERE x NOT IN (SELECT v FROM inner_no_null)",
    );
    assert_eq!(n, 1, "5 NOT IN {{1,2,3}} should be definite 1");
    cleanup(&path);
}

/// `NULL IN {1, 2, 3}` = NULL (LHS is NULL)  →  WHERE NULL  →  row excluded  →  0 rows.
/// Regression: LHS-NULL path must still produce NULL.
#[test]
fn lhs_null_still_null() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_null WHERE x IN (SELECT v FROM inner_no_null)",
    );
    assert_eq!(n, 0, "NULL IN {{1,2,3}} should be NULL");
    cleanup(&path);
}

/// Regression: `1 IN {1, NULL}` = 1 (match found even though NULL present).
#[test]
fn in_set_with_null_but_lhs_matches_is_true() {
    let (io, conn, path) = setup();
    let n = row_count(
        &io,
        &conn,
        "SELECT x FROM outer_match WHERE x IN (SELECT v FROM inner_with_null)",
    );
    assert_eq!(n, 1, "1 IN {{1, NULL}} should be 1 (match found)");
    cleanup(&path);
}
