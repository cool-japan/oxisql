//! Integration tests for `CREATE TRIGGER` / `DROP TRIGGER` and row-trigger
//! execution (Q11a).
//!
//! Behaviour is checked against upstream SQLite semantics:
//!
//! * triggers persist in `sqlite_schema` and survive a close/reopen cycle;
//! * `BEFORE`/`AFTER` × `INSERT`/`UPDATE`/`DELETE` all fire, per row;
//! * `WHEN` guards, `OLD.*` / `NEW.*` (including `rowid`), and `UPDATE OF (c)`
//!   column filtering all behave as specified;
//! * `RAISE(ABORT)` aborts the statement with the trigger's own message and
//!   `RAISE(IGNORE)` skips just the offending row while the statement continues;
//! * `DROP TRIGGER` and `DROP TABLE` both remove the catalog entry and the
//!   `sqlite_schema` row;
//! * rows written by a trigger body do not count towards `changes()`;
//! * a trigger cannot re-enter itself (`recursive_triggers` is off, as upstream
//!   defaults).

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall IO backend"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_trigger_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

struct Db {
    io: Arc<dyn limbo_core::IO>,
    conn: Arc<Connection>,
    path: std::path::PathBuf,
}

fn open(tag: &str) -> Db {
    let path = temp_db_path(tag);
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(
        io.clone(),
        path.to_str().expect("temp path is valid UTF-8"),
        false,
    )
    .expect("open database");
    let conn = db.connect().expect("connect");
    Db { io, conn, path }
}

/// Close the current connection and reopen the same file, so that the schema is
/// rebuilt from `sqlite_schema` rather than from in-memory state.
fn reopen(db: Db) -> Db {
    let Db { io, conn, path } = db;
    conn.close().expect("close connection");
    drop(conn);
    let db = Database::open_file(
        io.clone(),
        path.to_str().expect("temp path is valid UTF-8"),
        false,
    )
    .expect("reopen database");
    let conn = db.connect().expect("reconnect");
    Db { io, conn, path }
}

fn exec(db: &Db, sql: &str) {
    db.conn
        .execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn exec_err(db: &Db, sql: &str) -> String {
    match db.conn.execute(sql) {
        Ok(()) => panic!("expected an error from {sql}, but it succeeded"),
        Err(e) => format!("{e}"),
    }
}

fn read_rows(db: &Db, sql: &str) -> Vec<Vec<Value>> {
    let mut stmt = db
        .conn
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
                out.push(row.get_values().cloned().collect::<Vec<_>>());
            }
            StepResult::IO => db.io.run_once().expect("io"),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

fn read_col(db: &Db, sql: &str) -> Vec<Value> {
    read_rows(db, sql)
        .into_iter()
        .map(|mut r| r.drain(..).next().expect("at least one column"))
        .collect()
}

fn read_one(db: &Db, sql: &str) -> Value {
    let mut values = read_col(db, sql);
    assert_eq!(values.len(), 1, "expected exactly one row for {sql}");
    values.pop().expect("checked len == 1 above")
}

fn ints(values: &[Value]) -> Vec<i64> {
    values
        .iter()
        .map(|v| match v {
            Value::Integer(i) => *i,
            other => panic!("expected Integer, got {other:?}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Catalog: create / persist / reload / drop
// ---------------------------------------------------------------------------

#[test]
fn create_trigger_persists_in_sqlite_schema() {
    let db = open("persist");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );

    let rows = read_rows(
        &db,
        "SELECT type, name, tbl_name, rootpage FROM sqlite_master WHERE type = 'trigger'",
    );
    assert_eq!(rows.len(), 1, "exactly one trigger row expected");
    assert_eq!(rows[0][0], Value::build_text("trigger"));
    assert_eq!(rows[0][1], Value::build_text("t_ai"));
    assert_eq!(rows[0][2], Value::build_text("t"));
    assert_eq!(
        rows[0][3],
        Value::Integer(0),
        "a trigger owns no b-tree, so rootpage is 0"
    );
    cleanup(&db.path);
}

#[test]
fn trigger_survives_close_and_reopen_and_still_fires() {
    // The full round trip: persisted to sqlite_schema, re-parsed on open, then
    // fired. A stub would pass the "row exists" half and fail this.
    let db = open("roundtrip");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a * 10); END",
    );

    let db = reopen(db);
    exec(&db, "INSERT INTO t VALUES (7)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log")),
        vec![70],
        "the reloaded trigger fired on the reopened connection"
    );
    cleanup(&db.path);
}

