//! Regression tests for INSERT translation, split out of mod.rs for size.

#[cfg(test)]
mod insert_pk_param_tests {
    //! Regression tests for positional-parameter binding against columns that
    //! belong to a table-level `PRIMARY KEY(...)` constraint.
    //!
    //! Historically, an `INTEGER NOT NULL` column promoted to a rowid alias by a
    //! table-level PRIMARY KEY would trip the column's `NOT NULL` HaltIfNull
    //! guard (the rowid-alias register is intentionally SoftNull'd), causing a
    //! spurious "NOT NULL constraint failed" on `INSERT ... VALUES (?)`. These
    //! tests pin the corrected behaviour end-to-end.

    use crate::schema::Column;
    use crate::schema::Type;
    use crate::{Database, StepResult, Value};
    use std::num::NonZero;
    use std::sync::Arc;

    use super::super::column_is_nullable;

    /// Build an in-memory database, run `create`, INSERT a single bound integer
    /// parameter via `INSERT INTO t VALUES (?)`, then read column `select_col`
    /// back from the single resulting row.
    fn insert_param_and_read_back(create: &str, select_col: &str, bound: i64) -> Value {
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        conn.execute(create).expect("create table");

        let mut stmt = conn
            .prepare("INSERT INTO t VALUES (?)")
            .expect("prepare insert");
        stmt.bind_at(
            NonZero::new(1).expect("nonzero index"),
            Value::Integer(bound),
        );
        loop {
            match stmt.step().expect("insert step") {
                StepResult::Done => break,
                StepResult::IO => io.run_once().expect("insert io"),
                other => panic!("unexpected insert step result: {other:?}"),
            }
        }

        let mut q = conn
            .prepare(&format!("SELECT {select_col} FROM t"))
            .expect("prepare select");
        loop {
            match q.step().expect("select step") {
                StepResult::Row => {
                    return q.row().expect("row").get_value(0).clone();
                }
                StepResult::IO => io.run_once().expect("select io"),
                StepResult::Done => panic!("no row returned for SELECT {select_col}"),
                other => panic!("unexpected select step result: {other:?}"),
            }
        }
    }

    /// Helper to construct a `Column` for `column_is_nullable` unit tests.
    fn col(name: &str, primary_key: bool, is_rowid_alias: bool) -> Column {
        Column {
            name: Some(name.to_string()),
            ty: Type::Integer,
            ty_str: "INTEGER".to_string(),
            primary_key,
            is_rowid_alias,
            notnull: false,
            default: None,
            unique: false,
            unique_conflict: limbo_sqlite3_parser::ast::ResolveType::Abort,
            collation: None,
            is_generated: false,
        }
    }

    #[test]
    fn table_level_pk_param_not_null() {
        // The exact historically-failing case: a bound parameter against an
        // INTEGER NOT NULL column that is the table-level PRIMARY KEY must round
        // trip, NOT raise a spurious NOT NULL error nor become NULL.
        let got = insert_param_and_read_back(
            "CREATE TABLE t(a INTEGER NOT NULL, PRIMARY KEY(a))",
            "a",
            42,
        );
        assert_eq!(
            got,
            Value::Integer(42),
            "bound param must round-trip, got {got:?}"
        );
    }

    #[test]
    fn pk_column_before_constraint() {
        // Column declared before the table-level PRIMARY KEY clause.
        let got = insert_param_and_read_back(
            "CREATE TABLE t(a INTEGER NOT NULL, PRIMARY KEY(a))",
            "a",
            7,
        );
        assert_eq!(got, Value::Integer(7));
    }

    #[cfg(feature = "index_experimental")]
    #[test]
    fn pk_column_after_constraint() {
        // Order-independence: a non-rowid (TEXT) PK plus a trailing column. The
        // PK column `a` is bound via an explicit column list; the unmapped
        // nullable column `b` must default to NULL while `a` round-trips. This
        // also exercises the defense-in-depth path where `a` (PK) is unmapped vs
        // mapped depending on the column list.
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        conn.execute("CREATE TABLE t(a TEXT NOT NULL, b TEXT, PRIMARY KEY(a))")
            .expect("create table");

        let mut stmt = conn
            .prepare("INSERT INTO t(a) VALUES (?)")
            .expect("prepare insert");
        stmt.bind_at(NonZero::new(1).expect("idx"), Value::Integer(123));
        loop {
            match stmt.step().expect("insert step") {
                StepResult::Done => break,
                StepResult::IO => io.run_once().expect("insert io"),
                other => panic!("unexpected insert step: {other:?}"),
            }
        }

        let mut q = conn.prepare("SELECT a, b FROM t").expect("prepare select");
        loop {
            match q.step().expect("select step") {
                StepResult::Row => {
                    let row = q.row().expect("row");
                    assert_eq!(row.get_value(0).clone(), Value::Integer(123));
                    assert_eq!(row.get_value(1).clone(), Value::Null, "unmapped b is NULL");
                    return;
                }
                StepResult::IO => io.run_once().expect("select io"),
                StepResult::Done => panic!("no row returned"),
                other => panic!("unexpected select step: {other:?}"),
            }
        }
    }

