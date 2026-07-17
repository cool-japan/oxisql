//! Integration tests for `ALTER TABLE ... RENAME TO` / `ALTER TABLE ... RENAME COLUMN`.
//!
//! These specifically cover the bug where `cursor_loop`'s unfiltered scan of every
//! `sqlite_schema` row during a rename hit a `todo!()` (panic) the instant the schema
//! contained a `VIEW`/`TRIGGER`/`CREATE VIRTUAL TABLE` row anywhere at all, even one
//! utterly unrelated to the table/column actually being renamed. See
//! `crates/oxisqlite-core/vdbe/execute/function.rs`'s `AlterTableFunc::RenameTable` /
//! `AlterTableFunc::RenameColumn` handling in `op_function`.
//!
//! `CREATE VIEW` / `CREATE TRIGGER` are not yet implemented as top-level statements in
//! this engine (`translate::translate_inner` bails with "not supported yet" for both),
//! so views/triggers are injected directly via `INSERT INTO sqlite_schema ...` — a plain
//! row insert into an ordinary btree table, which is unaffected by that front-end
//! limitation. This exercises exactly the same `op_function` code path a real
//! `CREATE VIEW`/`CREATE TRIGGER` would have populated, since the rename's `cursor_loop`
//! only ever looks at the persisted rows, never at how they got there.
//!
//! `RenameTable` and `RenameColumn` are NOT equally exposed to this bug: `RenameTable`
//! re-parses every non-null `sql` value in the schema unconditionally (only checking
//! relatedness to the renamed table *after* parsing), so any view/trigger/vtab row
//! anywhere panicked it. `RenameColumn` has a `if table != tbl_name { break 'sql None }`
//! short-circuit *before* it ever parses, so it only panicked on a row whose `tbl_name`
//! happens to equal the table being altered — trivially true for a trigger defined ON
//! that table, but impossible for a view (a view's `tbl_name` is its own name, which
//! can't collide with an existing table's name). The `rename_column_*` tests below are
//! deliberately split to cover both the short-circuited-away case and the case that
//! actually reaches the fixed match arms.

use limbo_core::{Database, LimboError, MemoryIO, StepResult, Value};
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
) -> Result<(), LimboError> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Done => return Ok(()),
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(LimboError::Busy),
        }
    }
}

/// Run a query and collect every row's values as a `Vec<Vec<Value>>`, in result order.
fn query_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<Vec<Value>> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare `{sql}`: {e:?}"));
    let mut rows = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step `{sql}`: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                let n = stmt.num_columns();
                rows.push((0..n).map(|i| row.get_value(i).clone()).collect());
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => return rows,
            StepResult::Interrupt => panic!("interrupted running `{sql}`"),
        }
    }
}

/// The persisted DDL text (and rowid-bearing columns) for a single named `sqlite_schema`
/// row of a given `type`. Panics if there isn't exactly one match.
fn schema_row(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    entry_type: &str,
    name: &str,
) -> (String, String, i64, Option<String>) {
    let sql = format!(
        "SELECT type, tbl_name, rootpage, sql FROM sqlite_schema \
         WHERE name = '{name}' AND type = '{entry_type}'"
    );
    let rows = query_rows(io, conn, &sql);
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one sqlite_schema row for {entry_type} {name}, got {}",
        rows.len()
    );
    let row = &rows[0];
    let get_text = |v: &Value| match v {
        Value::Text(t) => t.as_str().to_string(),
        other => panic!("expected text column, got {other:?}"),
    };
    let entry_type = get_text(&row[0]);
    let tbl_name = get_text(&row[1]);
    let rootpage = match &row[2] {
        Value::Integer(i) => *i,
        other => panic!("expected integer rootpage, got {other:?}"),
    };
    let sql_text = match &row[3] {
        Value::Text(t) => Some(t.as_str().to_string()),
        Value::Null => None,
        other => panic!("expected text or null sql column, got {other:?}"),
    };
    (entry_type, tbl_name, rootpage, sql_text)
}

