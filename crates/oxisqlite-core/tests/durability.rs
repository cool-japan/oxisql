//! Integration tests for Slice A — durability & WAL lifecycle:
//! `PRAGMA synchronous`, WAL truncation on clean close / Drop / explicit
//! TRUNCATE checkpoint, and malformed-WAL error handling (no panics).

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

/// Size of the WAL header in bytes. A `-wal` file at or below this size carries
/// no frames (an "empty" WAL), which is the post-truncate steady state.
const WAL_HEADER_SIZE: u64 = 32;

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().unwrap())
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_durability_{}_{}_{}.db",
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

/// Step a single-column single-row integer pragma/select and return the int.
fn read_int(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> i64 {
    let mut stmt = conn.query(sql).unwrap().expect("statement");
    loop {
        match stmt.step().unwrap() {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                return match row.get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer, got {other:?}"),
                };
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => panic!("no row produced for {sql}"),
            other => panic!("unexpected step result: {other:?}"),
        }
    }
}

fn wal_len(path: &std::path::Path) -> Option<u64> {
    std::fs::metadata(format!("{}-wal", path.display()))
        .ok()
        .map(|m| m.len())
}

/// Returns the number of WAL frames carried by the `-wal` file at `path`.
///
/// A post-truncate WAL is "empty" (carries no frames): the engine resets the
/// file and rewrites only the 32-byte WAL header. On some platforms the header
/// is padded to a 512-byte block, so a raw byte-size check (`len <= 32`) is too
/// strict; the load-bearing property is that NO frame data exists beyond the
/// header. We treat any `-wal` that is at most a single 512-byte header block
/// AND has zero non-zero bytes after the 32-byte header as carrying no frames.
fn wal_frame_bytes_present(path: &std::path::Path) -> bool {
    let wal = format!("{}-wal", path.display());
    match std::fs::read(&wal) {
        Ok(bytes) => {
            // Beyond a single 512-byte header block we cannot fit a frame
            // (frame = 24-byte frame header + a >= 512-byte page), and within a
            // single block any non-zero byte after the 32-byte header would be
            // frame/garbage data. Either condition means frames may be present.
            if bytes.len() > 512 {
                return true;
            }
            bytes
                .get(WAL_HEADER_SIZE as usize..)
                .map(|tail| tail.iter().any(|b| *b != 0))
                .unwrap_or(false)
        }
        // Absent -wal -> definitely no frames.
        Err(_) => false,
    }
}

#[test]
fn pragma_synchronous_roundtrip() {
    let path = temp_db_path("sync_roundtrip");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();

    for (name, expected) in [("OFF", 0), ("NORMAL", 1), ("FULL", 2), ("EXTRA", 3)] {
        conn.execute(format!("PRAGMA synchronous = {name}"))
            .unwrap();
        let got = read_int(&io, &conn, "PRAGMA synchronous");
        assert_eq!(
            got, expected,
            "name form {name} should read back {expected}"
        );
    }
    // Numeric form.
    for n in [0_i64, 1, 2, 3] {
        conn.execute(format!("PRAGMA synchronous = {n}")).unwrap();
        let got = read_int(&io, &conn, "PRAGMA synchronous");
        assert_eq!(got, n, "numeric form {n} should read back {n}");
    }
    cleanup(&path);
}

#[test]
fn synchronous_off_still_durable_within_process() {
    let path = temp_db_path("sync_off");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();

    conn.execute("PRAGMA synchronous = OFF").unwrap();
    conn.execute("CREATE TABLE t(x)").unwrap();
    for i in 0..10 {
        conn.execute(format!("INSERT INTO t VALUES ({i})")).unwrap();
    }
    let count = read_int(&io, &conn, "SELECT count(*) FROM t");
    assert_eq!(
        count, 10,
        "rows visible within process even with synchronous=OFF"
    );

    // A checkpoint must still succeed in OFF mode (it just skips the fsyncs).
    conn.checkpoint().unwrap();
    let count_after = read_int(&io, &conn, "SELECT count(*) FROM t");
    assert_eq!(count_after, 10);
    cleanup(&path);
}

#[test]
fn wal_truncated_on_close() {
    let path = temp_db_path("close_truncate");
    cleanup(&path);
    let io = new_io();
    {
        let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE t(x)").unwrap();
        for i in 0..5 {
            conn.execute(format!("INSERT INTO t VALUES ({i})")).unwrap();
        }
        conn.close().unwrap();
    }
    // After a clean close the WAL is reset: either absent, or present but
    // carrying no frames (size <= 32-byte header). Both mean the `.db` is
    // self-contained for a byte-level reader.
    assert!(
        !wal_frame_bytes_present(&path),
        "post-close -wal should hold no frames, was {:?} bytes",
        wal_len(&path)
    );

    // A fresh reader must see all rows WITHOUT a manual checkpoint.
    let io2 = new_io();
    let db2 = Database::open_file(io2.clone(), path.to_str().unwrap(), false).unwrap();
    let conn2 = db2.connect().unwrap();
    let count = read_int(&io2, &conn2, "SELECT count(*) FROM t");
    assert_eq!(count, 5, "fresh reader sees all committed rows after close");
    cleanup(&path);
}

