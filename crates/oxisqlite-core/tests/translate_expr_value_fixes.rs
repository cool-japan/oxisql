//! Integration tests for the `translate/expr.rs` fixes covering:
//!   - `IN (...)` / `NOT IN (...)` used as a *value* (SELECT list, CASE WHEN, function
//!     argument) rather than only as a top-level WHERE/HAVING/JOIN-ON condition. This
//!     previously called `todo!()` in `translate_expr`'s `InList` arm and crashed the
//!     process; `translate_condition_expr` already had its own, separate, jump-based
//!     `InList` implementation used only for top-level conditions.
//!   - `schema.table.column` (`DoublyQualified`) references bound against the implicit
//!     "main" schema, with a clean error for any other schema name.
//!   - The parenthesized multi-expression guard (`(a, b)` used as a value) failing with a
//!     clean parse error instead of `todo!()`.
//!   - The `MATCH` operator failing with a clean parse error instead of `todo!()`.
//!
//! SQL three-valued (0/1/NULL) logic for `IN`/`NOT IN`:
//!   x IN  (set): 1 if a definite match; NULL if no definite match but the set contains
//!                NULL; else 0.
//!   x NOT IN (set): 0 if a definite match; NULL if no definite match but the set contains
//!                   NULL; else 1.
//!   NULL IN/NOT IN (any non-empty set): always NULL.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_expr_value_fixes_{}_{}_{}.db",
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

