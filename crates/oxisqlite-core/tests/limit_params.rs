use limbo_core::{Database, MemoryIO, StepResult, Value};
use std::num::NonZero;
use std::sync::Arc;

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

/// Execute a statement that produces no rows (DDL / DML), pumping IO as needed.
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

/// Step a prepared statement (with bound params already set) and
/// collect integer values from column 0.
fn collect_stmt_rows(io: &Arc<dyn limbo_core::IO>, stmt: &mut limbo_core::Statement) -> Vec<i64> {
    let mut rows = Vec::new();
    loop {
        match stmt.step().expect("step") {
            StepResult::Row => {
                let row = stmt
                    .row()
                    .expect("row must be present after StepResult::Row");
                match row.get_value(0) {
                    Value::Integer(v) => rows.push(*v),
                    other => panic!("expected integer, got {other:?}"),
                }
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

/// Seed a fresh in-memory table with rows 1..=n and return (io, conn).
fn seed_table(n: i64) -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (v INTEGER)").expect("create table");
    for i in 1..=n {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert row");
    }
    (io, conn)
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests: LIMIT ?
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn limit_param_positive() {
    let (io, conn) = seed_table(10);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(3));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(rows, vec![1, 2, 3], "LIMIT ? = 3 should return 3 rows");
}

#[test]
fn limit_param_zero() {
    let (io, conn) = seed_table(5);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(0));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert!(rows.is_empty(), "LIMIT ? = 0 must return no rows");
}

#[test]
fn limit_param_negative_means_unlimited() {
    let (io, conn) = seed_table(5);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(-1));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(
        rows,
        vec![1, 2, 3, 4, 5],
        "LIMIT ? = -1 should return all rows"
    );
}

#[test]
fn limit_param_null_means_unlimited() {
    let (io, conn) = seed_table(5);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Null);
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(
        rows,
        vec![1, 2, 3, 4, 5],
        "LIMIT ? = NULL should return all rows"
    );
}

#[test]
fn limit_param_larger_than_table() {
    let (io, conn) = seed_table(3);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(100));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(rows, vec![1, 2, 3], "LIMIT ? > table size returns all rows");
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests: LIMIT ? OFFSET ?
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn limit_and_offset_params() {
    let (io, conn) = seed_table(10);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ? OFFSET ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(3));
    stmt.bind_at(NonZero::new(2).expect("nonzero"), Value::Integer(4));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(
        rows,
        vec![5, 6, 7],
        "LIMIT 3 OFFSET 4 should return rows 5-7"
    );
}

#[test]
fn offset_param_skips_all_rows() {
    let (io, conn) = seed_table(5);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ? OFFSET ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(10));
    stmt.bind_at(NonZero::new(2).expect("nonzero"), Value::Integer(100));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert!(rows.is_empty(), "OFFSET beyond table size returns no rows");
}

#[test]
fn offset_param_zero_acts_like_no_offset() {
    let (io, conn) = seed_table(5);
    let mut stmt = conn
        .prepare("SELECT v FROM t ORDER BY v LIMIT ? OFFSET ?")
        .expect("prepare");
    stmt.bind_at(NonZero::new(1).expect("nonzero"), Value::Integer(3));
    stmt.bind_at(NonZero::new(2).expect("nonzero"), Value::Integer(0));
    let rows = collect_stmt_rows(&io, &mut stmt);
    assert_eq!(rows, vec![1, 2, 3], "OFFSET 0 should behave like no OFFSET");
}