    #[cfg(feature = "index_experimental")]
    #[test]
    fn composite_table_level_pk_params() {
        // Composite table-level PRIMARY KEY: both key columns receive bound
        // parameters and must round-trip (composite PK -> no rowid alias).
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        conn.execute("CREATE TABLE t(a INTEGER NOT NULL, b INTEGER NOT NULL, PRIMARY KEY(a, b))")
            .expect("create table");

        let mut stmt = conn
            .prepare("INSERT INTO t VALUES (?, ?)")
            .expect("prepare insert");
        stmt.bind_at(NonZero::new(1).expect("idx"), Value::Integer(10));
        stmt.bind_at(NonZero::new(2).expect("idx"), Value::Integer(20));
        loop {
            match stmt.step().expect("insert step") {
                StepResult::Done => break,
                StepResult::IO => io.run_once().expect("insert io"),
                other => panic!("unexpected insert step: {other:?}"),
            }
        }

        let mut q = conn.prepare("SELECT a, b FROM t").expect("prepare select");
        loop {
            match q.step().expect("select step") {
                StepResult::Row => {
                    let row = q.row().expect("row");
                    assert_eq!(row.get_value(0).clone(), Value::Integer(10));
                    assert_eq!(row.get_value(1).clone(), Value::Integer(20));
                    return;
                }
                StepResult::IO => io.run_once().expect("select io"),
                StepResult::Done => panic!("no row returned"),
                other => panic!("unexpected select step: {other:?}"),
            }
        }
    }

    #[test]
    fn column_level_int_pk_still_works() {
        // No regression: a column-level INTEGER PRIMARY KEY rowid alias still
        // accepts a bound parameter and stores it as the rowid value.
        let got = insert_param_and_read_back("CREATE TABLE t(a INTEGER PRIMARY KEY)", "a", 99);
        assert_eq!(got, Value::Integer(99));
    }

    #[test]
    fn real_not_null_violation_still_errors() {
        // The HaltIfNull relaxation must remain scoped to rowid-alias columns: a
        // genuine NOT NULL violation on an ordinary column must still error.
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        conn.execute("CREATE TABLE t(a INTEGER PRIMARY KEY, b TEXT NOT NULL)")
            .expect("create table");

        let mut stmt = conn
            .prepare("INSERT INTO t(a) VALUES (1)")
            .expect("prepare insert");
        let mut errored = false;
        loop {
            match stmt.step() {
                Ok(StepResult::Done) => break,
                Ok(StepResult::IO) => io.run_once().expect("io"),
                Ok(other) => panic!("unexpected step: {other:?}"),
                Err(_) => {
                    errored = true;
                    break;
                }
            }
        }
        assert!(
            errored,
            "NOT NULL violation on non-rowid column must still error"
        );
    }

    #[cfg(feature = "index_experimental")]
    #[test]
    fn unmapped_table_level_pk_column_is_not_silently_null() {
        // Defense-in-depth at the statement level: a table-level PRIMARY KEY
        // column omitted from the INSERT column list (and without DEFAULT) must
        // be rejected as non-nullable rather than silently populated with NULL.
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        // Composite PK avoids the rowid-alias special case; `a` is part of the
        // PK but omitted below.
        conn.execute("CREATE TABLE t(a TEXT, b TEXT, PRIMARY KEY(a, b))")
            .expect("create table");

        let result = conn.prepare("INSERT INTO t(b) VALUES ('x')");
        match result {
            // Rejected at translation time (preferred).
            Err(_) => {}
            // Or rejected at execution time.
            Ok(mut stmt) => {
                let mut errored = false;
                loop {
                    match stmt.step() {
                        Ok(StepResult::Done) => break,
                        Ok(StepResult::IO) => io.run_once().expect("io"),
                        Ok(other) => panic!("unexpected step: {other:?}"),
                        Err(_) => {
                            errored = true;
                            break;
                        }
                    }
                }
                assert!(
                    errored,
                    "omitting PK column `a` must not silently insert NULL"
                );
            }
        }
    }

