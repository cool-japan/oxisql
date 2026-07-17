//! Blocking-wrapper integration tests for
//! [`SqliteConnectionBlocking::open_from_bytes`].
//!
//! Compiled only with `--features blocking`. The image is produced by the
//! engine into a file under [`std::env::temp_dir`] (no checked-in fixtures),
//! read back as bytes, and reopened purely from memory via the synchronous
//! wrapper.
#![cfg(feature = "blocking")]

use oxisql_core::Value;
use oxisql_sqlite_compat::blocking::SqliteConnectionBlocking;

struct TempDbPath {
    path: std::path::PathBuf,
}

impl TempDbPath {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "oxisql_ofb_blk_{}_{}_{}.db",
            tag,
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        Self { path }
    }

    fn as_str(&self) -> &str {
        self.path.to_str().expect("temp db path is valid UTF-8")
    }
}

impl Drop for TempDbPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(format!("{}-wal", self.path.display()));
    }
}

fn build_image_bytes(temp: &TempDbPath, big: &str) -> Vec<u8> {
    {
        let conn = SqliteConnectionBlocking::open(temp.as_str()).expect("open temp db");
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, body TEXT)",
            &[],
        )
        .expect("create table");
        conn.execute("CREATE INDEX idx_name ON t (name)", &[])
            .expect("create index");
        conn.execute(
            "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
            &[&1i64, &"alice", &big],
        )
        .expect("insert overflow row");
        conn.execute(
            "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
            &[&2i64, &"bob", &"short"],
        )
        .expect("insert small row");
        conn.execute("PRAGMA wal_checkpoint", &[])
            .expect("checkpoint");
    } // drop closes the connection: checkpoint-on-close finalizes the image.

    std::fs::read(temp.as_str()).expect("read database file bytes")
}

#[test]
fn test_blocking_open_from_bytes_matches_file_results() {
    let temp = TempDbPath::new("match");
    let big = "z".repeat(10_000); // larger than the 4096 page size -> overflow.
    let bytes = build_image_bytes(&temp, &big);

    let conn = SqliteConnectionBlocking::open_from_bytes(&bytes).expect("open_from_bytes");

    let rows = conn.query("SELECT count(*) FROM t", &[]).unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(2)));

    let rows = conn
        .query("SELECT body FROM t WHERE id = $1", &[&1i64])
        .unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text(big.clone())));

    let rows = conn.query("SELECT name FROM t ORDER BY name", &[]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text("alice".into())));
    assert_eq!(rows[1].get_by_index(0), Some(&Value::Text("bob".into())));
}

#[test]
fn test_blocking_open_from_bytes_write_after_open() {
    let temp = TempDbPath::new("write_after");
    let bytes = build_image_bytes(&temp, "small");

    let conn = SqliteConnectionBlocking::open_from_bytes(&bytes).expect("open_from_bytes");
    conn.execute(
        "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
        &[&3i64, &"carol", &"new"],
    )
    .expect("insert after open");

    let rows = conn.query("SELECT count(*) FROM t", &[]).unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(3)));
}

#[test]
fn test_blocking_open_from_bytes_empty_is_err() {
    assert!(
        SqliteConnectionBlocking::open_from_bytes(&[]).is_err(),
        "empty bytes must error, not panic"
    );
}

#[test]
fn test_blocking_open_from_bytes_garbage_is_err() {
    let garbage = vec![0xABu8; 4096];
    assert!(
        SqliteConnectionBlocking::open_from_bytes(&garbage).is_err(),
        "garbage bytes must error, not panic"
    );
}