#[test]
fn drop_trigger_removes_row_and_stops_firing() {
    let db = open("drop");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");
    exec(&db, "DROP TRIGGER t_ai");
    exec(&db, "INSERT INTO t VALUES (2)");

    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log")),
        vec![1],
        "only the pre-DROP insert was logged"
    );
    assert_eq!(
        read_rows(&db, "SELECT name FROM sqlite_master WHERE type = 'trigger'").len(),
        0,
        "the sqlite_schema row is gone too"
    );

    // ...and stays gone across a reopen.
    let db = reopen(db);
    exec(&db, "INSERT INTO t VALUES (3)");
    assert_eq!(ints(&read_col(&db, "SELECT a FROM log")), vec![1]);
    cleanup(&db.path);
}

#[test]
fn drop_trigger_errors_unless_if_exists() {
    let db = open("drop_missing");
    exec(&db, "CREATE TABLE t (a)");
    let err = exec_err(&db, "DROP TRIGGER nope");
    assert!(
        err.contains("no such trigger"),
        "unexpected error text: {err}"
    );
    exec(&db, "DROP TRIGGER IF EXISTS nope");
    cleanup(&db.path);
}

#[test]
fn create_trigger_rejects_duplicate_unless_if_not_exists() {
    let db = open("dup");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    let err = exec_err(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    assert!(err.contains("already exists"), "unexpected error: {err}");
    exec(
        &db,
        "CREATE TRIGGER IF NOT EXISTS t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    cleanup(&db.path);
}

#[test]
fn create_trigger_rejects_unknown_table_and_unknown_column() {
    let db = open("validate");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");

    let err = exec_err(
        &db,
        "CREATE TRIGGER x AFTER INSERT ON nosuch BEGIN INSERT INTO log VALUES (1); END",
    );
    assert!(err.contains("no such table"), "unexpected error: {err}");

    // A bad NEW reference is caught at CREATE time, as upstream does.
    let err = exec_err(
        &db,
        "CREATE TRIGGER x AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.nope); END",
    );
    assert!(err.contains("no such column"), "unexpected error: {err}");

    // OLD is not in scope for an INSERT trigger.
    let err = exec_err(
        &db,
        "CREATE TRIGGER x AFTER INSERT ON t BEGIN INSERT INTO log VALUES (OLD.a); END",
    );
    assert!(err.contains("no such column"), "unexpected error: {err}");

    // ...nor NEW for a DELETE trigger.
    let err = exec_err(
        &db,
        "CREATE TRIGGER x AFTER DELETE ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    assert!(err.contains("no such column"), "unexpected error: {err}");
    cleanup(&db.path);
}

#[test]
fn drop_table_drops_its_triggers() {
    let db = open("drop_table");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "DROP TABLE t");
    assert_eq!(
        read_rows(&db, "SELECT name FROM sqlite_master WHERE type = 'trigger'").len(),
        0,
        "DROP TABLE removes the table's trigger rows"
    );

    // Re-creating the table must not resurrect the trigger.
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (5)");
    assert_eq!(read_col(&db, "SELECT a FROM log").len(), 0);
    cleanup(&db.path);
}

// ---------------------------------------------------------------------------
// Firing: INSERT / UPDATE / DELETE × BEFORE / AFTER
// ---------------------------------------------------------------------------

#[test]
fn after_insert_trigger_fires_per_row() {
    let db = open("ai");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (2), (3)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log ORDER BY a")),
        vec![1, 2, 3],
        "a row trigger fires once per inserted row"
    );
    cleanup(&db.path);
}

#[test]
fn before_insert_trigger_fires_before_the_row_is_visible() {
    let db = open("bi");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (n)");
    // Count rows already in `t` at trigger time: for a BEFORE trigger the new
    // row must not be there yet.
    exec(
        &db,
        "CREATE TRIGGER t_bi BEFORE INSERT ON t BEGIN INSERT INTO log SELECT count(*) FROM t; END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");
    exec(&db, "INSERT INTO t VALUES (2)");
    assert_eq!(
        ints(&read_col(&db, "SELECT n FROM log")),
        vec![0, 1],
        "BEFORE INSERT sees the table without the incoming row"
    );
    cleanup(&db.path);
}

#[test]
fn after_delete_trigger_sees_old_values() {
    let db = open("ad");
    exec(&db, "CREATE TABLE t (a, b)");
    exec(&db, "CREATE TABLE log (a, b)");
    exec(
        &db,
        "CREATE TRIGGER t_ad AFTER DELETE ON t BEGIN INSERT INTO log VALUES (OLD.a, OLD.b); END",
    );
    exec(&db, "INSERT INTO t VALUES (1, 10), (2, 20)");
    exec(&db, "DELETE FROM t WHERE a = 2");
    assert_eq!(
        read_rows(&db, "SELECT a, b FROM log"),
        vec![vec![Value::Integer(2), Value::Integer(20)]],
        "OLD.* is readable from an AFTER DELETE body, after the row is gone"
    );
    assert_eq!(ints(&read_col(&db, "SELECT a FROM t")), vec![1]);
    cleanup(&db.path);
}

#[test]
fn update_trigger_sees_both_row_images() {
    let db = open("au");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (before_v, after_v)");
    exec(
        &db,
        "CREATE TRIGGER t_au AFTER UPDATE ON t BEGIN INSERT INTO log VALUES (OLD.a, NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (2)");
    exec(&db, "UPDATE t SET a = a + 100");
    let mut rows = read_rows(&db, "SELECT before_v, after_v FROM log");
    rows.sort_by_key(|r| match r[0] {
        Value::Integer(i) => i,
        _ => 0,
    });
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Integer(101)],
            vec![Value::Integer(2), Value::Integer(102)],
        ]
    );
    cleanup(&db.path);
}

