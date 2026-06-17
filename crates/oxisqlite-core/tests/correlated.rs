//! Integration tests for Slice B — correlated and uncorrelated subqueries in
//! expression position:
//! - scalar subqueries `(SELECT ...)` (correlated & uncorrelated),
//! - `EXISTS` / `NOT EXISTS` (correlated),
//! - `IN (SELECT ...)` (uncorrelated),
//! - the no-row-is-NULL rule and a regression test for the formerly-panicking
//!   correlated scalar subquery.
//!
//! These exercise real bytecode execution end to end against an on-disk
//! database, asserting concrete result values.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_correlated_{}_{}_{}.db",
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

/// A simple owned cell so result assertions can compare without borrowing the
/// statement's row buffer.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Int(i64),
    Real(f64),
    Text(String),
    Null,
    Blob(Vec<u8>),
}

fn to_cell(v: &Value) -> Cell {
    match v {
        Value::Integer(i) => Cell::Int(*i),
        Value::Float(f) => Cell::Real(*f),
        Value::Text(t) => Cell::Text(t.as_str().to_string()),
        Value::Null => Cell::Null,
        Value::Blob(b) => Cell::Blob(b.to_vec()),
    }
}

/// Run a query to completion, returning all rows as owned cells.
fn run_query(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> Vec<Vec<Cell>> {
    let mut stmt = conn
        .query(sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement produced for {sql}"));
    let mut out = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                out.push(row.get_values().map(to_cell).collect());
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Build a fresh DB with two related tables `a` and `b`:
/// a(id, k), b(id, k) so we can express joins via correlated subqueries.
fn setup() -> (Arc<dyn limbo_core::IO>, Arc<Connection>, std::path::PathBuf) {
    let path = temp_db_path("setup");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();
    exec(&conn, "CREATE TABLE a (id INTEGER, k INTEGER)");
    exec(&conn, "CREATE TABLE b (id INTEGER, k INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1, 10), (2, 20), (3, 30)");
    // b has: two rows with k=10, one with k=20, none with k=30, one with k=99.
    exec(
        &conn,
        "INSERT INTO b VALUES (1, 10), (2, 10), (3, 20), (4, 99)",
    );
    (io, conn, path)
}

#[test]
fn scalar_subquery_uncorrelated() {
    let (io, conn, path) = setup();
    // (SELECT max(k) FROM b) is constant for every outer row of a.
    let rows = run_query(
        &io,
        &conn,
        "SELECT id, (SELECT max(k) FROM b) FROM a ORDER BY id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Int(99)],
            vec![Cell::Int(2), Cell::Int(99)],
            vec![Cell::Int(3), Cell::Int(99)],
        ]
    );
    cleanup(&path);
}

#[test]
fn scalar_subquery_correlated() {
    let (io, conn, path) = setup();
    // The canonical correlated COUNT subquery.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, (SELECT COUNT(*) FROM b WHERE b.k = a.k) FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(2)], // two b rows with k=10
            vec![Cell::Int(20), Cell::Int(1)], // one b row with k=20
            vec![Cell::Int(30), Cell::Int(0)], // none with k=30
        ]
    );
    cleanup(&path);
}

#[test]
fn scalar_subquery_correlated_sum() {
    let (io, conn, path) = setup();
    // A correlated scalar subquery returning a non-count aggregate; NULL when no match.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, (SELECT sum(b.id) FROM b WHERE b.k = a.k) FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(3)], // ids 1 + 2
            vec![Cell::Int(20), Cell::Int(3)], // id 3
            vec![Cell::Int(30), Cell::Null],   // no rows -> sum() is NULL
        ]
    );
    cleanup(&path);
}

#[test]
fn scalar_subquery_no_row_is_null() {
    let (io, conn, path) = setup();
    // Uncorrelated subquery over an empty result set yields NULL.
    let rows = run_query(
        &io,
        &conn,
        "SELECT id, (SELECT k FROM b WHERE k = 12345) FROM a ORDER BY id LIMIT 1",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1), Cell::Null]]);
    cleanup(&path);
}

#[test]
fn scalar_subquery_in_where_uncorrelated() {
    let (io, conn, path) = setup();
    // WHERE a.k = (SELECT min(k) FROM b) -> min is 10, so only a.k=10.
    let rows = run_query(
        &io,
        &conn,
        "SELECT id FROM a WHERE a.k = (SELECT min(k) FROM b)",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1)]]);
    cleanup(&path);
}

