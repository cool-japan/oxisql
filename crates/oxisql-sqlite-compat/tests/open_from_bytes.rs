//! Integration tests for [`SqliteConnection::open_from_bytes`].
//!
//! The database image under test is produced by the engine itself into a file
//! under [`std::env::temp_dir`] (no checked-in binary fixtures), read back as
//! bytes, and then reopened purely from memory. Query results from the
//! byte-image connection are asserted to match what was written to the file.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

/// A unique database path under the OS temp dir; removes the file and its
/// `-wal` sidecar on drop.
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
            "oxisql_ofb_{}_{}_{}.db",
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

/// Create a populated database on disk (with an overflow-sized row and an
/// index), checkpoint it into the main file, and return its raw bytes.
async fn build_image_bytes(temp: &TempDbPath, big: &str) -> Vec<u8> {
    {
        let conn = SqliteConnection::open(temp.as_str())
            .await
            .expect("open temp db");
        conn.execute(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL, body TEXT)",
            &[],
        )
        .await
        .expect("create table");
        conn.execute("CREATE INDEX idx_name ON t (name)", &[])
            .await
            .expect("create index");
        conn.execute(
            "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
            &[&1i64, &"alice", &big],
        )
        .await
        .expect("insert overflow row");
        conn.execute(
            "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
            &[&2i64, &"bob", &"short"],
        )
        .await
        .expect("insert small row");
        // Fold the WAL into the main database file so the on-disk bytes are a
        // complete, self-contained image. Only Passive checkpoint mode is
        // implemented in the engine today; the clean close below performs the
        // authoritative checkpoint-on-close that writes every frame into the
        // main `.db` file and truncates the WAL.
        conn.execute("PRAGMA wal_checkpoint", &[])
            .await
            .expect("checkpoint");
    } // drop closes the connection: checkpoint-on-close finalizes the image.

    std::fs::read(temp.as_str()).expect("read database file bytes")
}

#[tokio::test]
async fn test_open_from_bytes_matches_file_results() {
    let temp = TempDbPath::new("match");
    let big = "z".repeat(10_000); // larger than the 4096 page size -> overflow.
    let bytes = build_image_bytes(&temp, &big).await;

    let conn = SqliteConnection::open_from_bytes(&bytes)
        .await
        .expect("open_from_bytes");

    // Row count.
    let rows = conn.query("SELECT count(*) FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(2)));

    // Overflow payload round-trips byte-for-byte.
    let rows = conn
        .query("SELECT body FROM t WHERE id = $1", &[&1i64])
        .await
        .unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text(big.clone())));

    // Index-ordered read.
    let rows = conn
        .query("SELECT name FROM t ORDER BY name", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text("alice".into())));
    assert_eq!(rows[1].get_by_index(0), Some(&Value::Text("bob".into())));
}

#[tokio::test]
async fn test_open_from_bytes_write_after_open() {
    let temp = TempDbPath::new("write_after");
    let big = "q".repeat(8_000);
    let bytes = build_image_bytes(&temp, &big).await;

    let conn = SqliteConnection::open_from_bytes(&bytes)
        .await
        .expect("open_from_bytes");

    // The in-memory image is writable; new writes are visible on this
    // connection but must not touch the source byte slice.
    conn.execute(
        "INSERT INTO t (id, name, body) VALUES ($1, $2, $3)",
        &[&3i64, &"carol", &"new"],
    )
    .await
    .expect("insert after open");

    let rows = conn.query("SELECT count(*) FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(3)));

    // The original byte slice is unchanged (it is only borrowed by value copy).
    let reopened = SqliteConnection::open_from_bytes(&bytes)
        .await
        .expect("reopen original bytes");
    let rows = reopened.query("SELECT count(*) FROM t", &[]).await.unwrap();
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::I64(2)),
        "a second open of the same bytes must not see the post-open insert"
    );
}

#[tokio::test]
async fn test_open_from_bytes_empty_is_err() {
    let err = SqliteConnection::open_from_bytes(&[]).await;
    assert!(err.is_err(), "empty bytes must error, not panic");
}

#[tokio::test]
async fn test_open_from_bytes_garbage_is_err() {
    let garbage = vec![0xABu8; 4096];
    let err = SqliteConnection::open_from_bytes(&garbage).await;
    assert!(err.is_err(), "garbage bytes must error, not panic");
}

#[tokio::test]
async fn test_open_from_bytes_truncated_is_err() {
    let temp = TempDbPath::new("trunc");
    let bytes = build_image_bytes(&temp, "small").await;
    // Keep only the first 50 bytes: shorter than the 100-byte header.
    let truncated = &bytes[..50];
    let err = SqliteConnection::open_from_bytes(truncated).await;
    assert!(err.is_err(), "truncated header must error, not panic");
}