#[test]
fn before_update_trigger_sees_pre_update_table_state() {
    let db = open("bu");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (old_v, new_v, still_old)");
    // `still_old` reads the table itself: for a BEFORE trigger the row must
    // still hold its old value.
    exec(
        &db,
        "CREATE TRIGGER t_bu BEFORE UPDATE ON t BEGIN \
         INSERT INTO log SELECT OLD.a, NEW.a, (SELECT a FROM t WHERE rowid = OLD.rowid); END",
    );
    exec(&db, "INSERT INTO t VALUES (5)");
    exec(&db, "UPDATE t SET a = 9");
    assert_eq!(
        read_rows(&db, "SELECT old_v, new_v, still_old FROM log"),
        vec![vec![
            Value::Integer(5),
            Value::Integer(9),
            Value::Integer(5)
        ]]
    );
    cleanup(&db.path);
}

#[test]
fn update_of_column_filter_matches_sqlite() {
    let db = open("update_of");
    exec(&db, "CREATE TABLE t (a, b)");
    exec(&db, "CREATE TABLE log (tag)");
    exec(
        &db,
        "CREATE TRIGGER t_of AFTER UPDATE OF b ON t BEGIN INSERT INTO log VALUES ('b'); END",
    );
    exec(&db, "INSERT INTO t VALUES (1, 2)");

    exec(&db, "UPDATE t SET a = 11");
    assert_eq!(
        read_col(&db, "SELECT tag FROM log").len(),
        0,
        "UPDATE OF b must not fire when only a is assigned"
    );

    exec(&db, "UPDATE t SET b = 22");
    assert_eq!(
        read_col(&db, "SELECT tag FROM log").len(),
        1,
        "UPDATE OF b fires when b is assigned"
    );

    exec(&db, "UPDATE t SET a = 33, b = 44");
    assert_eq!(
        read_col(&db, "SELECT tag FROM log").len(),
        2,
        "UPDATE OF b fires when b is among the assigned columns"
    );
    cleanup(&db.path);
}