    #[test]
    fn column_is_nullable_unit() {
        // Rowid alias is always nullable (rowid is autogenerated).
        assert!(column_is_nullable(&col("a", true, true), &[]));
        // Per-column PK flag set -> not nullable.
        assert!(!column_is_nullable(&col("a", true, false), &[]));
        // Non-PK column not in the table PK list -> nullable.
        assert!(column_is_nullable(&col("a", false, false), &[]));
        // Defense-in-depth: flag missing but name is in the table PK list
        // (case-insensitive) -> not nullable.
        assert!(!column_is_nullable(
            &col("a", false, false),
            &["a".to_string()]
        ));
        assert!(!column_is_nullable(
            &col("A", false, false),
            &["a".to_string()]
        ));
        assert!(!column_is_nullable(
            &col("a", false, false),
            &["A".to_string()]
        ));
    }
}

#[cfg(test)]
mod vtab_insert_parity_tests {
    //! Regression test for the multi-row `VALUES` bug in
    //! `translate_virtual_table_insert`: it used to build the row via
    //! `values.pop()`, which silently processes only the LAST row of a
    //! multi-row `VALUES` list and discards every earlier row without error
    //! -- `INSERT INTO vt VALUES (1,10),(2,20),(3,30)` inserted only
    //! `(3,30)`. It now emits one `VUpdate` per row (matching real SQLite's
    //! vtab INSERT codegen, and this crate's own BTree INSERT path's
    //! per-row `Insn::Insert`), so the virtual table's `xUpdate` callback
    //! must be invoked once per row, in order, with the correct values.
    //!
    //! There is no lightweight, pure-Rust way to stand up a *working*
    //! virtual table in this crate's test infrastructure (see the comment in
    //! `crates/oxisqlite-core/tests/alter_rename.rs`: instantiating one
    //! requires a module registered in `syms.vtab_modules`, which needs
    //! either dynamic (dlopen) extension loading or a hand-built module).
    //! This test builds a minimal `VTabModuleImpl` directly out of raw
    //! C-ABI function pointers -- the same shape a real (dynamically loaded)
    //! extension would provide -- and registers it into the connection's
    //! symbol table the way `ext::register_vtab_module` (the C-ABI entry
    //! point real extensions call through) would.

    use std::ffi::{c_char, c_void, CString};
    use std::rc::Rc;
    use std::sync::{Arc, Mutex};

    use limbo_ext::{
        Conn, ConstraintInfo, ExtIndexInfo, IndexInfo, OrderByInfo, ResultCode, VTabCreateResult,
        VTabKind, VTabModuleImpl, Value as ExtValue,
    };

    use crate::ext::VTabImpl;
    use crate::{Database, StepResult};

    /// Every row `xUpdate` (our `update` callback below) was called with, as
    /// `(col_a, col_b)` integers. Captured globally since the C-ABI callback
    /// is a plain `unsafe extern "C" fn` and can't close over test-local
    /// state.
    static UPDATE_CALLS: Mutex<Vec<(i64, i64)>> = Mutex::new(Vec::new());

    /// `cargo test`/`cargo nextest` run tests in this module concurrently on
    /// separate threads within the same process, but they all share the one
    /// process-wide `UPDATE_CALLS` static above. Each test still locks and
    /// clears it at the start (cheap, and makes each test's precondition
    /// explicit), but that alone isn't sufficient: without also serializing
    /// the tests against each other, one test's `xUpdate` calls can land in
    /// `UPDATE_CALLS` while a *different* test is mid-assertion. Every test
    /// in this module acquires this lock for its full body.
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    /// Acquire [`TEST_LOCK`], recovering from poisoning so one test's panic
    /// (leaving the mutex poisoned) doesn't spuriously fail every later test
    /// in this module.
    fn lock_tests() -> std::sync::MutexGuard<'static, ()> {
        TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    unsafe extern "C" fn vt_create(_args: *const ExtValue, _argc: i32) -> VTabCreateResult {
        let schema = CString::new("CREATE TABLE x(a, b)").expect("valid C string");
        VTabCreateResult {
            code: ResultCode::OK,
            // Ownership transfers to the core side, which frees it via
            // `CString::from_raw` in `VTabModuleImpl::create` -- see
            // oxisqlite-ext/src/vtabs.rs.
            schema: schema.into_raw(),
            table: std::ptr::null(),
        }
    }