fn schema_row_exists(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    entry_type: &str,
    name: &str,
) -> bool {
    let sql = format!(
        "SELECT COUNT(*) FROM sqlite_schema WHERE name = '{name}' AND type = '{entry_type}'"
    );
    match &query_rows(io, conn, &sql)[0][0] {
        Value::Integer(n) => *n == 1,
        other => panic!("expected integer count, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------
// Headline bug: ALTER TABLE ... RENAME {TO,COLUMN} must not panic when the schema
// contains an unrelated VIEW or TRIGGER row anywhere.
// ---------------------------------------------------------------------------------

#[test]
fn rename_table_succeeds_with_unrelated_view_present() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    exec(&io, &conn, "CREATE TABLE other_table(y)").expect("create other_table");
    exec(&io, &conn, "INSERT INTO t VALUES (1), (2), (3)").expect("seed t");

    // Simulate a pre-existing, wholly unrelated view (CREATE VIEW isn't implemented as a
    // top-level statement yet, see module docs above).
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('view', 'v', 'v', 0, 'CREATE VIEW v AS SELECT * FROM other_table')",
    )
    .expect("inject unrelated view row");

    // This used to panic (`todo!()`) the instant the cursor_loop's unfiltered scan over
    // every sqlite_schema row reached the view row.
    exec(&io, &conn, "ALTER TABLE t RENAME TO t2")
        .expect("ALTER TABLE RENAME TO must succeed even with an unrelated view present");

    // t2 exists with t's data intact; t is gone.
    assert_eq!(
        query_rows(&io, &conn, "SELECT x FROM t2 ORDER BY x"),
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
        ]
    );
    assert!(
        exec(&io, &conn, "SELECT * FROM t").is_err(),
        "old table name t must no longer resolve"
    );

    // The view is structurally untouched: still present, and since it doesn't reference
    // `t` at all, its body is byte-for-byte unchanged.
    let (entry_type, tbl_name, rootpage, sql) = schema_row(&io, &conn, "view", "v");
    assert_eq!(entry_type, "view");
    assert_eq!(tbl_name, "v");
    assert_eq!(rootpage, 0);
    assert_eq!(
        sql.as_deref(),
        Some("CREATE VIEW v AS SELECT * FROM other_table")
    );
}

#[test]
fn rename_table_succeeds_with_unrelated_trigger_present() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    exec(&io, &conn, "CREATE TABLE other_table(y)").expect("create other_table");
    exec(&io, &conn, "INSERT INTO t VALUES (10), (20)").expect("seed t");

    // Simulate a pre-existing trigger defined on a different table (CREATE TRIGGER isn't
    // implemented as a top-level statement yet, see module docs above).
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('trigger', 'trg', 'other_table', 0, \
         'CREATE TRIGGER trg AFTER INSERT ON other_table BEGIN SELECT 1; END')",
    )
    .expect("inject unrelated trigger row");

    exec(&io, &conn, "ALTER TABLE t RENAME TO t2")
        .expect("ALTER TABLE RENAME TO must succeed even with an unrelated trigger present");

    assert_eq!(
        query_rows(&io, &conn, "SELECT x FROM t2 ORDER BY x"),
        vec![vec![Value::Integer(10)], vec![Value::Integer(20)]]
    );

    let (entry_type, tbl_name, rootpage, sql) = schema_row(&io, &conn, "trigger", "trg");
    assert_eq!(entry_type, "trigger");
    assert_eq!(
        tbl_name, "other_table",
        "trigger's own tbl_name is untouched"
    );
    assert_eq!(rootpage, 0);
    assert_eq!(
        sql.as_deref(),
        Some("CREATE TRIGGER trg AFTER INSERT ON other_table BEGIN SELECT 1; END")
    );
}

