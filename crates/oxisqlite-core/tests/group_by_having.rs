//! Integration tests for GROUP BY ... HAVING aggregate queries, exercising the
//! real bytecode execution path end to end against an on-disk database.
//!
//! Regression coverage for a VDBE bug where the GROUP BY "clear accumulator"
//! subroutine reset too few registers: when a query contains a zero-argument
//! aggregate (e.g. `COUNT(*)` reached via the HAVING clause, whose planner path
//! gives it empty `args`), `group_by_sorter_column_count()` under-counted the
//! aggregate-accumulator block, so the trailing aggregate register was never
//! NULLed between groups. A finalized `Integer` count from a preceding
//! single-row group then survived into the next group's first `AggStep`, whose
//! `Value::Null`-only init guard did not fire, and execution panicked with
//! "Unexpected value Value(Integer(1)) in AggStep at register N".
//!
//! The data below deliberately places size-1 groups *before* larger groups
//! (group sizes [1, 3, 1, 2] in key order) so the stale `Integer(1)` is present
//! at the moment the next group begins accumulating.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall io"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_group_by_having_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

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
            StepResult::IO => io.run_once().expect("io"),
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

/// Build a fresh DB with table `t(k, v)` whose rows form groups of MIXED sizes
/// in key order: k=1 -> 1 row, k=2 -> 3 rows, k=3 -> 1 row, k=4 -> 2 rows
/// (group sizes [1, 3, 1, 2]). Rows are inserted interleaved so the GROUP BY
/// sorter actually has to reorder them.
fn setup() -> (Arc<dyn limbo_core::IO>, Arc<Connection>, std::path::PathBuf) {
    let path = temp_db_path("setup");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().expect("path"), false).expect("open");
    let conn = db.connect().expect("connect");
    exec(&conn, "CREATE TABLE t (k INTEGER, v INTEGER)");
    exec(
        &conn,
        "INSERT INTO t (k, v) VALUES (1, 10), (2, 5), (3, 100), (2, 7), (4, 2), (2, 9), (4, 8)",
    );
    (io, conn, path)
}

/// The reported bug: `SELECT <key>, COUNT(*) ... GROUP BY <key> HAVING COUNT(*) > 1`.
/// COUNT(*) appears in both the result and the HAVING clause, so the plan holds
/// two aggregates; the HAVING one has empty `args`, which triggered the
/// under-clearing. A size-1 group (k=1) precedes the first surviving group (k=2).
#[test]
fn count_having_mixed_group_sizes() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT k, COUNT(*) FROM t GROUP BY k HAVING COUNT(*) > 1 ORDER BY k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(2), Cell::Int(3)], // k=2 has 3 rows
            vec![Cell::Int(4), Cell::Int(2)], // k=4 has 2 rows
        ],
        "COUNT(*) must be exactly the per-group row count, with no stale carry-over"
    );
    cleanup(&path);
}

/// Exact shape of the downstream failing query `SELECT hash ... GROUP BY hash
/// HAVING COUNT(*) > 1`: the aggregate exists ONLY in the HAVING clause (empty
/// args), which is the worst case for the under-clearing bug.
#[test]
fn count_having_key_only_projection() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT k FROM t GROUP BY k HAVING COUNT(*) > 1 ORDER BY k",
    );
    assert_eq!(rows, vec![vec![Cell::Int(2)], vec![Cell::Int(4)]]);
    cleanup(&path);
}

/// The non-grouped path (a single implicit group) must still return the right
/// total after the fix.
#[test]
fn bare_count_non_grouped() {
    let (io, conn, path) = setup();
    let rows = run_query(&io, &conn, "SELECT COUNT(*) FROM t");
    assert_eq!(rows, vec![vec![Cell::Int(7)]]);

    // A grouped COUNT without HAVING must also list every group with the right count.
    let all = run_query(
        &io,
        &conn,
        "SELECT k, COUNT(*) FROM t GROUP BY k ORDER BY k",
    );
    assert_eq!(
        all,
        vec![
            vec![Cell::Int(1), Cell::Int(1)],
            vec![Cell::Int(2), Cell::Int(3)],
            vec![Cell::Int(3), Cell::Int(1)],
            vec![Cell::Int(4), Cell::Int(2)],
        ]
    );
    cleanup(&path);
}

/// SUM across mixed-size groups must be CORRECT (not merely non-panicking): the
/// per-group accumulator must be zeroed at each boundary, so k=2 sums to exactly
/// 21 (5+7+9), never 21 plus a prior group's carry.
#[test]
fn sum_having_mixed_group_sizes() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT k, SUM(v) FROM t GROUP BY k HAVING SUM(v) > 15 ORDER BY k",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(2), Cell::Int(21)],  // 5 + 7 + 9
            vec![Cell::Int(3), Cell::Int(100)], // single row
        ]
    );
    cleanup(&path);
}

/// Multiple aggregates (SUM/AVG/MIN/MAX) alongside a HAVING `COUNT(*)`
/// (zero-arg, the under-clearing trigger). This both reproduces the panic on the
/// buggy engine and checks that every aggregate arm produces the correct
/// per-group value.
#[test]
fn multi_aggregate_group_by_having_count() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT k, SUM(v), AVG(v), MIN(v), MAX(v) FROM t \
         GROUP BY k HAVING COUNT(*) > 1 ORDER BY k",
    );
    assert_eq!(
        rows,
        vec![
            // k=2: rows {5,7,9} -> sum 21, avg 7.0, min 5, max 9
            vec![
                Cell::Int(2),
                Cell::Int(21),
                Cell::Real(7.0),
                Cell::Int(5),
                Cell::Int(9),
            ],
            // k=4: rows {2,8} -> sum 10, avg 5.0, min 2, max 8
            vec![
                Cell::Int(4),
                Cell::Int(10),
                Cell::Real(5.0),
                Cell::Int(2),
                Cell::Int(8),
            ],
        ]
    );
    cleanup(&path);
}

/// A HAVING clause where a non-COUNT aggregate (SUM) is the trailing register
/// after a zero-arg COUNT(*), i.e. SUM is the accumulator that the buggy clear
/// range failed to reset. Proves the SUM arm no longer panics across groups and
/// still filters correctly.
#[test]
fn sum_as_trailing_uncovered_accumulator() {
    let (io, conn, path) = setup();
    let rows = run_query(
        &io,
        &conn,
        "SELECT k FROM t GROUP BY k HAVING COUNT(*) > 1 AND SUM(v) > 15 ORDER BY k",
    );
    // k=2: count 3, sum 21 -> passes both. k=4: count 2, sum 10 -> fails SUM>15.
    assert_eq!(rows, vec![vec![Cell::Int(2)]]);
    cleanup(&path);
}