#[test]
fn wal_checkpoint_truncate_mode() {
    // SQL-level `PRAGMA wal_checkpoint(TRUNCATE)` routes through the VDBE which
    // currently always runs Passive (the op ignores the mode), so we drive
    // TRUNCATE via the pager-level `Connection::checkpoint_truncate()`.
    let path = temp_db_path("ckpt_truncate");
    cleanup(&path);
    let io = new_io();
    let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
    let conn = db.connect().unwrap();

    conn.execute("CREATE TABLE t(x)").unwrap();
    for i in 0..8 {
        conn.execute(format!("INSERT INTO t VALUES ({i})")).unwrap();
    }
    conn.checkpoint_truncate().unwrap();

    assert!(
        !wal_frame_bytes_present(&path),
        "post-TRUNCATE -wal should hold no frames, was {:?} bytes",
        wal_len(&path)
    );
    // Data still readable on the same connection after the WAL reset.
    let count = read_int(&io, &conn, "SELECT count(*) FROM t");
    assert_eq!(count, 8);
    cleanup(&path);
}

#[test]
fn drop_checkpoints_without_explicit_close() {
    let path = temp_db_path("drop_ckpt");
    cleanup(&path);
    let io = new_io();
    {
        let db = Database::open_file(io.clone(), path.to_str().unwrap(), false).unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE t(x)").unwrap();
        for i in 0..6 {
            conn.execute(format!("INSERT INTO t VALUES ({i})")).unwrap();
        }
        // No explicit close(); dropping `conn` (and `db`) must checkpoint+truncate.
    }
    assert!(
        !wal_frame_bytes_present(&path),
        "post-Drop -wal should hold no frames, was {:?} bytes",
        wal_len(&path)
    );
    let io2 = new_io();
    let db2 = Database::open_file(io2.clone(), path.to_str().unwrap(), false).unwrap();
    let conn2 = db2.connect().unwrap();
    let count = read_int(&io2, &conn2, "SELECT count(*) FROM t");
    assert_eq!(
        count, 6,
        "Drop ran the checkpoint; fresh reader sees all rows"
    );
    cleanup(&path);
}

#[test]
fn malformed_wal_returns_err_not_panic() {
    // Reachable malformed-WAL branches WITHOUT computing checksums:
    //   (a) truncated header (< 32 bytes) -> Corrupt "too small"
    //   (b) random 32-byte header -> almost surely fails the header checksum
    // For both, opening the DB (which runs WAL recovery via read_entire_wal_dumb)
    // must return Err and must NOT panic.
    let base = temp_db_path("malformed");
    cleanup(&base);

    // First, create a real, non-empty db file with a row, then close it so the
    // WAL is reset. The db FILE must be non-empty so reopen takes the recovery
    // branch (not the orphaned/fresh branch).
    {
        let io = new_io();
        let db = Database::open_file(io.clone(), base.to_str().unwrap(), false).unwrap();
        let conn = db.connect().unwrap();
        conn.execute("CREATE TABLE t(x)").unwrap();
        conn.execute("INSERT INTO t VALUES (1)").unwrap();
        conn.close().unwrap();
    }
    // Remove any leftover (empty) wal/shm so we control the -wal bytes exactly.
    let _ = std::fs::remove_file(format!("{}-wal", base.display()));
    let _ = std::fs::remove_file(format!("{}-shm", base.display()));

    let wal_path = format!("{}-wal", base.display());

    // (a) Truncated header: only 10 bytes. Wrap in catch_unwind to assert NO panic.
    std::fs::write(&wal_path, vec![0u8; 10]).unwrap();
    let res_a = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let io = new_io();
        Database::open_file(io.clone(), base.to_str().unwrap(), false)
            .and_then(|db| db.connect().map(|_| ()))
    }));
    assert!(res_a.is_ok(), "truncated-header WAL must not panic");
    assert!(
        res_a.unwrap().is_err(),
        "truncated-header WAL must surface an error"
    );
    let _ = std::fs::remove_file(&wal_path);
    let _ = std::fs::remove_file(format!("{}-shm", base.display()));

    // (b) 32-byte bogus header (non-zero magic, random bytes): the header
    // checksum almost certainly will not match -> Corrupt error, no panic.
    let mut bogus = vec![0u8; 32];
    // Set a plausible magic in the first 4 bytes and a page_size, leave the
    // stored checksums (bytes 24..32) as zero so they won't match the computed one.
    bogus[0] = 0x37;
    bogus[1] = 0x7f;
    bogus[2] = 0x06;
    bogus[3] = 0x82;
    // page_size = 4096 at bytes 8..12 (big-endian): 0x00001000
    bogus[8] = 0x00;
    bogus[9] = 0x00;
    bogus[10] = 0x10;
    bogus[11] = 0x00;
    std::fs::write(&wal_path, &bogus).unwrap();
    let res_b = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let io = new_io();
        Database::open_file(io.clone(), base.to_str().unwrap(), false)
            .and_then(|db| db.connect().map(|_| ()))
    }));
    assert!(res_b.is_ok(), "bogus-checksum WAL must not panic");
    assert!(
        res_b.unwrap().is_err(),
        "bogus-checksum WAL must surface an error"
    );

    cleanup(&base);
}