#[test]
fn correlated_in_where() {
    let (io, conn, path) = setup();
    // WHERE with a correlated scalar subquery comparison:
    // keep a-rows whose k has at least one match in b.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k FROM a WHERE (SELECT COUNT(*) FROM b WHERE b.k = a.k) > 0 ORDER BY a.k",
    );
    assert_eq!(rows, vec![vec![Cell::Int(10)], vec![Cell::Int(20)]]);
    cleanup(&path);
}

#[test]
fn exists_correlated() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.k = a.k) ORDER BY a.k",
    );
    assert_eq!(rows, vec![vec![Cell::Int(10)], vec![Cell::Int(20)]]);
    cleanup(&path);
}

#[test]
fn not_exists_correlated() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k FROM a WHERE NOT EXISTS (SELECT 1 FROM b WHERE b.k = a.k) ORDER BY a.k",
    );
    // Only a.k = 30 has no matching b row.
    assert_eq!(rows, vec![vec![Cell::Int(30)]]);
    cleanup(&path);
}

#[test]
fn exists_correlated_as_result_column() {
    let (io, conn, path) = setup();
    // EXISTS used as a 0/1 value in the SELECT list.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, EXISTS (SELECT 1 FROM b WHERE b.k = a.k) FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(1)],
            vec![Cell::Int(20), Cell::Int(1)],
            vec![Cell::Int(30), Cell::Int(0)],
        ]
    );
    cleanup(&path);
}

#[test]
fn in_subquery_uncorrelated() {
    let (io, conn, path) = setup();
    // a.k IN (SELECT k FROM b) -> b has k in {10, 20, 99}; a has {10,20,30}.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k FROM a WHERE a.k IN (SELECT k FROM b) ORDER BY a.k",
    );
    assert_eq!(rows, vec![vec![Cell::Int(10)], vec![Cell::Int(20)]]);
    cleanup(&path);
}

#[test]
fn not_in_subquery_uncorrelated() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k FROM a WHERE a.k NOT IN (SELECT k FROM b) ORDER BY a.k",
    );
    assert_eq!(rows, vec![vec![Cell::Int(30)]]);
    cleanup(&path);
}

#[test]
fn in_subquery_as_result_column() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, a.k IN (SELECT k FROM b) FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(1)],
            vec![Cell::Int(20), Cell::Int(1)],
            vec![Cell::Int(30), Cell::Int(0)],
        ]
    );
    cleanup(&path);
}

/// Regression test: a correlated scalar subquery in the SELECT list used to hit
/// `todo!()` in `translate_expr` and panic. It must now return correct rows.
#[test]
fn regression_correlated_scalar_no_longer_panics() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.id, a.k, (SELECT COUNT(*) FROM b WHERE b.k = a.k) AS n FROM a ORDER BY a.id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Int(10), Cell::Int(2)],
            vec![Cell::Int(2), Cell::Int(20), Cell::Int(1)],
            vec![Cell::Int(3), Cell::Int(30), Cell::Int(0)],
        ]
    );
    cleanup(&path);
}

/// A subquery nested inside another subquery, where the innermost query is
/// correlated to the outermost scope. Exercises grandparent-scope forwarding.
#[test]
fn nested_correlated_subquery() {
    let (io, conn, path) = setup();
    // For each a-row, count b-rows whose k matches a.k, but only when that count is positive.
    // The inner EXISTS is correlated to `a` two levels up.
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, \
           (SELECT COUNT(*) FROM b WHERE b.k = a.k AND EXISTS \
             (SELECT 1 FROM b b2 WHERE b2.k = a.k)) \
         FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(2)],
            vec![Cell::Int(20), Cell::Int(1)],
            vec![Cell::Int(30), Cell::Int(0)],
        ]
    );
    cleanup(&path);
}

/// Two correlated subqueries in the same SELECT list, to ensure each gets its
/// own coroutine/registers and they don't clobber one another.
#[test]
fn two_correlated_subqueries_same_select() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k, \
           (SELECT COUNT(*) FROM b WHERE b.k = a.k), \
           (SELECT sum(b.id) FROM b WHERE b.k = a.k) \
         FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(10), Cell::Int(2), Cell::Int(3)],
            vec![Cell::Int(20), Cell::Int(1), Cell::Int(3)],
            vec![Cell::Int(30), Cell::Int(0), Cell::Null],
        ]
    );
    cleanup(&path);
}

/// A scalar subquery whose value participates in arithmetic, to ensure the
/// result register flows into the surrounding expression correctly.
#[test]
fn scalar_subquery_in_arithmetic() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT a.k + (SELECT COUNT(*) FROM b WHERE b.k = a.k) FROM a ORDER BY a.k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(12)], // 10 + 2
            vec![Cell::Int(21)], // 20 + 1
            vec![Cell::Int(30)], // 30 + 0
        ]
    );
    cleanup(&path);
}
