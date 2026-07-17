//! Integration tests for `PRAGMA auto_vacuum = FULL` root-page bookkeeping.
//!
//! `Pager::btree_create`'s `AutoVacuumMode::Full` arm computes a canonical
//! "next root page" slot (`vacuum_mode_largest_root_page + 1`, skipping
//! pointer-map pages) every time a table or index is created. This crate does
//! NOT implement full SQLite-style root-page *relocation*: when that
//! canonical slot is already occupied by an ordinary (non-root) page, the
//! allocator falls back to using whatever page it actually returns instead of
//! physically moving the occupant out of the way and fixing up every
//! reference to it. See the long comment in `btree_create` for why: this
//! crate's pointer-map bookkeeping is only ever populated for root pages
//! (nothing maintains ptrmap entries for ordinary b-tree/overflow pages as
//! they're created during inserts/splits), so there is no reliable "what
//! points at this arbitrary occupied page" information to relocate it
//! safely.
//!
//! What IS fixed (and covered here): `vacuum_mode_largest_root_page` used to
//! never advance past its initial value, so every CREATE TABLE/INDEX after
//! the first computed the SAME canonical slot -- already occupied by the
//! previous table's own root. That bookkeeping now correctly tracks the
//! highest root page actually used. Combined with the documented allocator
//! fallback, every table/index remains fully readable and writable,
//! `PRAGMA integrity_check` stays clean, and the schema's on-disk root-page
//! numbers are stable across a fresh reconnect -- across several tables and
//! enough intervening data to force the "canonical slot is already occupied"
//! fallback path to actually trigger.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_auto_vacuum_{}_{}_{}.db",
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