#[test]
fn when_clause_gates_firing() {
    let db = open("when");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t WHEN NEW.a > 10 \
         BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (11), (2), (12)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log ORDER BY a")),
        vec![11, 12]
    );
    cleanup(&db.path);
}

#[test]
fn new_and_old_rowid_are_addressable() {
    let db = open("rowid");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (rid)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.rowid); END",
    );
    exec(
        &db,
        "CREATE TRIGGER t_ad AFTER DELETE ON t BEGIN INSERT INTO log VALUES (-OLD.rowid); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (2)");
    exec(&db, "DELETE FROM t WHERE a = 1");
    assert_eq!(
        ints(&read_col(&db, "SELECT rid FROM log ORDER BY rid")),
        vec![-1, 1, 2]
    );
    cleanup(&db.path);
}

// ---------------------------------------------------------------------------
// RAISE
// ---------------------------------------------------------------------------

#[test]
fn raise_abort_rejects_the_write_with_the_trigger_message() {
    let db = open("raise_abort");
    exec(&db, "CREATE TABLE t (a)");
    exec(
        &db,
        "CREATE TRIGGER t_guard BEFORE INSERT ON t WHEN NEW.a < 0 \
         BEGIN SELECT RAISE(ABORT, 'a must be non-negative'); END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");

    let err = exec_err(&db, "INSERT INTO t VALUES (-1)");
    assert!(
        err.contains("a must be non-negative"),
        "the trigger's own message must surface: {err}"
    );
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM t")),
        vec![1],
        "the rejected row was not written"
    );
    cleanup(&db.path);
}

#[test]
fn raise_rollback_and_fail_also_raise_the_message() {
    for (tag, form) in [("rollback", "ROLLBACK"), ("fail", "FAIL")] {
        let db = open(tag);
        exec(&db, "CREATE TABLE t (a)");
        exec(
            &db,
            &format!(
                "CREATE TRIGGER t_guard BEFORE INSERT ON t WHEN NEW.a < 0 \
                 BEGIN SELECT RAISE({form}, 'negative rejected'); END"
            ),
        );
        let err = exec_err(&db, "INSERT INTO t VALUES (-5)");
        assert!(
            err.contains("negative rejected"),
            "RAISE({form}) must surface its message: {err}"
        );
        cleanup(&db.path);
    }
}

#[test]
fn raise_ignore_skips_the_row_and_continues_the_statement() {
    let db = open("raise_ignore");
    exec(&db, "CREATE TABLE t (a)");
    exec(
        &db,
        "CREATE TRIGGER t_skip BEFORE INSERT ON t WHEN NEW.a < 0 \
         BEGIN SELECT RAISE(IGNORE); END",
    );
    // The negative row is dropped; the statement keeps going and the other rows
    // land. This is what upstream's "jump to the calling OP_Program's P2" does.
    exec(&db, "INSERT INTO t VALUES (1), (-1), (2)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM t ORDER BY a")),
        vec![1, 2]
    );
    cleanup(&db.path);
}

#[test]
fn raise_ignore_skips_a_delete_but_keeps_deleting_other_rows() {
    let db = open("raise_ignore_del");
    exec(&db, "CREATE TABLE t (a)");
    exec(
        &db,
        "CREATE TRIGGER t_keep BEFORE DELETE ON t WHEN OLD.a = 2 \
         BEGIN SELECT RAISE(IGNORE); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (2), (3)");
    exec(&db, "DELETE FROM t");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM t")),
        vec![2],
        "row 2 was protected; the rest of the DELETE still ran"
    );
    cleanup(&db.path);
}