#[test]
fn rename_column_succeeds_with_unrelated_view_present() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x, y)").expect("create t");
    exec(&io, &conn, "CREATE TABLE other_table(z)").expect("create other_table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a'), (2, 'b')").expect("seed t");

    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('view', 'v', 'v', 0, 'CREATE VIEW v AS SELECT * FROM other_table')",
    )
    .expect("inject unrelated view row");

    exec(&io, &conn, "ALTER TABLE t RENAME COLUMN x TO z2")
        .expect("ALTER TABLE RENAME COLUMN must succeed even with an unrelated view present");

    assert_eq!(
        query_rows(&io, &conn, "SELECT z2, y FROM t ORDER BY z2"),
        vec![
            vec![Value::Integer(1), Value::build_text("a")],
            vec![Value::Integer(2), Value::build_text("b")],
        ]
    );

    let (_, _, _, sql) = schema_row(&io, &conn, "view", "v");
    assert_eq!(
        sql.as_deref(),
        Some("CREATE VIEW v AS SELECT * FROM other_table"),
        "unrelated view body must be untouched by an unrelated table's column rename"
    );
}

#[test]
fn rename_column_succeeds_with_unrelated_trigger_present() {
    // NOTE: `RenameColumn` has a pre-parse `if table != tbl_name { break 'sql None }`
    // short-circuit that `RenameTable` lacks (see the doc comment above), so a trigger
    // whose `tbl_name` differs from the table being altered never even reaches the
    // parser/match that this bug lives in. This test guards that short-circuit itself
    // (it must still pass pre-fix); `rename_column_succeeds_with_trigger_on_altered_table`
    // below is the one that actually exercises the fixed code path for `RenameColumn`.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x, y)").expect("create t");
    exec(&io, &conn, "CREATE TABLE other_table(z)").expect("create other_table");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a'), (2, 'b')").expect("seed t");

    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('trigger', 'trg', 'other_table', 0, \
         'CREATE TRIGGER trg AFTER INSERT ON other_table BEGIN SELECT 1; END')",
    )
    .expect("inject unrelated trigger row");

    exec(&io, &conn, "ALTER TABLE t RENAME COLUMN x TO z2")
        .expect("ALTER TABLE RENAME COLUMN must succeed even with an unrelated trigger present");

    assert_eq!(
        query_rows(&io, &conn, "SELECT z2, y FROM t ORDER BY z2"),
        vec![
            vec![Value::Integer(1), Value::build_text("a")],
            vec![Value::Integer(2), Value::build_text("b")],
        ]
    );
    assert!(schema_row_exists(&io, &conn, "trigger", "trg"));
}

#[test]
fn rename_column_succeeds_with_trigger_on_altered_table() {
    // The scenario that actually exercises `RenameColumn`'s `CreateTrigger(_) => None`
    // arm: a trigger's `tbl_name` is the table it fires ON, so a trigger defined on `t`
    // itself (very realistic — e.g. an audit trigger) shares `tbl_name` with the column
    // being renamed and *does* reach the parser/match, unlike the "unrelated" trigger
    // above. Pre-fix, this panics via `todo!()`.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x, y)").expect("create t");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a'), (2, 'b')").expect("seed t");

    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('trigger', 'trg', 't', 0, \
         'CREATE TRIGGER trg AFTER INSERT ON t BEGIN SELECT 1; END')",
    )
    .expect("inject trigger defined on t");

    exec(&io, &conn, "ALTER TABLE t RENAME COLUMN x TO z2").expect(
        "ALTER TABLE RENAME COLUMN must succeed even with a trigger defined on the very \
         table/column being altered",
    );

    assert_eq!(
        query_rows(&io, &conn, "SELECT z2, y FROM t ORDER BY z2"),
        vec![
            vec![Value::Integer(1), Value::build_text("a")],
            vec![Value::Integer(2), Value::build_text("b")],
        ]
    );
    // The trigger body is left exactly as-is (known limitation, not corruption — see the
    // comment on `op_function`): it still refers to the column by its old name `x`.
    let (_, tbl_name, _, sql) = schema_row(&io, &conn, "trigger", "trg");
    assert_eq!(tbl_name, "t");
    assert_eq!(
        sql.as_deref(),
        Some("CREATE TRIGGER trg AFTER INSERT ON t BEGIN SELECT 1; END")
    );
}

