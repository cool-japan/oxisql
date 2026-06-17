use limbo_core::{Database, MemoryIO, StepResult, Value};
use std::sync::Arc;

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

fn exec(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Result<(), limbo_core::LimboError> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Done => return Ok(()),
            // Both IO and Busy require an io.run_once() pump before retrying.
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(limbo_core::LimboError::Busy),
        }
    }
}

fn count_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    table: &str,
) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut stmt = conn.prepare(&sql).expect("prepare count");
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
fn insert_or_fail_keeps_prior_rows() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed first row");
    // OR FAIL: conflict on id=1 should return an error but keep id=1 row
    let result = exec(&io, &conn, "INSERT OR FAIL INTO t VALUES (1)");
    assert!(result.is_err(), "INSERT OR FAIL must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "prior row must survive OR FAIL"
    );
}

#[test]
fn insert_or_abort_removes_partial_rows() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    // Multi-row INSERT: first value (2) would succeed, second (1) conflicts.
    // OR ABORT must roll back the partial insert (id=2), leaving only seed (id=1).
    let result = exec(&io, &conn, "INSERT OR ABORT INTO t VALUES (2), (1)");
    assert!(result.is_err(), "INSERT OR ABORT must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "ABORT must roll back partial multi-row insert"
    );
}

#[test]
fn insert_or_rollback_errors_on_conflict() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    // OR ROLLBACK: conflict must return an error
    let result = exec(&io, &conn, "INSERT OR ROLLBACK INTO t VALUES (1)");
    assert!(result.is_err(), "INSERT OR ROLLBACK must error on conflict");
}

#[test]
fn insert_or_ignore_skips_conflicting_row() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    exec(&io, &conn, "INSERT OR IGNORE INTO t VALUES (1)").expect("IGNORE must not error");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "IGNORE must skip the conflicting row"
    );
}

#[test]
fn insert_default_abort_removes_partial_rows() {
    // Default INSERT (no explicit OR clause) must behave like ABORT:
    // conflict reverts ALL rows inserted by this statement.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    let result = exec(&io, &conn, "INSERT INTO t VALUES (2), (1)");
    assert!(result.is_err(), "default INSERT must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "default INSERT ABORT must roll back partial inserts"
    );
}