fn open(tag: &str) -> (Arc<dyn limbo_core::IO>, Arc<Connection>, std::path::PathBuf) {
    let path = temp_db_path(tag);
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();
    (io, conn, path)
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Step a single-column query and collect every row's value of column 0.
fn read_values(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> Vec<Value> {
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
                let row = stmt.row().expect("row available after StepResult::Row");
                out.push(row.get_value(0).clone());
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

/// Step a single-column, single-row query and return its value.
fn read_value(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> Value {
    let mut values = read_values(io, conn, sql);
    assert_eq!(values.len(), 1, "expected exactly one row for {sql}");
    values.pop().expect("checked len == 1 above")
}

// ---------------------------------------------------------------------------------------------
// `IN (...)` / `NOT IN (...)` as a value.
// ---------------------------------------------------------------------------------------------

/// `SELECT x IN (1,2,3) FROM t`: previously crashed (todo!()); now produces 0/1 per row.
#[test]
fn in_list_as_select_value_true_and_false() {
    let (io, conn, path) = open("select_value");
    exec(&conn, "CREATE TABLE t (x INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (2)");
    exec(&conn, "INSERT INTO t VALUES (5)");

    let values = read_values(&io, &conn, "SELECT x IN (1, 2, 3) FROM t ORDER BY x");
    assert_eq!(
        values,
        vec![Value::Integer(1), Value::Integer(0)],
        "x IN (1,2,3) should be 1 for x=2 and 0 for x=5"
    );
    cleanup(&path);
}

/// `SELECT x NOT IN (...) FROM t` as a value.
#[test]
fn in_list_not_in_as_select_value() {
    let (io, conn, path) = open("select_value_not_in");
    exec(&conn, "CREATE TABLE t (x INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (2)");
    exec(&conn, "INSERT INTO t VALUES (5)");

    let values = read_values(&io, &conn, "SELECT x NOT IN (1, 2, 3) FROM t ORDER BY x");
    assert_eq!(
        values,
        vec![Value::Integer(0), Value::Integer(1)],
        "x NOT IN (1,2,3) should be 0 for x=2 and 1 for x=5"
    );
    cleanup(&path);
}

/// `CASE WHEN x IN (1,2) THEN 'a' ELSE 'b' END`: IN used as a value nested inside CASE.
#[test]
fn in_list_inside_case_when() {
    let (io, conn, path) = open("case_when");
    exec(&conn, "CREATE TABLE t (x INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (1)");
    exec(&conn, "INSERT INTO t VALUES (2)");
    exec(&conn, "INSERT INTO t VALUES (99)");

    let values = read_values(
        &io,
        &conn,
        "SELECT CASE WHEN x IN (1, 2) THEN 'a' ELSE 'b' END FROM t ORDER BY x",
    );
    assert_eq!(
        values,
        vec![
            Value::build_text("a"),
            Value::build_text("a"),
            Value::build_text("b"),
        ]
    );
    cleanup(&path);
}

/// `IN (...)` used as a function argument: another value-context (not top-level condition).
#[test]
fn in_list_as_function_argument() {
    let (io, conn, path) = open("function_arg");
    exec(&conn, "CREATE TABLE t (x INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (1)");
    exec(&conn, "INSERT INTO t VALUES (5)");

    let values = read_values(&io, &conn, "SELECT abs(x IN (1, 2)) FROM t ORDER BY x");
    assert_eq!(
        values,
        vec![Value::Integer(1), Value::Integer(0)],
        "abs(x IN (1,2)) should still see the correct 0/1 produced by the IN value"
    );
    cleanup(&path);
}

/// `SELECT NULL IN (1,2,3)`: the LHS is NULL, so the result must be NULL (not 0).
#[test]
fn null_in_list_is_null() {
    let (io, conn, path) = open("null_in_list");
    let v = read_value(&io, &conn, "SELECT NULL IN (1, 2, 3)");
    assert_eq!(v, Value::Null, "NULL IN (1,2,3) must be NULL, got {v:?}");
    cleanup(&path);
}

/// `SELECT 1 IN (1,NULL,3)`: a definite match against `1` wins even though NULL is also
/// present in the list -- must be 1, not NULL.
#[test]
fn value_matches_despite_null_in_list_is_true() {
    let (io, conn, path) = open("matches_with_null");
    let v = read_value(&io, &conn, "SELECT 1 IN (1, NULL, 3)");
    assert_eq!(
        v,
        Value::Integer(1),
        "1 IN (1,NULL,3) must be 1 (a definite match wins over an unrelated NULL), got {v:?}"
    );
    cleanup(&path);
}

/// `SELECT 2 IN (1,NULL,3)`: no definite match, but NULL is present, so the result is
/// unknown (NULL), not definitely false (0).
#[test]
fn no_match_with_null_in_list_is_null_not_false() {
    let (io, conn, path) = open("no_match_with_null");
    let v = read_value(&io, &conn, "SELECT 2 IN (1, NULL, 3)");
    assert_eq!(
        v,
        Value::Null,
        "2 IN (1,NULL,3) must be NULL (no definite match, but NULL makes it unknown), got {v:?}"
    );
    cleanup(&path);
}

/// `SELECT 4 IN (1,2,3)`: no NULLs anywhere and no match -- must be definitely 0.
#[test]
fn no_match_no_null_in_list_is_definitely_false() {
    let (io, conn, path) = open("no_match_no_null");
    let v = read_value(&io, &conn, "SELECT 4 IN (1, 2, 3)");
    assert_eq!(v, Value::Integer(0));
    cleanup(&path);
}

/// `NOT IN` three-valued semantics as a value, mirroring the `IN` cases above.
#[test]
fn not_in_list_null_semantics() {
    let (io, conn, path) = open("not_in_null_semantics");
    assert_eq!(
        read_value(&io, &conn, "SELECT NULL NOT IN (1, 2, 3)"),
        Value::Null,
        "NULL NOT IN (...) must always be NULL"
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 1 NOT IN (1, NULL, 3)"),
        Value::Integer(0),
        "1 NOT IN (1,NULL,3): a definite equal match makes NOT IN definitely false"
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 2 NOT IN (1, NULL, 3)"),
        Value::Null,
        "2 NOT IN (1,NULL,3): NULL in the list means \"not equal to everything\" can't be proven"
    );
    cleanup(&path);
}

// ---------------------------------------------------------------------------------------------
// `DoublyQualified` (`schema.table.column`).
// ---------------------------------------------------------------------------------------------

/// `main.t.x`: the only valid 3-part reference in a single-schema (no ATTACH) engine.
#[test]
fn doubly_qualified_main_schema_reference() {
    let (io, conn, path) = open("doubly_qualified_ok");
    exec(&conn, "CREATE TABLE t (x INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (42)");

    let v = read_value(&io, &conn, "SELECT main.t.x FROM t");
    assert_eq!(v, Value::Integer(42));
    cleanup(&path);
}

/// A non-"main" schema qualifier must be a clean parse error (no ATTACH support), not a panic.
#[test]
fn doubly_qualified_unknown_schema_errors_cleanly() {
    let (_io, conn, path) = open("doubly_qualified_bad_schema");
    exec(&conn, "CREATE TABLE t (x INTEGER)");

    let result = conn.query("SELECT nope.t.x FROM t");
    assert!(
        result.is_err(),
        "a non-'main' schema qualifier must be a clean parse error, not a panic; got {:?}",
        result.err()
    );
    cleanup(&path);
}

// ---------------------------------------------------------------------------------------------
// Parenthesized multi-expression value and MATCH operator.
// ---------------------------------------------------------------------------------------------

/// `(a, b)` used as a value (row-value syntax) is not implemented; must be a clean parse
/// error, not a panic.
#[test]
fn parenthesized_multi_expression_errors_cleanly() {
    let (_io, conn, path) = open("parenthesized_multi");
    exec(&conn, "CREATE TABLE t (a INTEGER, b INTEGER)");

    let result = conn.query("SELECT (a, b) FROM t");
    assert!(
        result.is_err(),
        "a multi-expression parenthesized value should be a clean parse error, not a panic; got {:?}",
        result.err()
    );
    cleanup(&path);
}

/// `MATCH` has no supporting virtual table module in this engine; must be a clean parse
/// error (matching real SQLite's own behavior for the same case), not a panic.
#[test]
fn match_operator_errors_cleanly() {
    let (_io, conn, path) = open("match_operator");
    exec(&conn, "CREATE TABLE t (x TEXT)");

    let result = conn.query("SELECT * FROM t WHERE x MATCH 'foo'");
    assert!(
        result.is_err(),
        "MATCH with no supporting virtual table module must be a clean parse error, not a panic; got {:?}",
        result.err()
    );
    cleanup(&path);
}
