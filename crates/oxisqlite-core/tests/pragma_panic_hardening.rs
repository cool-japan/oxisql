//! Regression tests for two process-aborting panics reachable from ordinary,
//! valid SQL issued over the public `Connection` API:
//!
//!   * `PRAGMA page_size = N` used to hit an unconditional
//!     `todo!("updating page_size is not yet implemented")` in
//!     `translate::pragma::update_pragma`. Because the pragma dispatcher only
//!     routes `table_info`/`foreign_key_list` to the read path, *every* other
//!     pragma with an `= value` body lands in `update_pragma`, so
//!     `PRAGMA page_size = 4096` — which countless clients and ORMs emit
//!     unconditionally at connection open — aborted the host process at
//!     statement-translation time.
//!
//!   * `PRAGMA auto_vacuum = incremental` was accepted, persisted into the
//!     database header and armed `AutoVacuumMode::Incremental` on the pager.
//!     The *next* root-page allocation (`Pager::btree_create`, i.e. the next
//!     `CREATE TABLE` / `CREATE INDEX`) then reached a bare
//!     `unimplemented!()` and aborted the process. Because the mode was
//!     persisted, the abort recurred on every reopen of the file.
//!
//! Both are now panic-free paths: `page_size` follows SQLite's
//! "validate, then defer to VACUUM" no-op semantics (it never raises), and
//! incremental auto-vacuum is rejected with a typed error *before* anything is
//! mutated, with the pager arm hardened as a second line of defence.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall io"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_pragma_hardening_{}_{}_{}.db",
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

/// Step a single-column, single-row query and return the integer in column 0.
fn read_int(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> i64 {
    let mut stmt = conn
        .query(sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement produced for {sql}"));
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                return match row.get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer for {sql}, got {other:?}"),
                };
            }
            StepResult::IO => io.run_once().expect("io"),
            StepResult::Done => panic!("no row produced for {sql}"),
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
}

/// `PRAGMA page_size = N` must never panic and — matching SQLite — must never
/// raise, whatever the argument looks like.
#[test]
fn pragma_page_size_assignment_never_panics_and_never_errors() {
    let (io, conn, path) = open("page_size");

    let original = read_int(&io, &conn, "PRAGMA page_size");
    assert!(
        (512..=65536).contains(&original),
        "page_size should read back in the legal range, got {original}"
    );

    // The canonical client-handshake form: a legal power-of-two page size.
    // This is the exact statement that used to abort the process.
    conn.execute("PRAGMA page_size = 4096")
        .expect("PRAGMA page_size = 4096 must not error");

    // Non-power-of-two, out-of-range, zero, negative and non-numeric values are
    // all silently ignored by SQLite; none of them may panic or raise here.
    for stmt in [
        "PRAGMA page_size = 1234",
        "PRAGMA page_size = 0",
        "PRAGMA page_size = 511",
        "PRAGMA page_size = 131072",
        "PRAGMA page_size = -4096",
        "PRAGMA page_size = 'nonsense'",
    ] {
        conn.execute(stmt)
            .unwrap_or_else(|e| panic!("{stmt} must not error, got {e:?}"));
    }

    // The database is still fully usable, and the effective page size is
    // unchanged (the request is deferred, exactly like SQLite defers it to the
    // next VACUUM on a database that already has content).
    assert_eq!(
        read_int(&io, &conn, "PRAGMA page_size"),
        original,
        "page size must not change under the deferred (no-op) implementation"
    );
    exec(&conn, "CREATE TABLE t(a INTEGER, b TEXT)");
    exec(&conn, "INSERT INTO t VALUES (1, 'one')");
    assert_eq!(read_int(&io, &conn, "SELECT count(*) FROM t"), 1);

    cleanup(&path);
}

/// `PRAGMA auto_vacuum = incremental` (and its `2` spelling) must be rejected
/// with a typed error, must not be persisted, and must not arm a mode that
/// aborts the process on the next `CREATE TABLE`.
#[test]
fn pragma_auto_vacuum_incremental_is_rejected_without_panicking() {
    let (io, conn, path) = open("incremental");

    let err = conn
        .execute("PRAGMA auto_vacuum = incremental")
        .expect_err("incremental auto-vacuum is not implemented and must be rejected");
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("incremental"),
        "error should name the unsupported mode, got: {msg}"
    );

    // `= 2` is the numeric spelling of the same mode.
    assert!(
        conn.execute("PRAGMA auto_vacuum = 2").is_err(),
        "numeric incremental auto-vacuum must be rejected too"
    );

    // Nothing was armed on the pager and nothing was persisted to the header,
    // so the next root-page allocation must succeed rather than abort.
    assert_eq!(
        read_int(&io, &conn, "PRAGMA auto_vacuum"),
        0,
        "a rejected auto_vacuum request must leave the mode untouched"
    );
    exec(&conn, "CREATE TABLE t(a INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (7)");
    assert_eq!(read_int(&io, &conn, "SELECT count(*) FROM t"), 1);

    // ... and the rejection survives a reconnect (i.e. mode 2 never reached
    // the on-disk header).
    drop(conn);
    let db = Database::open_file(io.clone(), path.to_str().expect("utf-8 temp path"), false)
        .expect("reopen database");
    let conn = db.connect().expect("reconnect");
    assert_eq!(read_int(&io, &conn, "PRAGMA auto_vacuum"), 0);
    exec(&conn, "CREATE TABLE t2(a INTEGER)");

    cleanup(&path);
}

/// The supported auto-vacuum modes keep working: rejecting `incremental` must
/// not regress `none` / `full`.
#[test]
fn pragma_auto_vacuum_none_and_full_still_work() {
    let (io, conn, path) = open("supported_modes");

    exec(&conn, "PRAGMA auto_vacuum = none");
    assert_eq!(read_int(&io, &conn, "PRAGMA auto_vacuum"), 0);

    exec(&conn, "PRAGMA auto_vacuum = full");
    assert_eq!(read_int(&io, &conn, "PRAGMA auto_vacuum"), 1);
    exec(&conn, "CREATE TABLE t(a INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (1)");
    assert_eq!(read_int(&io, &conn, "SELECT count(*) FROM t"), 1);

    cleanup(&path);
}