/// Step a single-column, single-row integer pragma/query and return the int.
fn read_int(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> i64 {
    let mut stmt = conn.query(sql).unwrap().expect("statement");
    loop {
        match stmt.step().unwrap() {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                return match row.get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer for {sql}, got {other:?}"),
                };
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => panic!("no row produced for {sql}"),
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
}

/// Step a single-column, single-row text pragma/query and return the string.
fn read_text(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> String {
    let mut stmt = conn.query(sql).unwrap().expect("statement");
    loop {
        match stmt.step().unwrap() {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                return match row.get_value(0) {
                    Value::Text(t) => t.as_str().to_string(),
                    other => panic!("expected text for {sql}, got {other:?}"),
                };
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => panic!("no row produced for {sql}"),
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
}

#[test]
fn auto_vacuum_full_root_pages_survive_intervening_writes_and_reconnect() {
    let path = temp_db_path("relocation");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();

    conn.execute("PRAGMA auto_vacuum = FULL").unwrap();
    assert_eq!(
        read_int(&io, &conn, "PRAGMA auto_vacuum"),
        1,
        "auto_vacuum should read back as FULL (1)"
    );

    // Table 1: created immediately after enabling auto_vacuum, so its root
    // lands exactly on the canonical slot (no intervening pages exist yet).
    conn.execute("CREATE TABLE t1(a INTEGER, b TEXT)").unwrap();
    for i in 0..200 {
        conn.execute(format!(
            "INSERT INTO t1 VALUES ({i}, '{}')",
            "x".repeat(200)
        ))
        .unwrap();
    }

    // Table 2: created only after t1 has grown across many b-tree/overflow
    // pages. Per the allocator's current (documented) limitation, the "next"
    // canonical root slot it computes here is almost certainly already
    // occupied by one of t1's own pages -- exercising the
    // `allocated_page_id != root_page_num` fallback path in `btree_create`,
    // not just the trivial "lands exactly on the canonical slot" case.
    //
    // (Deliberately no `CREATE INDEX` here: index creation in this engine is
    // gated behind the off-by-default `index_experimental` feature, which is
    // orthogonal to auto-vacuum; a fourth plain table below plays the same
    // "one more root page" role without pulling in that dependency.)
    conn.execute("CREATE TABLE t2(a INTEGER)").unwrap();
    for i in 0..50 {
        conn.execute(format!("INSERT INTO t2 VALUES ({i})"))
            .unwrap();
    }

    // A third table, to double-check the high-water mark keeps advancing
    // correctly rather than getting stuck after the first fallback.
    conn.execute("CREATE TABLE t3(a INTEGER)").unwrap();
    conn.execute("INSERT INTO t3 VALUES (42)").unwrap();

    // A fourth table, created last, for one more root-page data point.
    conn.execute("CREATE TABLE t4(a INTEGER)").unwrap();
    conn.execute("INSERT INTO t4 VALUES (7)").unwrap();

    assert_eq!(read_int(&io, &conn, "SELECT count(*) FROM t1"), 200);
    assert_eq!(read_int(&io, &conn, "SELECT count(*) FROM t2"), 50);
    assert_eq!(
        read_int(&io, &conn, "SELECT sum(a) FROM t2"),
        (0..50i64).sum::<i64>()
    );
    assert_eq!(read_int(&io, &conn, "SELECT a FROM t3"), 42);
    assert_eq!(read_int(&io, &conn, "SELECT a FROM t4"), 7);
    assert_eq!(
        read_text(&io, &conn, "PRAGMA integrity_check"),
        "ok",
        "integrity_check must be clean even though roots are not compact/lowest-numbered"
    );

    // Every table's root page, as recorded by this connection's schema.
    let root = |conn: &Arc<Connection>, io: &Arc<dyn limbo_core::IO>, name: &str| {
        read_int(
            io,
            conn,
            &format!("SELECT rootpage FROM sqlite_schema WHERE name = '{name}'"),
        )
    };
    let t1_root_before = root(&conn, &io, "t1");
    let t2_root_before = root(&conn, &io, "t2");
    let t3_root_before = root(&conn, &io, "t3");
    let t4_root_before = root(&conn, &io, "t4");

    for (name, page) in [
        ("t1", t1_root_before),
        ("t2", t2_root_before),
        ("t3", t3_root_before),
        ("t4", t4_root_before),
    ] {
        assert!(page > 0, "{name}'s root page must be a valid page number");
    }
    let mut roots = vec![
        t1_root_before,
        t2_root_before,
        t3_root_before,
        t4_root_before,
    ];
    let n_roots = roots.len();
    roots.sort_unstable();
    roots.dedup();
    assert_eq!(
        roots.len(),
        n_roots,
        "no two tables may share the same root page"
    );

    conn.checkpoint().unwrap();
    conn.close().unwrap();

    // Fresh connection: re-reads the schema (and thus every root page number)
    // from disk from scratch, with no state carried over in memory.
    let io2 = new_io();
    let db2 = Database::open_file(io2.clone(), path.to_str().unwrap(), false).unwrap();
    let conn2 = db2.connect().unwrap();

    assert_eq!(
        root(&conn2, &io2, "t1"),
        t1_root_before,
        "t1's root page must be stable across reconnect"
    );
    assert_eq!(
        root(&conn2, &io2, "t2"),
        t2_root_before,
        "t2's root page must be stable across reconnect"
    );
    assert_eq!(
        root(&conn2, &io2, "t3"),
        t3_root_before,
        "t3's root page must be stable across reconnect"
    );
    assert_eq!(
        root(&conn2, &io2, "t4"),
        t4_root_before,
        "t4's root page must be stable across reconnect"
    );

    assert_eq!(read_int(&io2, &conn2, "SELECT count(*) FROM t1"), 200);
    assert_eq!(read_int(&io2, &conn2, "SELECT count(*) FROM t2"), 50);
    assert_eq!(
        read_int(&io2, &conn2, "SELECT sum(a) FROM t2"),
        (0..50i64).sum::<i64>()
    );
    assert_eq!(read_int(&io2, &conn2, "SELECT a FROM t3"), 42);
    assert_eq!(read_int(&io2, &conn2, "SELECT a FROM t4"), 7);
    assert_eq!(
        read_text(&io2, &conn2, "PRAGMA integrity_check"),
        "ok",
        "integrity_check must still be clean after a fresh connection re-reads from disk"
    );

    cleanup(&path);
}