#[test]
fn raise_outside_a_trigger_is_a_clean_error() {
    let db = open("raise_outside");
    let err = exec_err(&db, "SELECT RAISE(ABORT, 'nope')");
    assert!(
        err.contains("RAISE"),
        "RAISE outside a trigger must error, not panic: {err}"
    );
    cleanup(&db.path);
}

// ---------------------------------------------------------------------------
// Semantics that are easy to get subtly wrong
// ---------------------------------------------------------------------------

#[test]
fn trigger_body_writes_do_not_count_towards_changes() {
    let db = open("changes");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN \
         INSERT INTO log VALUES (NEW.a); INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");
    assert_eq!(
        read_one(&db, "SELECT changes()"),
        Value::Integer(1),
        "changes() reports the outer INSERT's single row, not the trigger's two"
    );
    assert_eq!(
        read_col(&db, "SELECT a FROM log").len(),
        2,
        "the body really did write both rows"
    );
    cleanup(&db.path);
}

#[test]
fn triggers_chain_but_never_re_enter_themselves() {
    // `recursive_triggers` is off (upstream's default): a trigger may fire other
    // triggers, but a trigger that would re-enter itself is skipped instead of
    // recursing forever.
    let db = open("chain");
    exec(&db, "CREATE TABLE a (x)");
    exec(&db, "CREATE TABLE b (x)");
    exec(&db, "CREATE TABLE c (x)");
    exec(
        &db,
        "CREATE TRIGGER a_ai AFTER INSERT ON a BEGIN INSERT INTO b VALUES (NEW.x + 1); END",
    );
    exec(
        &db,
        "CREATE TRIGGER b_ai AFTER INSERT ON b BEGIN INSERT INTO c VALUES (NEW.x + 1); END",
    );
    exec(&db, "INSERT INTO a VALUES (1)");
    assert_eq!(ints(&read_col(&db, "SELECT x FROM b")), vec![2]);
    assert_eq!(
        ints(&read_col(&db, "SELECT x FROM c")),
        vec![3],
        "non-recursive nesting (a -> b -> c) works"
    );

    // Self-referential trigger: inserts exactly one extra row, then stops.
    let db2 = open("selfref");
    exec(&db2, "CREATE TABLE t (x)");
    exec(
        &db2,
        "CREATE TRIGGER t_ai AFTER INSERT ON t WHEN NEW.x < 5 \
         BEGIN INSERT INTO t VALUES (NEW.x + 1); END",
    );
    exec(&db2, "INSERT INTO t VALUES (1)");
    assert_eq!(
        ints(&read_col(&db2, "SELECT x FROM t ORDER BY x")),
        vec![1, 2],
        "the trigger's own INSERT does not re-fire the trigger"
    );
    cleanup(&db2.path);
    cleanup(&db.path);
}

#[test]
fn trigger_body_can_update_and_delete_other_tables() {
    let db = open("body_ops");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE counter (n)");
    exec(&db, "CREATE TABLE junk (a)");
    exec(&db, "INSERT INTO counter VALUES (0)");
    exec(&db, "INSERT INTO junk VALUES (1), (2), (3)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN \
         UPDATE counter SET n = n + 1; DELETE FROM junk WHERE a = NEW.a; END",
    );
    exec(&db, "INSERT INTO t VALUES (2)");
    assert_eq!(ints(&read_col(&db, "SELECT n FROM counter")), vec![1]);
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM junk ORDER BY a")),
        vec![1, 3],
        "the body's DELETE used NEW.a as its predicate"
    );
    cleanup(&db.path);
}