    unsafe extern "C" fn vt_open(_table: *const c_void, _conn: *mut Conn) -> *const c_void {
        // No read path is exercised by this INSERT-only test; a non-null,
        // trivially droppable pointer is enough in case anything opens a
        // cursor defensively.
        Box::into_raw(Box::new(())) as *const c_void
    }

    unsafe extern "C" fn vt_close(cursor: *const c_void) -> ResultCode {
        if !cursor.is_null() {
            drop(unsafe { Box::from_raw(cursor as *mut ()) });
        }
        ResultCode::OK
    }

    unsafe extern "C" fn vt_filter(
        _cursor: *const c_void,
        _argc: i32,
        _argv: *const ExtValue,
        _idx_str: *const c_char,
        _idx_num: i32,
    ) -> ResultCode {
        ResultCode::OK
    }

    unsafe extern "C" fn vt_column(_cursor: *const c_void, _idx: u32) -> ExtValue {
        ExtValue::null()
    }

    unsafe extern "C" fn vt_next(_cursor: *const c_void) -> ResultCode {
        ResultCode::OK
    }

    unsafe extern "C" fn vt_eof(_cursor: *const c_void) -> bool {
        true
    }

    unsafe extern "C" fn vt_rowid(_cursor: *const c_void) -> i64 {
        0
    }

    /// The `xUpdate` callback under test. Per the `VUpdate` calling
    /// convention documented on `translate_virtual_table_insert`: `argv[0]`
    /// and `argv[1]` are NULL for an insert, `argv[2..]` are the new row's
    /// column values -- here exactly `(a, b)`.
    unsafe extern "C" fn vt_update(
        _table: *const c_void,
        argc: i32,
        argv: *const ExtValue,
        p_out_rowid: *mut i64,
    ) -> ResultCode {
        assert_eq!(argc, 4, "expected argc = 2 (insert markers) + 2 columns");
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        let a = args[2].to_integer().expect("column a must be an integer");
        let b = args[3].to_integer().expect("column b must be an integer");
        UPDATE_CALLS
            .lock()
            .expect("call log lock not poisoned")
            .push((a, b));
        if !p_out_rowid.is_null() {
            unsafe { *p_out_rowid = 0 };
        }
        ResultCode::OK
    }

    unsafe extern "C" fn vt_destroy(_table: *const c_void) -> ResultCode {
        ResultCode::OK
    }

    unsafe extern "C" fn vt_best_idx(
        _constraints: *const ConstraintInfo,
        _constraint_len: i32,
        _order_by: *const OrderByInfo,
        _order_by_len: i32,
    ) -> ExtIndexInfo {
        IndexInfo::default().to_ffi()
    }

    /// Run a statement via `prepare()` + a manual `step()` loop rather than
    /// `Connection::execute()`.
    ///
    /// This sidesteps a pre-existing, unrelated bug: `Connection::execute()`
    /// holds `self.syms.borrow()` alive across its *entire* step loop (see
    /// `Connection::execute` in lib.rs), which panics with "already
    /// borrowed" the instant a `CREATE VIRTUAL TABLE` statement reaches
    /// `op_vcreate` (`vdbe/execute/function.rs`), since that opcode itself
    /// needs `conn.syms.borrow_mut()` to register the newly created vtab
    /// instance. `Connection::prepare()` only borrows `syms` for the
    /// translate phase and releases it before returning, so stepping the
    /// resulting `Statement` afterward -- exactly what real callers
    /// (`rusqlite`-style prepare-then-step usage) do -- does not hit this.
    /// Out of scope to fix here (`lib.rs` is not part of this change); using
    /// `prepare()` + `step()` for every statement in this test module avoids
    /// it without masking the INSERT-path behavior under test.
    fn run_stmt(conn: &Arc<crate::Connection>, io: &Arc<dyn crate::IO>, sql: &str) {
        let mut stmt = conn.prepare(sql).expect("prepare statement");
        loop {
            match stmt.step().expect("step") {
                StepResult::Done => break,
                StepResult::IO => io.run_once().expect("io"),
                other => panic!("unexpected step result for {sql:?}: {other:?}"),
            }
        }
    }

