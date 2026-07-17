//! Regression coverage for the removal of the "temporary create-then-destroy" dance that used
//! to back `VTabModuleImpl::create_schema` (see `crates/oxisqlite-ext/src/vtabs.rs`). Virtual
//! table column names are no longer persisted into `sqlite_schema.sql` as a synthesized comment
//! at `CREATE VIRTUAL TABLE` time; they are resolved on demand from the single, live vtab
//! instance the `VCreate` instruction already creates (see `VirtualTable::table`/
//! `resolve_columns` in `crates/oxisqlite-core/vtab.rs`), the same instantiation `PRAGMA
//! table_info` and query compilation already read via `Table::columns`.
//!
//! This file asserts two things:
//!  1. Columns are still correctly discoverable after `CREATE VIRTUAL TABLE`, both via `PRAGMA
//!     table_info` and via the raw `sql` text stored in `sqlite_schema` -- which must no longer
//!     carry the old `/* tbl_name(col1, col2) */` comment -- matching what the old (now removed)
//!     cached/comment-based approach used to report.
//!  2. Two separate, independent schema-introspection queries report identical, correct columns
//!     and do not trigger any additional `xCreate`/`xDestroy` calls into the vtab module: there is
//!     exactly one live instantiation, created once by `CREATE VIRTUAL TABLE`, and schema
//!     introspection only ever reads it rather than re-creating (or destroying) it.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex, MutexGuard,
};

use limbo_core::{Connection, Database, MemoryIO, StepResult, IO};
use limbo_ext::{
    ExtensionApi, ResultCode, VTabCursor, VTabKind, VTabModule, VTabModuleDerive, VTable, Value,
};

/// Number of times `SchemaColsVtabModule::create` (xCreate) has been invoked, process-wide.
///
/// Before the fix, `CREATE VIRTUAL TABLE` drove this to 2: once for the real, persisted
/// instance (`VCreate`), and once more for the old `create_schema`'s throwaway
/// create-then-destroy, run purely to read column names for a comment embedded in the
/// persisted DDL text. After the fix it must be exactly 1.
static CREATE_CALLS: AtomicUsize = AtomicUsize::new(0);
/// Number of times `SchemaColsTable::destroy` (xDestroy) has been invoked, process-wide.
///
/// Before the fix this was driven to 1 immediately by `create_schema`'s cleanup, even though
/// the real, live table instance is never destroyed by `CREATE VIRTUAL TABLE` itself. After the
/// fix it must stay at 0 for the lifetime of this test (nothing here ever drops the table).
static DESTROY_CALLS: AtomicUsize = AtomicUsize::new(0);

/// Serializes tests in this file: they share the process-wide counters above.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn lock_tests() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory: database");
    let conn = db.connect().expect("connect to :memory: database");
    (io, conn)
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Runs `PRAGMA table_info(<table_name>)` to completion, collecting each row's
/// `(cid, name, type, notnull, pk)` -- enough to validate the reported column list without
/// depending on the exact `dflt_value` representation.
fn table_info_rows(
    io: &Arc<dyn IO>,
    conn: &Arc<Connection>,
    table_name: &str,
) -> Vec<(i64, String, String, i64, i64)> {
    let sql = format!("PRAGMA table_info({table_name})");
    let mut stmt = conn
        .query(&sql)
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
                let cid = row.get::<i64>(0).expect("cid column");
                let name = row.get::<String>(1).expect("name column");
                let ty = row.get::<String>(2).expect("type column");
                let notnull = row.get::<i64>(3).expect("notnull column");
                let pk = row.get::<i64>(5).expect("pk column");
                out.push((cid, name, ty, notnull, pk));
            }
            StepResult::IO => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

/// Returns the persisted `sql` text `sqlite_schema` holds for `table_name`.
fn schema_sql_text(io: &Arc<dyn IO>, conn: &Arc<Connection>, table_name: &str) -> String {
    let sql =
        format!("SELECT sql FROM sqlite_schema WHERE tbl_name = '{table_name}' AND type = 'table'");
    let mut stmt = conn
        .query(&sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement produced for {sql}"));
    let mut out = None;
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                out = row.get::<String>(0).ok();
            }
            StepResult::IO => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out.unwrap_or_else(|| panic!("no sqlite_schema row found for table {table_name}"))
}

fn expected_columns() -> Vec<(i64, String, String, i64, i64)> {
    vec![
        (0, "id".to_string(), "INTEGER".to_string(), 0, 0),
        (1, "name".to_string(), "TEXT".to_string(), 0, 0),
        (2, "score".to_string(), "REAL".to_string(), 0, 0),
    ]
}

// -----------------------------------------------------------------------
// A tiny vtab module with three declared columns, used purely to observe schema
// introspection. Every `create` (xCreate) call increments `CREATE_CALLS`; every `destroy`
// (xDestroy) call increments `DESTROY_CALLS`. It is never actually scanned by any query in
// this file, so `open`/`filter`/`column`/`next`/`eof` are intentionally trivial.
// -----------------------------------------------------------------------

#[derive(Debug, VTabModuleDerive)]
struct SchemaColsVtabModule;

impl VTabModule for SchemaColsVtabModule {
    type Table = SchemaColsTable;
    const VTAB_KIND: VTabKind = VTabKind::VirtualTable;
    const NAME: &'static str = "test_schema_cols";

    fn create(_args: &[Value]) -> Result<(String, Self::Table), ResultCode> {
        CREATE_CALLS.fetch_add(1, Ordering::SeqCst);
        let schema = "CREATE TABLE test_schema_cols(id INTEGER, name TEXT, score REAL)".to_string();
        Ok((schema, SchemaColsTable))
    }
}

