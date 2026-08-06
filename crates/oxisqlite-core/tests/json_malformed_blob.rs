//! Regression tests: a hand-crafted (i.e. attacker-supplied) JSONB **blob** must
//! never abort the process, however malformed it is.
//!
//! `json/mod.rs::convert_dbtype_to_jsonb` feeds any `Value::Blob` straight into
//! `Jsonb::from_raw_data` after only a *root-header* sanity check
//! (`Jsonb::is_valid`). Every nested element header inside the blob — including
//! its declared payload size, which is a full `u32` — is therefore fully
//! attacker-controlled, and the traversal/operation code used to slice and
//! `unwrap()` on it without bounds checks. A blob literal in ordinary SQL
//! (`SELECT json_extract(X'1bc7ff', '$[0]')`) is enough to reach it.
//!
//! Every statement below must produce either a row or a typed `Err`; a panic is
//! a test failure.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall io"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_json_malformed_{}_{}_{}.db",
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

/// Run a statement to completion. Returns `Ok(())` if it produced rows or
/// finished, `Err(message)` if the engine rejected it. Panicking is a failure.
fn try_run(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> Result<(), String> {
    let mut stmt = match conn.query(sql) {
        Ok(Some(stmt)) => stmt,
        Ok(None) => return Ok(()),
        Err(e) => return Err(format!("{e}")),
    };
    loop {
        match stmt.step() {
            Ok(StepResult::Row) => {}
            Ok(StepResult::Done) => return Ok(()),
            Ok(StepResult::IO) => io.run_once().map_err(|e| format!("{e}"))?,
            Ok(StepResult::Interrupt) | Ok(StepResult::Busy) => return Ok(()),
            Err(e) => return Err(format!("{e}")),
        }
    }
}

/// Hex blob literals whose nested element headers declare payload sizes far
/// beyond the blob itself, plus a few truncated/degenerate shapes.
const MALFORMED_JSONB_BLOBS: &[&str] = &[
    // ARRAY(size 1) -> TEXT with a 1-byte size marker declaring 255 bytes.
    "1bc7ff",
    // ARRAY(size 2) -> TEXT with a 2-byte size marker declaring 0xFFFF bytes.
    "2bd7ffff",
    // ARRAY(size 4) -> TEXT with a 4-byte size marker declaring ~4 GiB.
    "4be7ffffffff",
    // OBJECT(size 1) -> TEXT key with a 1-byte size marker declaring 255 bytes.
    "1cc7ff",
    // OBJECT(size 2) -> a key/value pair where the value size overflows.
    "2cc7ff",
    // ARRAY(size 3) -> nested ARRAY whose declared size exceeds the parent.
    "3bcbff",
    // ARRAY declaring a payload that the blob does not contain at all.
    "cbff0b",
    // Truncated multi-byte size headers.
    "eb",
    "db",
    "cb",
    // Header byte only.
    "0b",
    "0c",
];

/// Every JSON built-in that walks a path over the blob must survive.
#[test]
fn malformed_jsonb_blobs_never_panic() {
    let (io, conn, path) = open("blobs");

    for hex in MALFORMED_JSONB_BLOBS {
        for sql in [
            format!("SELECT json_extract(X'{hex}', '$[0]')"),
            format!("SELECT json_extract(X'{hex}', '$.a')"),
            format!("SELECT json_extract(X'{hex}', '$')"),
            format!("SELECT json(X'{hex}')"),
            format!("SELECT json_type(X'{hex}', '$[0]')"),
            format!("SELECT json_array_length(X'{hex}')"),
            format!("SELECT json_set(X'{hex}', '$[0]', 1)"),
            format!("SELECT json_insert(X'{hex}', '$[0]', 1)"),
            format!("SELECT json_replace(X'{hex}', '$[0]', 1)"),
            format!("SELECT json_remove(X'{hex}', '$[0]')"),
            format!("SELECT json_patch(X'{hex}', '{{\"a\":1}}')"),
            format!("SELECT jsonb_extract(X'{hex}', '$[0]')"),
            format!("SELECT json_insert(X'{hex}', '$.a[#]', 1)"),
            format!("SELECT json_set(X'{hex}', '$.a[0]', 1)"),
            format!("SELECT json_extract(X'{hex}', '$[0][1]')"),
            format!("SELECT json_extract(X'{hex}', '$.a[#-1]')"),
        ] {
            // Any outcome except a panic is acceptable.
            let _ = try_run(&io, &conn, &sql);
        }
    }

    // The connection is still healthy afterwards.
    assert!(try_run(&io, &conn, "SELECT json_extract('{\"a\":1}', '$.a')").is_ok());

    cleanup(&path);
}

/// Deterministic (seeded, dependency-free) sweep over many more blob shapes:
/// every possible root header byte, followed by a handful of pseudo-random
/// payload bytes. This is the shape a fuzzer would explore; running it as a
/// fixed corpus keeps it reproducible in CI.
#[test]
fn systematic_jsonb_header_sweep_never_panics() {
    let (io, conn, path) = open("sweep");

    // xorshift64* — deterministic, no dependencies.
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    for header in 0u16..=0xFF {
        for payload_len in [0usize, 1, 2, 3, 4, 5, 6, 8, 9, 12, 17] {
            let mut bytes = vec![header as u8];
            for _ in 0..payload_len {
                bytes.push((next() & 0xFF) as u8);
            }
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            for sql in [
                format!("SELECT json(X'{hex}')"),
                format!("SELECT json_array_length(X'{hex}', '$')"),
                format!("SELECT json_patch(X'{hex}', '{{\"a\":1}}')"),
                format!("SELECT json_patch('{{\"a\":1}}', X'{hex}')"),
                format!("SELECT json_quote(json(X'{hex}'))"),
            ] {
                let _ = try_run(&io, &conn, &sql);
            }
            // Path-shape matrix: `$`/`$[i]` hit
            // `SegmentVariant::Single(ArrayLocator)`, `$.a[...]` hits
            // `SegmentVariant::KeyWithArrayIndex`, and `$[#]` is the append
            // locator. Each traversal arm slices and splices at
            // header-declared, attacker-controlled offsets, so all of them
            // need coverage.
            for path in [
                "$", "$[0]", "$[1]", "$[#]", "$[#-1]", "$.a", "$.a.b", "$.a[#]", "$.a[0]",
                "$.a[#-1]", "$[0][1]", "$[0].a",
            ] {
                for sql in [
                    format!("SELECT json_extract(X'{hex}', '{path}')"),
                    format!("SELECT jsonb_extract(X'{hex}', '{path}')"),
                    format!("SELECT json_type(X'{hex}', '{path}')"),
                    format!("SELECT json_set(X'{hex}', '{path}', 1)"),
                    format!("SELECT json_insert(X'{hex}', '{path}', 1)"),
                    format!("SELECT json_replace(X'{hex}', '{path}', 1)"),
                    format!("SELECT json_remove(X'{hex}', '{path}')"),
                ] {
                    let _ = try_run(&io, &conn, &sql);
                }
            }
        }
    }

    assert!(try_run(&io, &conn, "SELECT json_extract('[1]', '$[0]')").is_ok());
    cleanup(&path);
}

/// Read a single-row, single-column text/None result as a debug string.
fn read_one(io: &Arc<dyn limbo_core::IO>, conn: &Arc<Connection>, sql: &str) -> String {
    let mut stmt = conn
        .query(sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement for {sql}"));
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                return match row.get_value(0) {
                    Value::Text(t) => t.as_str().to_string(),
                    other => format!("{other:?}"),
                };
            }
            StepResult::IO => io.run_once().expect("io"),
            StepResult::Done => return "<none>".to_string(),
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
}

/// `$[#]` is SQLite's append locator. Removing the `unreachable!()` that used
/// to abort on it means implementing real behaviour, so assert the *output*,
/// not merely the absence of a panic.
#[test]
fn append_locator_produces_correct_output() {
    let (io, conn, path) = open("append_locator");
    for (sql, expected) in [
        ("SELECT json_insert('[1,2]', '$[#]', 3)", "[1,2,3]"),
        ("SELECT json_set('[1,2]', '$[#]', 3)", "[1,2,3]"),
        ("SELECT json_insert('[]', '$[#]', 1)", "[1]"),
        ("SELECT json_set('[1,2,3]', '$[1]', 9)", "[1,9,3]"),
        ("SELECT json_extract('[1,2,3]', '$[#-1]')", "3"),
        ("SELECT json_remove('[1,2,3]', '$[0]')", "[2,3]"),
        // NOTE: an out-of-range explicit index (`json_insert('[1,2]', '$[5]', 9)`)
        // is a no-op here rather than an append, and the nested `$.a[#]` form (`SegmentVariant::KeyWithArrayIndex`)
        // silently drops the appended value in this engine
        // (`json_insert('{"a":[1,2]}', '$.a[#]', 3)` -> `{"a":[1,2]}`). That is
        // a pre-existing *wrong-output* gap in a code path untouched here, not a
        // panic, so it belongs to a later functional wave; it is deliberately
        // not asserted so this panic-hardening suite stays green and honest.
    ] {
        let got = read_one(&io, &conn, sql);
        assert!(
            got.contains(expected),
            "{sql} => {got}, expected to contain {expected}"
        );
    }
    cleanup(&path);
}

/// Well-formed JSON keeps working (the hardening must not over-reject).
#[test]
fn well_formed_json_still_works() {
    let (io, conn, path) = open("wellformed");

    for sql in [
        "SELECT json_extract('[1,2,3]', '$[1]')",
        "SELECT json_extract('{\"a\":{\"b\":7}}', '$.a.b')",
        "SELECT json_set('{\"a\":1}', '$.a', 2)",
        "SELECT json_insert('{\"a\":1}', '$.b', 2)",
        "SELECT json_replace('{\"a\":1}', '$.a', 3)",
        "SELECT json_remove('{\"a\":1,\"b\":2}', '$.b')",
        "SELECT json_array_length('[1,2,3]')",
        "SELECT json_type('{\"a\":1}', '$.a')",
        "SELECT json_extract(jsonb('[1,2,3]'), '$[2]')",
    ] {
        try_run(&io, &conn, sql).unwrap_or_else(|e| panic!("{sql} should succeed, got {e}"));
    }

    cleanup(&path);
}
