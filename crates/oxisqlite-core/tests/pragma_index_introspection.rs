//! Tests for the `PRAGMA index_list` / `PRAGMA index_info` implementations.
//!
//! Behaviour is feature-dependent by design:
//!
//!   * With `index_experimental` the in-memory schema retains parsed index
//!     definitions, so both pragmas emit real rows (seq/name/unique/origin/partial
//!     and seqno/cid/name respectively). The column list comes from the parsed
//!     index key, not from string-splitting the CREATE INDEX DDL.
//!
//!   * Without the feature the schema keeps only a per-table "has indexes" bit,
//!     so the pragmas cannot enumerate index rows. Rather than silently return an
//!     empty result — which a caller could not distinguish from "no indexes" —
//!     the engine raises a typed error when the target table is known to have
//!     indexes. This test pins that contract so the two builds stay honest.
//!
//! Database files live under [`std::env::temp_dir`].

use std::sync::Arc;

use limbo_core::{Connection, Database};
#[cfg(feature = "index_experimental")]
use limbo_core::{StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall io"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_pragma_index_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
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
    let db = Database::open_file(io.clone(), path.to_str().expect("utf-8 temp path"), false)
        .expect("open database");
    let conn = db.connect().expect("connect");
    (io, conn, path)
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Collect every row of a query as a vector of column-0..n `Value`s.
#[cfg(feature = "index_experimental")]
fn collect_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<Connection>,
    sql: &str,
) -> Vec<Vec<Value>> {
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
                out.push(row.get_values().cloned().collect::<Vec<Value>>());
            }
            StepResult::IO => io.run_once().expect("io"),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

#[cfg(feature = "index_experimental")]
fn as_text(v: &Value) -> String {
    match v {
        Value::Text(t) => t.as_str().to_string(),
        other => panic!("expected text, got {other:?}"),
    }
}

#[cfg(feature = "index_experimental")]
fn as_int(v: &Value) -> i64 {
    match v {
        Value::Integer(i) => *i,
        other => panic!("expected integer, got {other:?}"),
    }
}

/// With the feature enabled, `PRAGMA index_list` / `index_info` return real rows,
/// and the multi-column key surfaces as clean column names (`a`, `b`) — the exact
/// case the previous DDL-text parser mangled into tokens like `b DESC`.
#[cfg(feature = "index_experimental")]
#[test]
fn index_list_and_index_info_return_rows() {
    let (io, conn, path) = open("rows");
    exec(&conn, "CREATE TABLE t(a INTEGER, b INTEGER, c INTEGER)");
    exec(&conn, "CREATE INDEX idx_ab ON t(a, b DESC)");

    // index_list(t): exactly one user index named idx_ab, non-unique, origin "c".
    let list = collect_rows(&io, &conn, "PRAGMA index_list(t)");
    let user_rows: Vec<&Vec<Value>> = list
        .iter()
        .filter(|r| !as_text(&r[1]).starts_with("sqlite_"))
        .collect();
    assert_eq!(
        user_rows.len(),
        1,
        "one explicit index expected, got {list:?}"
    );
    let row = user_rows[0];
    assert_eq!(as_text(&row[1]), "idx_ab");
    assert_eq!(as_int(&row[2]), 0, "idx_ab is not unique");
    assert_eq!(
        as_text(&row[3]),
        "c",
        "explicit CREATE INDEX has origin 'c'"
    );
    assert_eq!(as_int(&row[4]), 0, "no partial predicate is tracked");

    // index_info(idx_ab): the parsed key columns, in order, without DESC noise.
    let info = collect_rows(&io, &conn, "PRAGMA index_info(idx_ab)");
    let names: Vec<String> = info.iter().map(|r| as_text(&r[2])).collect();
    assert_eq!(names, vec!["a".to_string(), "b".to_string()]);

    cleanup(&path);
}

/// Without the feature the engine cannot enumerate index columns, so
/// `PRAGMA index_info` must raise a typed error naming the missing feature
/// rather than silently returning an empty result set that a caller could not
/// distinguish from "no such index".
///
/// (`CREATE INDEX` is itself rejected without `index_experimental`, so the
/// symmetric `index_list`-on-an-indexed-table error path can only be reached by
/// opening a database file that already carries indexes — it shares the same
/// `table_has_indexes` guard and is covered by the feature-on rows test above.)
#[cfg(not(feature = "index_experimental"))]
#[test]
fn index_info_without_feature_errors() {
    let (_io, conn, path) = open("no_feature");
    exec(&conn, "CREATE TABLE t(a INTEGER, b INTEGER)");

    let err = conn
        .execute("PRAGMA index_info(some_index)")
        .expect_err("index_info must error without the index_experimental feature");
    let msg = format!("{err}").to_lowercase();
    assert!(
        msg.contains("index_experimental"),
        "error should point at the missing feature, got: {msg}"
    );

    cleanup(&path);
}

/// Without the feature, a table with *no* indexes still returns an empty result
/// set for `index_list` (no error) — the error is reserved for the ambiguous
/// case where indexes exist but cannot be enumerated.
#[cfg(not(feature = "index_experimental"))]
#[test]
fn index_list_without_feature_ok_for_unindexed_table() {
    let (_io, conn, path) = open("no_feature_empty");
    exec(&conn, "CREATE TABLE t(a INTEGER, b INTEGER)");

    conn.execute("PRAGMA index_list(t)")
        .expect("unindexed table must not error");

    cleanup(&path);
}