#[test]
fn rename_column_does_not_panic_with_view_row_sharing_tbl_name() {
    // A *real* view's `tbl_name` is always its own name, which can never collide with an
    // existing table's name (shared schema namespace), so `RenameColumn`'s pre-parse
    // `tbl_name` short-circuit makes the `CreateView` arm unreachable for any naturally
    // occurring row. To still directly exercise that match arm (rather than leave it
    // proven only by code inspection), this forces a `view`-typed row's `tbl_name` to
    // equal the altered table — not a shape a real `CREATE VIEW` ever produces, but a
    // cheap, direct way to drive `op_function` into the exact arm and confirm it doesn't
    // panic. Pre-fix, this panics via `todo!()`.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x, y)").expect("create t");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a')").expect("seed t");

    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('view', 'v', 't', 0, 'CREATE VIEW v AS SELECT * FROM t')",
    )
    .expect("inject view row artificially sharing tbl_name with t");

    exec(&io, &conn, "ALTER TABLE t RENAME COLUMN x TO z2")
        .expect("must not panic when a view-typed row shares tbl_name with the altered table");
}

#[test]
fn rename_table_succeeds_with_view_trigger_and_unrelated_table_all_present() {
    // The exact combined scenario from the bug report: several unrelated schema-object
    // kinds coexisting with the table actually being renamed.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    exec(&io, &conn, "CREATE TABLE other_table(y)").expect("create other_table");
    exec(&io, &conn, "INSERT INTO t VALUES (42)").expect("seed t");
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('view', 'v', 'v', 0, 'CREATE VIEW v AS SELECT * FROM other_table')",
    )
    .expect("inject view row");
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('trigger', 'trg', 'other_table', 0, \
         'CREATE TRIGGER trg AFTER INSERT ON other_table BEGIN SELECT 1; END')",
    )
    .expect("inject trigger row");

    exec(&io, &conn, "ALTER TABLE t RENAME TO t2").expect(
        "ALTER TABLE RENAME TO must succeed with view + trigger + unrelated table all present",
    );

    assert_eq!(
        query_rows(&io, &conn, "SELECT x FROM t2"),
        vec![vec![Value::Integer(42)]]
    );
    assert!(schema_row_exists(&io, &conn, "view", "v"));
    assert!(schema_row_exists(&io, &conn, "trigger", "trg"));
    assert!(schema_row_exists(&io, &conn, "table", "other_table"));
}

// ---------------------------------------------------------------------------------
// CREATE VIRTUAL TABLE row present: this build compiles `CreateVirtualTable` support
// in unconditionally, but instantiating a *working* virtual table requires a module
// registered in `syms.vtab_modules`, which needs either dynamic (dlopen) extension
// loading or hand-written unsafe FFI extension scaffolding — neither of which exists
// in this crate's own test setup. So instead of a full create+rename+reload round
// trip, this proves the specific fix directly: the rename rewrite step (which used to
// panic via `todo!()` for a `CreateVirtualTable`-shaped row) now returns a clean,
// unrelated, typed error ("module not found", raised later while reloading the
// schema) instead of crashing the process.
// ---------------------------------------------------------------------------------

#[test]
fn rename_table_does_not_panic_with_virtual_table_row_present() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    exec(&io, &conn, "INSERT INTO t VALUES (7)").expect("seed t");

    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('table', 'vt', 'vt', 0, 'CREATE VIRTUAL TABLE vt USING nomodule()')",
    )
    .expect("inject virtual table row");

    // Before the fix this panicked (`todo!()`) inside the rename rewrite loop. After the
    // fix, the rewrite loop itself no longer panics; the statement still ultimately fails
    // (this crate has no `nomodule` vtab module registered to resurrect `vt` during the
    // unconditional full schema reload the rename performs at the end), but it must fail
    // with a clean, typed error rather than crashing the test process.
    let result = exec(&io, &conn, "ALTER TABLE t RENAME TO t2");
    let err = result.expect_err(
        "no vtab module named `nomodule` is registered, so this can't fully succeed, but it \
         must not panic",
    );
    let msg = err.to_string();
    assert!(
        msg.contains("nomodule") || msg.to_lowercase().contains("module"),
        "expected a 'virtual table module not found'-style error proving we got past the \
         rewrite step that used to panic, got: {msg}"
    );
}

