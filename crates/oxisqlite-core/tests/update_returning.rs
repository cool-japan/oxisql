//! Regression tests for `UPDATE ... RETURNING`.
//!
//! Historically the UPDATE row-loop emitter parsed the RETURNING clause into
//! `UpdatePlan::returning` and used it to populate `program.result_columns`
//! metadata, but never actually emitted `Insn::ResultRow` for the updated
//! rows — so `UPDATE ... RETURNING` silently produced zero rows instead of
//! the updated data, without raising any error.

use limbo_core::{Database, MemoryIO, StepResult, Value};
use std::sync::Arc;

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

/// Execute a statement that produces no rows (DDL / plain DML), pumping IO as needed.
fn exec(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Result<(), limbo_core::LimboError> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Done => return Ok(()),
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(limbo_core::LimboError::Busy),
        }
    }
}

/// Execute a statement and collect every row's column values, in order.
fn collect_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<Vec<Value>> {
    let mut stmt = conn.prepare(sql).expect("prepare");
    let mut rows = Vec::new();
    loop {
        match stmt.step().expect("step") {
            StepResult::Row => {
                let row = stmt
                    .row()
                    .expect("row must be present after StepResult::Row");
                rows.push(row.get_values().cloned().collect());
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

/// Run a `SELECT COUNT(*) ...` style query and return the single integer result.
fn count(io: &Arc<dyn limbo_core::IO>, conn: &Arc<limbo_core::Connection>, sql: &str) -> i64 {
    let mut stmt = conn.prepare(sql).expect("prepare count");
    loop {
        match stmt.step().expect("step count") {
            StepResult::Row => {
                return match stmt.row().expect("row").get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer count, got {other:?}"),
                };
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => panic!("count query returned no row"),
            StepResult::Interrupt => panic!("interrupted in count query"),
        }
    }
}

#[test]
fn update_returning_computes_post_update_values() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER, y INTEGER)",
    )
    .expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 10, 1)").expect("seed row 1");
    exec(&io, &conn, "INSERT INTO t VALUES (2, 20, 6)").expect("seed row 2");
    exec(&io, &conn, "INSERT INTO t VALUES (3, 30, 7)").expect("seed row 3");
    exec(&io, &conn, "INSERT INTO t VALUES (4, 40, 3)").expect("seed row 4");

    let rows = collect_rows(
        &io,
        &conn,
        "UPDATE t SET x = x + 1 WHERE y > 5 RETURNING x, y",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(21), Value::Integer(6)],
            vec![Value::Integer(31), Value::Integer(7)],
        ],
        "RETURNING must reflect post-update x values for exactly the rows matched by WHERE, not an empty result"
    );

    // The underlying table must actually have been updated to match what was returned.
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 21 AND y = 6"),
        1
    );
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 31 AND y = 7"),
        1
    );
    // Rows outside the WHERE filter must be untouched (and thus correctly absent from RETURNING).
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 10 AND y = 1"),
        1
    );
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 40 AND y = 3"),
        1
    );
}

#[test]
fn update_returning_rowid_reports_correct_rowids() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER)",
    )
    .expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 100)").expect("seed row 1");
    exec(&io, &conn, "INSERT INTO t VALUES (2, 200)").expect("seed row 2");
    exec(&io, &conn, "INSERT INTO t VALUES (3, 300)").expect("seed row 3");

    let rows = collect_rows(&io, &conn, "UPDATE t SET x = 1 RETURNING rowid");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
        ],
        "RETURNING rowid must report each updated row's correct rowid"
    );

    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 1"),
        3,
        "every row must have actually been updated"
    );
}

#[test]
fn update_returning_respects_limit() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER)",
    )
    .expect("create table");
    for i in 1..=5 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, 0)")).expect("seed row");
    }

    // NOTE: this engine's grammar (matching upstream SQLite) places RETURNING
    // *before* ORDER BY / LIMIT for UPDATE: `... WHERE expr RETURNING exprlist
    // [ORDER BY ...] [LIMIT ...]`, not after LIMIT.
    let rows = collect_rows(
        &io,
        &conn,
        "UPDATE t SET x = 1 WHERE x = 0 RETURNING x LIMIT 2",
    );
    assert_eq!(
        rows.len(),
        2,
        "RETURNING must produce exactly as many rows as were actually updated under LIMIT 2, not more"
    );
    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(1)]]);

    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 1"),
        2,
        "LIMIT 2 must have updated exactly 2 rows, matching the RETURNING row count"
    );
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 0"),
        3,
        "the remaining 3 rows must be untouched by LIMIT 2"
    );
}

#[test]
fn update_returning_empty_match_returns_zero_rows_without_error() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER)",
    )
    .expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 10)").expect("seed row");

    // 1=0 matches nothing: this is a *legitimately* empty RETURNING result,
    // distinct from the bug (which produced an empty result for every UPDATE
    // ... RETURNING, even when rows were in fact updated).
    let rows = collect_rows(&io, &conn, "UPDATE t SET x = 1 WHERE 1 = 0 RETURNING x");
    assert!(
        rows.is_empty(),
        "a RETURNING clause matching no rows must legitimately return zero rows"
    );

    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x = 10"),
        1,
        "no row should have been updated"
    );
}

#[test]
fn update_without_returning_still_emits_no_result_rows() {
    // Regression guard: a plain UPDATE (no RETURNING clause) must not start
    // emitting spurious empty result rows now that RETURNING emission exists.
    // `UpdatePlan::returning` is always `Some(vec![])` (not `None`) when no
    // RETURNING clause is present, so the fix must gate on emptiness too.
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER)",
    )
    .expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 10)").expect("seed row 1");
    exec(&io, &conn, "INSERT INTO t VALUES (2, 20)").expect("seed row 2");

    let rows = collect_rows(&io, &conn, "UPDATE t SET x = x + 1");
    assert!(
        rows.is_empty(),
        "UPDATE without RETURNING must not emit any result rows"
    );
    assert_eq!(
        count(&io, &conn, "SELECT COUNT(*) FROM t WHERE x IN (11, 21)"),
        2
    );
}