struct SchemaColsTable;

impl VTable for SchemaColsTable {
    type Cursor = SchemaColsCursor;
    type Error = &'static str;

    fn open(&self, _conn: Option<Arc<limbo_ext::Connection>>) -> Result<Self::Cursor, Self::Error> {
        Ok(SchemaColsCursor)
    }

    fn destroy(&mut self) -> Result<(), Self::Error> {
        DESTROY_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct SchemaColsCursor;

impl VTabCursor for SchemaColsCursor {
    type Error = &'static str;

    fn filter(&mut self, _args: &[Value], _idx_info: Option<(&str, i32)>) -> ResultCode {
        ResultCode::OK
    }

    fn rowid(&self) -> i64 {
        0
    }

    fn column(&self, _idx: u32) -> Result<Value, Self::Error> {
        Ok(Value::null())
    }

    fn eof(&self) -> bool {
        true
    }

    fn next(&mut self) -> ResultCode {
        ResultCode::EOF
    }
}

fn setup(table_name: &str) -> (Arc<dyn IO>, Arc<Connection>) {
    let (io, conn) = new_conn();
    let ext_api: ExtensionApi = conn.build_limbo_ext();
    let rc = unsafe { SchemaColsVtabModule::register_SchemaColsVtabModule(&ext_api as *const _) };
    assert!(rc.is_ok(), "vtab module registration failed: {rc:?}");
    exec(
        &conn,
        &format!("CREATE VIRTUAL TABLE {table_name} USING test_schema_cols()"),
    );
    (io, conn)
}

#[test]
fn vtab_columns_discoverable_via_pragma_and_sqlite_schema() {
    let _guard = lock_tests();
    CREATE_CALLS.store(0, Ordering::SeqCst);
    DESTROY_CALLS.store(0, Ordering::SeqCst);

    let (io, conn) = setup("t1");

    // Exactly one live instantiation backs `CREATE VIRTUAL TABLE`. The old "create-then-
    // destroy" dance used to add a second, throwaway `create` (immediately paired with a
    // `destroy`) purely to read the schema for a comment embedded in the persisted DDL text --
    // that extra create/destroy pair (and the comment it produced) is gone.
    assert_eq!(
        CREATE_CALLS.load(Ordering::SeqCst),
        1,
        "CREATE VIRTUAL TABLE must instantiate the module exactly once"
    );
    assert_eq!(
        DESTROY_CALLS.load(Ordering::SeqCst),
        0,
        "CREATE VIRTUAL TABLE must not destroy the live instance it just created"
    );

    let columns = table_info_rows(&io, &conn, "t1");
    assert_eq!(
        columns,
        expected_columns(),
        "PRAGMA table_info must report the vtab module's declared columns"
    );

    let sql_text = schema_sql_text(&io, &conn, "t1");
    assert!(
        !sql_text.contains("/*"),
        "persisted sqlite_schema.sql text must no longer embed a column-list comment, got: {sql_text:?}"
    );
    assert!(
        !sql_text.contains('\n'),
        "persisted sqlite_schema.sql text must be a single line (no appended comment line), got: {sql_text:?}"
    );
    assert!(
        sql_text.contains("CREATE VIRTUAL TABLE t1"),
        "persisted sqlite_schema.sql text must still be the literal CREATE VIRTUAL TABLE DDL, got: {sql_text:?}"
    );
    assert!(
        sql_text.contains("test_schema_cols"),
        "persisted sqlite_schema.sql text must still reference the module name, got: {sql_text:?}"
    );
}

#[test]
fn vtab_repeated_schema_introspection_is_stable_and_has_no_side_effects() {
    let _guard = lock_tests();
    CREATE_CALLS.store(0, Ordering::SeqCst);
    DESTROY_CALLS.store(0, Ordering::SeqCst);

    let (io, conn) = setup("t2");
    let create_calls_after_create = CREATE_CALLS.load(Ordering::SeqCst);
    let destroy_calls_after_create = DESTROY_CALLS.load(Ordering::SeqCst);

    // Two independent `PRAGMA table_info` calls -- separate prepare+step cycles, not a reused
    // statement -- must agree with each other and with the module's declared columns.
    let first = table_info_rows(&io, &conn, "t2");
    let second = table_info_rows(&io, &conn, "t2");

    assert_eq!(
        first, second,
        "two independent PRAGMA table_info calls must report identical columns"
    );
    assert_eq!(
        first,
        expected_columns(),
        "repeated introspection must still report the module's declared columns"
    );

    assert_eq!(
        CREATE_CALLS.load(Ordering::SeqCst),
        create_calls_after_create,
        "schema introspection must not re-instantiate (xCreate) the vtab module"
    );
    assert_eq!(
        DESTROY_CALLS.load(Ordering::SeqCst),
        destroy_calls_after_create,
        "schema introspection must not destroy (xDestroy) the live vtab instance"
    );

    // A third, differently-shaped introspection query (a raw `sqlite_schema` read, as `.schema`
    // would issue) must also be stable and side-effect free.
    let sql_text = schema_sql_text(&io, &conn, "t2");
    assert!(
        sql_text.contains("test_schema_cols"),
        "sqlite_schema.sql read must still reference the module name, got: {sql_text:?}"
    );
    assert_eq!(
        CREATE_CALLS.load(Ordering::SeqCst),
        create_calls_after_create,
        "reading sqlite_schema.sql must not re-instantiate (xCreate) the vtab module"
    );
    assert_eq!(
        DESTROY_CALLS.load(Ordering::SeqCst),
        destroy_calls_after_create,
        "reading sqlite_schema.sql must not destroy (xDestroy) the live vtab instance"
    );
}
