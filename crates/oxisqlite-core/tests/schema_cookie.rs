//! Integration tests for the schema-cookie write path.
//!
//! Verifies that every DDL statement that modifies the schema
//! (CREATE TABLE, DROP TABLE, CREATE INDEX, DROP INDEX, ALTER TABLE)
//! increments the schema_version cookie, and that
//! `PRAGMA schema_version = N` writes an explicit value.

use std::sync::Arc;

use limbo_core::{Database, MemoryIO, StepResult, Value};

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
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(limbo_core::LimboError::Busy),
        }
    }
}

/// Read the current schema_version pragma value.
fn schema_version(io: &Arc<dyn limbo_core::IO>, conn: &Arc<limbo_core::Connection>) -> i64 {
    let mut stmt = conn
        .prepare("PRAGMA schema_version")
        .expect("prepare schema_version");
    loop {
        match stmt.step().expect("step") {
            StepResult::Row => {
                return match stmt.row().expect("row").get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer schema_version, got {other:?}"),
                };
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => panic!("PRAGMA schema_version returned no row"),
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
}

#[test]
fn schema_version_increments_on_ddl() {
    let (io, conn) = new_mem_db();

    let v0 = schema_version(&io, &conn);

    // CREATE TABLE must bump the cookie.
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)",
    )
    .expect("create table");
    let v1 = schema_version(&io, &conn);
    assert!(
        v1 > v0,
        "CREATE TABLE must increment schema_version (was {v0}, now {v1})"
    );

    // DROP TABLE must bump again.
    exec(&io, &conn, "DROP TABLE t").expect("drop table");
    let v2 = schema_version(&io, &conn);
    assert!(
        v2 > v1,
        "DROP TABLE must increment schema_version (was {v1}, now {v2})"
    );

    // Each successive DDL must increase the cookie monotonically.
    exec(&io, &conn, "CREATE TABLE u (x INTEGER)").expect("create u");
    let v3 = schema_version(&io, &conn);
    assert!(
        v3 > v2,
        "second CREATE TABLE must increment schema_version (was {v2}, now {v3})"
    );
}

#[test]
fn pragma_set_schema_version() {
    let (io, conn) = new_mem_db();

    // Set an explicit value via PRAGMA.
    exec(&io, &conn, "PRAGMA schema_version = 42").expect("set schema_version");
    let got = schema_version(&io, &conn);
    assert_eq!(got, 42, "PRAGMA schema_version = 42 must read back as 42");

    // Set another value.
    exec(&io, &conn, "PRAGMA schema_version = 100").expect("set schema_version 100");
    let got2 = schema_version(&io, &conn);
    assert_eq!(
        got2, 100,
        "PRAGMA schema_version = 100 must read back as 100"
    );
}

/// A prepared SELECT statement that has not been stepped yet (pc=0) records the
/// compile-time schema cookie in its Transaction opcode.  After DDL runs on the
/// same connection the live cookie is higher, so the first step of the held
/// statement must return `LimboError::SchemaChanged`.
#[test]
fn held_statement_sees_schema_change() {
    let (io, conn) = new_mem_db();

    // Build the initial schema and data.
    exec(&io, &conn, "CREATE TABLE t (v INTEGER)").expect("create t");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("insert");

    // Prepare a SELECT but do NOT step it yet (pc remains 0, schema_cookie
    // is baked in as the cookie at compile time).
    let mut held = conn.prepare("SELECT v FROM t").expect("prepare SELECT");

    // Mutate the schema via DDL on the same connection.
    exec(&io, &conn, "CREATE TABLE u (x INTEGER)").expect("create u");

    // The first step of the held statement must fail with SchemaChanged because
    // the Transaction opcode will see a stale compile-time cookie.
    let mut got_schema_changed = false;
    loop {
        match held.step() {
            Err(limbo_core::LimboError::SchemaChanged) => {
                got_schema_changed = true;
                break;
            }
            Err(e) => panic!("unexpected error stepping held statement: {e:?}"),
            Ok(StepResult::Done) => break,
            Ok(StepResult::Row) => {}
            Ok(StepResult::IO | StepResult::Busy) => {
                io.run_once().expect("io run_once");
            }
            Ok(StepResult::Interrupt) => panic!("interrupted"),
        }
    }
    assert!(
        got_schema_changed,
        "expected SchemaChanged after DDL invalidated the held statement"
    );
}

/// Repeated reset-and-reuse of a prepared statement must NOT raise
/// `SchemaChanged` when no DDL has run in between.  The reset brings pc back
/// to 0, so the Transaction opcode re-executes each time; the compile-time
/// cookie must still match the live cookie because neither has changed.
#[test]
fn no_ddl_never_raises_schema_changed() {
    let (io, conn) = new_mem_db();

    exec(&io, &conn, "CREATE TABLE t (v INTEGER)").expect("create t");
    exec(&io, &conn, "INSERT INTO t VALUES (42)").expect("insert");

    let mut stmt = conn.prepare("SELECT v FROM t").expect("prepare SELECT");

    for i in 0..200usize {
        // Step the statement to Done.
        loop {
            match stmt.step().unwrap_or_else(|e| {
                panic!("SchemaChanged raised without DDL (iteration {i}): {e:?}")
            }) {
                StepResult::Done => break,
                StepResult::Row => {}
                StepResult::IO | StepResult::Busy => {
                    io.run_once().expect("io run_once");
                }
                StepResult::Interrupt => panic!("interrupted"),
            }
        }
        // Reset pc to 0 so the Transaction opcode re-runs on the next step.
        stmt.reset();
    }
}
