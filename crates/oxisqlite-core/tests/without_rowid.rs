/// Integration tests for WITHOUT ROWID table support.
///
/// WITHOUT ROWID tables use an index-format B-Tree where the PRIMARY KEY
/// columns form the key and the full row is the stored record payload.
///
/// Constraint enforced by this engine: PK column(s) must be declared first
/// in the column list (validated at CREATE TABLE time).
///
/// Tests cover:
///   - CREATE TABLE ... WITHOUT ROWID succeeds / fails correctly
///   - Basic INSERT and SELECT round-trip
///   - PK NOT NULL enforcement
///   - PK uniqueness enforcement (ABORT / IGNORE / REPLACE)
///   - Multi-row INSERT (VALUES with multiple rows)
///   - Text and composite PK variants
///   - Validation: missing PK → error
///   - Validation: PK column not first → error
use limbo_core::{Database, MemoryIO, StepResult, Value};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory:");
    let conn = db.connect().expect("connect");
    (io, conn)
}

/// Execute a statement, draining IO until Done.  Panics on any error.
fn exec(io: &Arc<dyn limbo_core::IO>, conn: &Arc<limbo_core::Connection>, sql: &str) {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {:?}: {:?}", sql, e));
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {:?}: {:?}", sql, e))
        {
            StepResult::Done => return,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {}
            StepResult::Interrupt => panic!("interrupted in exec"),
        }
    }
}

/// Execute a statement and expect an error (returns the error message).
fn exec_expect_err(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> String {
    match conn.prepare(sql) {
        Err(e) => format!("{:?}", e),
        Ok(mut stmt) => loop {
            match stmt.step() {
                Err(e) => return format!("{:?}", e),
                Ok(StepResult::Done) => panic!("expected error but got Done for: {:?}", sql),
                Ok(StepResult::IO | StepResult::Busy) => {
                    io.run_once().expect("io run_once");
                }
                Ok(StepResult::Row) => {}
                Ok(StepResult::Interrupt) => panic!("interrupted"),
            }
        },
    }
}

/// Collect all rows for a SELECT query, returning i64 values from column 0.
fn query_col_ints(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<i64> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {:?}: {:?}", sql, e));
    let mut out = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {:?}: {:?}", sql, e))
        {
            StepResult::Done => break,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {
                if let Value::Integer(v) = stmt.row().expect("row").get_value(0) {
                    out.push(*v);
                }
            }
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    out
}

/// Collect all rows for a SELECT query, returning text values from column 0.
fn query_col_texts(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<String> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {:?}: {:?}", sql, e));
    let mut out = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {:?}: {:?}", sql, e))
        {
            StepResult::Done => break,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {
                if let Value::Text(t) = stmt.row().expect("row").get_value(0) {
                    out.push(String::from_utf8_lossy(&t.value).into_owned());
                }
            }
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    out
}

fn count_rows(io: &Arc<dyn limbo_core::IO>, conn: &Arc<limbo_core::Connection>, tbl: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {}", tbl);
    let mut stmt = conn.prepare(&sql).expect("prepare count");
    loop {
        match stmt.step().expect("step count") {
            StepResult::Done => return 0,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {
                if let Value::Integer(n) = stmt.row().expect("row").get_value(0) {
                    return *n;
                }
            }
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests: CREATE TABLE
// ---------------------------------------------------------------------------

/// A simple single-column INTEGER PK WITHOUT ROWID table can be created.
#[test]
fn without_rowid_create_table_integer_pk_first_col() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    // Table should exist in sqlite_schema.
    let names = query_col_texts(
        &io,
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='table'",
    );
    assert!(names.iter().any(|n| n == "t"), "table 't' not in schema");
}

/// A WITHOUT ROWID table with a text PK (first column) can be created.
#[test]
fn without_rowid_create_table_text_pk_first_col() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (code TEXT PRIMARY KEY, name TEXT) WITHOUT ROWID",
    );
    let names = query_col_texts(
        &io,
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='table'",
    );
    assert!(names.iter().any(|n| n == "t"));
}

/// A WITHOUT ROWID table with composite PK (first two columns) can be created.
#[test]
fn without_rowid_create_table_composite_pk_first_cols() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (a INTEGER, b INTEGER, val TEXT, PRIMARY KEY(a, b)) WITHOUT ROWID",
    );
    let names = query_col_texts(
        &io,
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='table'",
    );
    assert!(names.iter().any(|n| n == "t"));
}

/// WITHOUT ROWID with no PRIMARY KEY must be rejected.
#[test]
fn without_rowid_no_pk_is_error() {
    let (io, conn) = new_mem_db();
    let err = exec_expect_err(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER, val TEXT) WITHOUT ROWID",
    );
    assert!(
        err.to_lowercase().contains("primary key"),
        "expected primary key error, got: {}",
        err
    );
}