#[test]
fn instead_of_trigger_on_a_table_is_rejected() {
    let db = open("instead_of");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    let err = exec_err(
        &db,
        "CREATE TRIGGER t_io INSTEAD OF INSERT ON t BEGIN INSERT INTO log VALUES (1); END",
    );
    assert!(
        err.contains("INSTEAD OF"),
        "INSTEAD OF on a table is rejected as upstream does: {err}"
    );
    cleanup(&db.path);
}

#[test]
fn temporary_trigger_fires_and_stays_out_of_main_schema() {
    // Q11d landed the `temp` catalog, so a TEMP trigger is no longer refused:
    // it is stored in `temp`, fires like any other trigger, and never reaches
    // `main`'s `sqlite_schema`. (`tests/multi_database.rs` additionally proves
    // it does not survive the connection.)
    let db = open("temp_trigger");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TEMP TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");
    assert_eq!(
        read_col(&db, "SELECT a FROM log"),
        vec![Value::Integer(1)],
        "a TEMP trigger must actually fire"
    );
    assert!(
        read_col(&db, "SELECT name FROM sqlite_schema WHERE type = 'trigger'").is_empty(),
        "a TEMP trigger must never be persisted into main's sqlite_schema"
    );
    cleanup(&db.path);
}

#[test]
fn unsupported_select_body_is_a_typed_error_not_a_silent_no_op() {
    let db = open("select_body");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE other (a)");
    exec(
        &db,
        "CREATE TRIGGER t_ai AFTER INSERT ON t BEGIN SELECT a FROM other; END",
    );
    // The trigger stores fine (SQLite accepts the syntax); firing it reports the
    // documented limitation instead of quietly dropping the statement.
    let err = exec_err(&db, "INSERT INTO t VALUES (1)");
    assert!(
        err.contains("Only `SELECT RAISE(...)` is supported"),
        "unexpected error: {err}"
    );
    cleanup(&db.path);
}

#[test]
fn raise_ignore_after_a_body_write_still_isolates_changes() {
    // Regression: `RAISE(IGNORE)` jumps clear over the change-counter restore
    // emitted at the end of the trigger's region, so the restore has to happen
    // before the jump. Otherwise rows the body wrote before raising IGNORE stay
    // counted in the outer statement's `changes()`.
    let db = open("ignore_changes");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER t_bi BEFORE INSERT ON t WHEN NEW.a < 0 BEGIN \
         INSERT INTO log VALUES (NEW.a); INSERT INTO log VALUES (NEW.a); \
         SELECT RAISE(IGNORE); END",
    );
    exec(&db, "INSERT INTO t VALUES (1), (-1), (2)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM t ORDER BY a")),
        vec![1, 2],
        "the negative row was skipped and the statement continued"
    );
    assert_eq!(
        read_col(&db, "SELECT a FROM log").len(),
        2,
        "the body's two writes happened before the IGNORE"
    );
    assert_eq!(
        read_one(&db, "SELECT changes()"),
        Value::Integer(2),
        "changes() counts the two surviving outer rows only — the trigger body's \
         two log rows must not leak in through the RAISE(IGNORE) path"
    );
    cleanup(&db.path);
}

#[test]
fn trigger_name_with_a_quote_round_trips_through_parse_schema() {
    // `translate_create_trigger` interpolates the trigger name into a
    // `ParseSchema` WHERE clause that is re-parsed as SQL, so the name has to be
    // escaped as a single-quoted literal. A name containing a quote is the
    // adversarial case: if the escape were wrong the reload would either miss
    // the trigger or fail to parse.
    let db = open("quoted_name");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TRIGGER \"weird'name\" AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );
    exec(&db, "INSERT INTO t VALUES (1)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log")),
        vec![1],
        "the trigger registered under its quoted name and fired"
    );
    exec(&db, "DROP TRIGGER \"weird'name\"");
    exec(&db, "INSERT INTO t VALUES (2)");
    assert_eq!(
        ints(&read_col(&db, "SELECT a FROM log")),
        vec![1],
        "and dropped cleanly"
    );
    cleanup(&db.path);
}
