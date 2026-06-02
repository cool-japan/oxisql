//! Integration tests for the sled-backed persistent embedded connection.
//!
//! These tests are only compiled when the `sled-storage` feature is enabled.

#![cfg(feature = "sled-storage")]

use oxisql_core::Connection;
use oxisql_embedded::SledEmbeddedConnection;

/// Generate a unique temporary directory path for a test.
fn temp_db_dir(suffix: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("oxisql_sled_test_{suffix}_{nanos}"))
}

/// Clean up a temporary database directory, ignoring errors.
fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn sled_open_creates_db() {
    let dir = temp_db_dir("open");
    let conn = SledEmbeddedConnection::open(&dir).expect("open should succeed");
    conn.ping().await.expect("ping should succeed");
    cleanup(&dir);
}

#[tokio::test]
async fn sled_create_table_and_insert() {
    let dir = temp_db_dir("insert");
    let conn = SledEmbeddedConnection::open(&dir).expect("open should succeed");

    conn.execute("CREATE TABLE items (id INT, name TEXT)", &[])
        .await
        .expect("CREATE TABLE should succeed");

    let affected = conn
        .execute("INSERT INTO items VALUES (1, 'hello')", &[])
        .await
        .expect("INSERT should succeed");
    assert_eq!(affected, 1, "INSERT should affect 1 row");

    cleanup(&dir);
}

#[tokio::test]
async fn sled_select_after_insert() {
    let dir = temp_db_dir("select");
    let conn = SledEmbeddedConnection::open(&dir).expect("open should succeed");

    conn.execute("CREATE TABLE kv (k INT, v TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO kv VALUES (42, 'world')", &[])
        .await
        .expect("INSERT");

    let rows = conn
        .query("SELECT k, v FROM kv", &[])
        .await
        .expect("SELECT should succeed");
    assert_eq!(rows.len(), 1, "should return one row");
    assert_eq!(
        rows[0].get_by_index(0).map(|v| v.to_string()),
        Some("42".to_owned())
    );
    assert_eq!(
        rows[0].get_by_index(1).map(|v| v.to_string()),
        Some("world".to_owned())
    );

    cleanup(&dir);
}

#[tokio::test]
async fn sled_open_write_read_persist() {
    let dir = temp_db_dir("persist");

    // Phase 1 — write data.
    {
        let conn = SledEmbeddedConnection::open(&dir).expect("open phase-1");
        conn.execute("CREATE TABLE persist_test (id INT, val TEXT)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO persist_test VALUES (1, 'persistent')", &[])
            .await
            .expect("INSERT");
        // conn is dropped here, flushing sled.
    }

    // Phase 2 — reopen and verify data survived.
    {
        let conn = SledEmbeddedConnection::open(&dir).expect("open phase-2");
        let rows = conn
            .query("SELECT id, val FROM persist_test", &[])
            .await
            .expect("SELECT after reopen");
        assert_eq!(rows.len(), 1, "row should survive process restart");
        assert_eq!(
            rows[0].get_by_index(0).map(|v| v.to_string()),
            Some("1".to_owned())
        );
        assert_eq!(
            rows[0].get_by_index(1).map(|v| v.to_string()),
            Some("persistent".to_owned())
        );
    }

    cleanup(&dir);
}

#[tokio::test]
async fn sled_delete_data() {
    let dir = temp_db_dir("delete");
    let conn = SledEmbeddedConnection::open(&dir).expect("open");

    conn.execute("CREATE TABLE del_test (id INT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO del_test VALUES (10)", &[])
        .await
        .expect("INSERT");
    conn.execute("INSERT INTO del_test VALUES (20)", &[])
        .await
        .expect("INSERT");

    let before = conn
        .query("SELECT id FROM del_test", &[])
        .await
        .expect("SELECT before delete");
    assert_eq!(before.len(), 2);

    conn.execute("DELETE FROM del_test WHERE id = 10", &[])
        .await
        .expect("DELETE");

    let after = conn
        .query("SELECT id FROM del_test", &[])
        .await
        .expect("SELECT after delete");
    assert_eq!(after.len(), 1);
    assert_eq!(
        after[0].get_by_index(0).map(|v| v.to_string()),
        Some("20".to_owned())
    );

    cleanup(&dir);
}

#[tokio::test]
async fn sled_execute_batch() {
    let dir = temp_db_dir("batch");
    let conn = SledEmbeddedConnection::open(&dir).expect("open");

    conn.execute_batch(
        "CREATE TABLE batch_t (x INT); INSERT INTO batch_t VALUES (1); INSERT INTO batch_t VALUES (2);",
    )
    .await
    .expect("execute_batch");

    let rows = conn
        .query("SELECT x FROM batch_t", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 2, "batch should insert 2 rows");

    cleanup(&dir);
}

#[tokio::test]
async fn sled_prepare_and_execute() {
    let dir = temp_db_dir("prepared");
    let conn = SledEmbeddedConnection::open(&dir).expect("open");

    conn.execute("CREATE TABLE prep_t (id INT, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    // Prepared statement via prepare().
    {
        let mut stmt = conn
            .prepare("INSERT INTO prep_t VALUES (1, 'alice')")
            .await
            .expect("prepare INSERT");
        stmt.execute(&[]).await.expect("execute prepared INSERT");
    }

    let rows = conn
        .query("SELECT id, name FROM prep_t", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);

    cleanup(&dir);
}