    fn register_test_module(conn: &Arc<crate::Connection>, name: &str) {
        let c_name = CString::new(name).expect("valid module name").into_raw();
        let implementation = VTabModuleImpl {
            name: c_name as *const c_char,
            create: vt_create,
            open: vt_open,
            close: vt_close,
            filter: vt_filter,
            column: vt_column,
            next: vt_next,
            eof: vt_eof,
            update: vt_update,
            rowid: vt_rowid,
            destroy: vt_destroy,
            best_idx: vt_best_idx,
        };
        conn.syms.borrow_mut().vtab_modules.insert(
            name.to_string(),
            Rc::new(VTabImpl {
                module_kind: VTabKind::VirtualTable,
                implementation: Rc::new(implementation),
            }),
        );
    }

    #[test]
    fn multi_row_values_insert_reaches_virtual_table_for_every_row() {
        let _guard = lock_tests();
        UPDATE_CALLS
            .lock()
            .expect("call log lock not poisoned")
            .clear();

        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");

        register_test_module(&conn, "test_vtab_mod");

        run_stmt(&conn, &io, "CREATE VIRTUAL TABLE vt USING test_vtab_mod()");

        // Before the fix, `translate_virtual_table_insert` used `values.pop()`
        // and silently inserted only the LAST row -- `xUpdate` would have
        // been called once, with (3, 30). It must now be called once per row,
        // in row order.
        run_stmt(
            &conn,
            &io,
            "INSERT INTO vt VALUES (1, 10), (2, 20), (3, 30)",
        );

        let calls = UPDATE_CALLS
            .lock()
            .expect("call log lock not poisoned")
            .clone();
        assert_eq!(
            calls,
            vec![(1, 10), (2, 20), (3, 30)],
            "expected one xUpdate call per row, in row order, got {calls:?}"
        );
    }

    #[test]
    fn single_row_values_insert_still_calls_virtual_table_exactly_once() {
        // Regression guard for the common case: a single-row `VALUES` insert
        // (the case that always worked, even before the multi-row fix above,
        // since `values.pop()` on a one-element `Vec` returns that one row)
        // must still produce exactly one `xUpdate` call, with the right
        // values -- i.e. looping over `rows` in the fixed implementation
        // didn't change single-row behavior.
        let _guard = lock_tests();
        UPDATE_CALLS
            .lock()
            .expect("call log lock not poisoned")
            .clear();

        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");

        register_test_module(&conn, "test_vtab_mod_single");

        run_stmt(
            &conn,
            &io,
            "CREATE VIRTUAL TABLE vt2 USING test_vtab_mod_single()",
        );
        run_stmt(&conn, &io, "INSERT INTO vt2 VALUES (7, 42)");

        let calls = UPDATE_CALLS
            .lock()
            .expect("call log lock not poisoned")
            .clone();
        assert_eq!(
            calls,
            vec![(7, 42)],
            "single-row insert must produce exactly one xUpdate call with the right values, got {calls:?}"
        );
    }

    #[test]
    fn mismatched_row_arity_is_rejected_with_a_clear_error() {
        // Regression guard for the defensive per-row arity check added
        // alongside the multi-row fix: previously `values.pop()` only ever
        // looked at one row, so a `VALUES` list with inconsistent row
        // lengths could never reach `populate_column_registers` for a
        // mismatched row. Looping over every row makes that reachable in
        // principle, so `translate_virtual_table_insert` guards it
        // explicitly with a clear `bail_parse_error!` rather than risking
        // `populate_column_registers`'s internal
        // `.expect("value index out of bounds")`.
        //
        // In practice this crate's own parser already rejects a `VALUES`
        // list with inconsistent row lengths earlier, as a `LexerError`
        // ("all VALUES must have the same number of terms") before
        // translation is ever reached -- so this test observes that error,
        // not the translator's own guard. Asserting on the message rather
        // than the exact `LimboError` variant keeps the test meaningful
        // (mismatched arity is cleanly rejected, not a panic) without being
        // coupled to exactly which layer catches it.
        let _guard = lock_tests();

        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");

        register_test_module(&conn, "test_vtab_mod_mismatch");

        run_stmt(
            &conn,
            &io,
            "CREATE VIRTUAL TABLE vt3 USING test_vtab_mod_mismatch()",
        );

        // `Statement` isn't `Debug`, so match manually instead of
        // `.expect_err(...)` (which requires the `Ok` type to be `Debug`).
        match conn.prepare("INSERT INTO vt3 VALUES (1, 2), (3)") {
            Err(err) => {
                let message = err.to_string();
                assert!(
                    message.contains("same number of terms"),
                    "expected a clear 'same number of terms' rejection for mismatched VALUES arity, got: {message}"
                );
            }
            Ok(_) => panic!("mismatched row arity must be rejected, not accepted"),
        }
    }
}