// ---------------------------------------------------------------------------------
// Graceful re-parse guards: a corrupted/foreign-written `sqlite_schema.sql` value must
// produce a clean `Err`, never a panic, when the rename rewrite logic re-parses it.
// ---------------------------------------------------------------------------------

#[test]
fn rename_table_fails_gracefully_on_unparseable_schema_sql() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");

    // A `sqlite_schema` row whose `sql` text fails to parse at all (illegal token). This
    // simulates direct file corruption or a foreign tool writing garbage into the schema
    // table; it must never be reachable via this engine's own DDL.
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('table', 'bogus', 'bogus', 999, 'not @@@ valid sql (((')",
    )
    .expect("inject corrupted row");

    let result = exec(&io, &conn, "ALTER TABLE t RENAME TO t2");
    assert!(
        result.is_err(),
        "a corrupted schema row's unparseable SQL must fail the statement gracefully, not \
         panic and not silently succeed"
    );
}

#[test]
fn rename_table_fails_gracefully_on_non_statement_schema_sql() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");

    // Valid SQL that parses to `Cmd::Explain(..)` rather than `Cmd::Stmt(..)` — exercises
    // the re-parse guard's `else` branch (a successfully-parsed, but wrong-shaped, `Cmd`).
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('table', 'bogus', 'bogus', 999, 'EXPLAIN SELECT 1')",
    )
    .expect("inject corrupted row");

    let result = exec(&io, &conn, "ALTER TABLE t RENAME TO t2");
    assert!(
        result.is_err(),
        "a schema row whose sql parses to Cmd::Explain rather than Cmd::Stmt must fail \
         gracefully, not panic"
    );
}

#[test]
fn rename_table_fails_gracefully_on_wrong_statement_kind_schema_sql() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");

    // Valid SQL, valid `Cmd::Stmt`, but not one of CREATE TABLE/INDEX/VIEW/TRIGGER/VIRTUAL
    // TABLE — exercises the inner match's final corruption catch-all.
    exec(
        &io,
        &conn,
        "INSERT INTO sqlite_schema (type, name, tbl_name, rootpage, sql) \
         VALUES ('table', 'bogus', 'bogus', 999, 'SELECT 1')",
    )
    .expect("inject corrupted row");

    let result = exec(&io, &conn, "ALTER TABLE t RENAME TO t2");
    assert!(
        result.is_err(),
        "a schema row whose sql is a SELECT (not a CREATE statement) must fail gracefully, \
         not panic"
    );
}

#[test]
fn rename_column_fails_gracefully_on_as_select_table_definition() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE shadow(x)").expect("create shadow");

    // Corrupt `shadow`'s own persisted definition in place: `sqlite_schema.sql` for a
    // table is always stored in `ColumnsAndConstraints` form (even `CREATE TABLE ... AS
    // SELECT` desugars before being persisted, see `translate::schema`), so a `CREATE
    // TABLE ... AS SELECT`-shaped row can only arise here from direct corruption. This
    // exercises `RenameColumn`'s inner `CreateTableBody::ColumnsAndConstraints` guard.
    exec(
        &io,
        &conn,
        "UPDATE sqlite_schema SET sql = 'CREATE TABLE shadow AS SELECT 1' \
         WHERE name = 'shadow' AND type = 'table'",
    )
    .expect("corrupt shadow's persisted definition");

    let result = exec(&io, &conn, "ALTER TABLE shadow RENAME COLUMN x TO z");
    assert!(
        result.is_err(),
        "a table whose persisted definition has an AS-SELECT body (never produced by this \
         engine's own writes) must fail gracefully during RENAME COLUMN, not panic"
    );
}