/// WITHOUT ROWID where PK column is NOT first must be rejected.
#[test]
fn without_rowid_pk_not_first_col_is_error() {
    let (io, conn) = new_mem_db();
    let err = exec_expect_err(
        &io,
        &conn,
        "CREATE TABLE t (val TEXT, id INTEGER PRIMARY KEY) WITHOUT ROWID",
    );
    assert!(
        err.to_lowercase().contains("first"),
        "expected 'first' position error, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Tests: INSERT and SELECT
// ---------------------------------------------------------------------------

/// Basic INSERT then SELECT on a WITHOUT ROWID table returns the inserted rows.
#[test]
fn without_rowid_insert_and_select() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'alpha')");
    exec(&io, &conn, "INSERT INTO t VALUES (2, 'beta')");
    exec(&io, &conn, "INSERT INTO t VALUES (3, 'gamma')");

    let ids = query_col_ints(&io, &conn, "SELECT id FROM t ORDER BY id");
    assert_eq!(ids, vec![1, 2, 3]);

    let vals = query_col_texts(&io, &conn, "SELECT val FROM t ORDER BY id");
    assert_eq!(vals, vec!["alpha", "beta", "gamma"]);
}

/// Counting rows in a WITHOUT ROWID table works correctly.
#[test]
fn without_rowid_count_rows() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (10, 100)");
    exec(&io, &conn, "INSERT INTO t VALUES (20, 200)");
    assert_eq!(count_rows(&io, &conn, "t"), 2);
}

/// A WITHOUT ROWID table with text PK supports INSERT and SELECT.
#[test]
fn without_rowid_text_pk_insert_select() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (code TEXT PRIMARY KEY, description TEXT) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES ('X1', 'first')");
    exec(&io, &conn, "INSERT INTO t VALUES ('X2', 'second')");

    let codes = query_col_texts(&io, &conn, "SELECT code FROM t ORDER BY code");
    assert_eq!(codes, vec!["X1", "X2"]);
}

// ---------------------------------------------------------------------------
// Tests: PK NOT NULL enforcement
// ---------------------------------------------------------------------------

/// Inserting NULL into a WITHOUT ROWID PK column must fail.
#[test]
fn without_rowid_null_pk_is_error() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    let err = exec_expect_err(&io, &conn, "INSERT INTO t VALUES (NULL, 'a')");
    assert!(
        err.to_lowercase().contains("not null") || err.to_lowercase().contains("notnull"),
        "expected NOT NULL error, got: {}",
        err
    );
}

// ---------------------------------------------------------------------------
// Tests: PK uniqueness
// ---------------------------------------------------------------------------

/// Inserting a duplicate PK into a WITHOUT ROWID table must fail (ABORT / default).
#[test]
fn without_rowid_duplicate_pk_is_error() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'first')");
    let err = exec_expect_err(&io, &conn, "INSERT INTO t VALUES (1, 'duplicate')");
    assert!(
        err.to_lowercase().contains("constraint")
            || err.to_lowercase().contains("unique")
            || err.to_lowercase().contains("primary"),
        "expected constraint error, got: {}",
        err
    );
    // Original row must still be there.
    let vals = query_col_texts(&io, &conn, "SELECT val FROM t WHERE id=1");
    assert_eq!(vals, vec!["first"]);
}

/// INSERT OR IGNORE skips a duplicate PK without error.
#[test]
fn without_rowid_insert_or_ignore_duplicate_pk() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(&io, &conn, "INSERT OR IGNORE INTO t VALUES (1, 'ignored')");
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    let vals = query_col_texts(&io, &conn, "SELECT val FROM t WHERE id=1");
    assert_eq!(vals, vec!["original"]);
}

/// INSERT OR REPLACE on a duplicate PK deletes the old row and inserts the new one.
#[test]
fn without_rowid_insert_or_replace_duplicate_pk() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT OR REPLACE INTO t VALUES (1, 'replaced')",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    let vals = query_col_texts(&io, &conn, "SELECT val FROM t WHERE id=1");
    assert_eq!(vals, vec!["replaced"]);
}

// ---------------------------------------------------------------------------
// Tests: Named-column INSERT
// ---------------------------------------------------------------------------

/// INSERT with explicit column names works on a WITHOUT ROWID table.
#[test]
fn without_rowid_insert_named_columns() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, a TEXT, b INTEGER) WITHOUT ROWID",
    );
    exec(
        &io,
        &conn,
        "INSERT INTO t (id, b, a) VALUES (5, 99, 'hello')",
    );

    let ids = query_col_ints(&io, &conn, "SELECT id FROM t");
    assert_eq!(ids, vec![5]);
    let vals = query_col_texts(&io, &conn, "SELECT a FROM t");
    assert_eq!(vals, vec!["hello"]);
}

// ---------------------------------------------------------------------------
// Tests: Composite PK
// ---------------------------------------------------------------------------

/// A WITHOUT ROWID table with a composite PK supports INSERT and SELECT.
#[test]
fn without_rowid_composite_pk_insert_select() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE edge (src INTEGER, dst INTEGER, weight REAL, PRIMARY KEY(src, dst)) WITHOUT ROWID",
    );
    exec(&io, &conn, "INSERT INTO edge VALUES (1, 2, 0.5)");
    exec(&io, &conn, "INSERT INTO edge VALUES (1, 3, 1.0)");
    exec(&io, &conn, "INSERT INTO edge VALUES (2, 3, 2.0)");

    assert_eq!(count_rows(&io, &conn, "edge"), 3);

    let srcs = query_col_ints(&io, &conn, "SELECT src FROM edge ORDER BY src, dst");
    assert_eq!(srcs, vec![1, 1, 2]);
}
