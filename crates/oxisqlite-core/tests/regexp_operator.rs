//! Integration tests for the `REGEXP` operator (and its `regexp(pattern, text)`
//! function form).
//!
//! SQLite defines `X REGEXP Y` as `regexp(Y, X)`: the pattern is the right-hand
//! operand and the subject is the left-hand operand. Matching is an unanchored
//! search (the pattern may match anywhere within the subject). A NULL operand
//! yields NULL, and a malformed pattern raises an error rather than panicking.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_regexp_{}_{}_{}.db",
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

fn read_value(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> Value {
    let mut values = read_values(io, conn, sql);
    assert_eq!(values.len(), 1, "expected exactly one row for {sql}");
    values.pop().expect("checked len == 1 above")
}

#[test]
fn regexp_operator_basic_match_and_nonmatch() {
    let (io, conn, path) = open("basic");
    assert_eq!(
        read_value(&io, &conn, "SELECT 'abc' REGEXP 'b'"),
        Value::Integer(1),
        "'b' is found inside 'abc'"
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'abc' REGEXP 'z'"),
        Value::Integer(0),
        "'z' is not found inside 'abc'"
    );
    cleanup(&path);
}

#[test]
fn regexp_operator_is_unanchored() {
    let (io, conn, path) = open("unanchored");
    // Unlike LIKE/GLOB, REGEXP matches anywhere in the string unless anchored.
    assert_eq!(
        read_value(&io, &conn, "SELECT 'hello world' REGEXP 'wor'"),
        Value::Integer(1)
    );
    // Anchors work.
    assert_eq!(
        read_value(&io, &conn, "SELECT 'hello world' REGEXP '^world'"),
        Value::Integer(0)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'hello world' REGEXP '^hello'"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'hello world' REGEXP 'world$'"),
        Value::Integer(1)
    );
    cleanup(&path);
}

#[test]
fn regexp_operator_metacharacters() {
    let (io, conn, path) = open("meta");
    assert_eq!(
        read_value(&io, &conn, "SELECT 'cat' REGEXP 'c.t'"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'ct' REGEXP 'c.t'"),
        Value::Integer(0)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'aaa' REGEXP 'a+'"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'foo123bar' REGEXP '[0-9]+'"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'foobar' REGEXP '[0-9]+'"),
        Value::Integer(0)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'gray' REGEXP 'gr(a|e)y'"),
        Value::Integer(1)
    );
    cleanup(&path);
}

#[test]
fn regexp_operator_null_operands_yield_null() {
    let (io, conn, path) = open("null");
    assert_eq!(
        read_value(&io, &conn, "SELECT NULL REGEXP 'a'"),
        Value::Null
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 'a' REGEXP NULL"),
        Value::Null
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT NULL REGEXP NULL"),
        Value::Null
    );
    cleanup(&path);
}

#[test]
fn regexp_operator_in_where_clause() {
    let (io, conn, path) = open("where");
    exec(&conn, "CREATE TABLE t (id INTEGER, name TEXT)");
    exec(&conn, "INSERT INTO t VALUES (1, 'alice')");
    exec(&conn, "INSERT INTO t VALUES (2, 'bob')");
    exec(&conn, "INSERT INTO t VALUES (3, 'carol')");
    exec(&conn, "INSERT INTO t VALUES (4, 'dave')");

    // Names containing a vowel-then-'l'.
    let ids = read_values(
        &io,
        &conn,
        "SELECT id FROM t WHERE name REGEXP 'a.*l' ORDER BY id",
    );
    assert_eq!(ids, vec![Value::Integer(1), Value::Integer(3)]);
    cleanup(&path);
}

#[test]
fn regexp_operator_non_text_operand_is_coerced() {
    let (io, conn, path) = open("coerce");
    // Integers are cast to their text form for matching.
    assert_eq!(
        read_value(&io, &conn, "SELECT 12345 REGEXP '234'"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT 12345 REGEXP '999'"),
        Value::Integer(0)
    );
    cleanup(&path);
}

#[test]
fn regexp_function_form_matches_operator() {
    let (io, conn, path) = open("func_form");
    // `regexp(pattern, text)` is the function form of `text REGEXP pattern`.
    assert_eq!(
        read_value(&io, &conn, "SELECT regexp('b', 'abc')"),
        Value::Integer(1)
    );
    assert_eq!(
        read_value(&io, &conn, "SELECT regexp('z', 'abc')"),
        Value::Integer(0)
    );
    cleanup(&path);
}

#[test]
fn regexp_operator_invalid_pattern_errors_without_panic() {
    let (_io, conn, path) = open("invalid");
    // An unclosed group is a malformed regex: this must surface as an error,
    // never a panic.
    let result = conn.query("SELECT 'abc' REGEXP '('");
    match result {
        Err(_) => { /* rejected at prepare time */ }
        Ok(Some(mut stmt)) => {
            // Or surfaced during execution.
            let mut errored = false;
            loop {
                match stmt.step() {
                    Ok(StepResult::Row) | Ok(StepResult::Done) => break,
                    Ok(StepResult::IO) => {
                        if _io.run_once().is_err() {
                            errored = true;
                            break;
                        }
                    }
                    Ok(_) => break,
                    Err(_) => {
                        errored = true;
                        break;
                    }
                }
            }
            assert!(errored, "malformed REGEXP pattern must produce an error");
        }
        Ok(None) => panic!("expected a statement"),
    }
    cleanup(&path);
}
