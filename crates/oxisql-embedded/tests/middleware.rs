use std::sync::Arc;

use oxisql_core::middleware::{ConnectionMetrics, LoggingConnection, MetricsConnection};
use oxisql_core::Connection;
use oxisql_embedded::EmbeddedConnection;

#[tokio::test]
async fn test_metrics_connection_counts_executes() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let metrics = Arc::new(ConnectionMetrics::default());
    let conn = MetricsConnection::new(inner, Arc::clone(&metrics));

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO t VALUES (1)", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO t VALUES (2)", &[])
        .await
        .expect("INSERT 2");

    let snap = metrics.snapshot();
    assert_eq!(snap.executes, 3);
    assert_eq!(snap.errors, 0);
    assert!(snap.execute_us > 0);
}

#[tokio::test]
async fn test_metrics_connection_counts_queries() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let metrics = Arc::new(ConnectionMetrics::default());
    let conn = MetricsConnection::new(inner, Arc::clone(&metrics));

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO t VALUES (42)", &[])
        .await
        .expect("INSERT 42");

    let _ = conn.query("SELECT id FROM t", &[]).await.expect("SELECT 1");
    let _ = conn
        .query("SELECT id FROM t WHERE id = 42", &[])
        .await
        .expect("SELECT 2");

    let snap = metrics.snapshot();
    assert_eq!(snap.queries, 2);
    assert!(snap.query_us > 0);
}

#[tokio::test]
async fn test_logging_connection_wraps_correctly() {
    // Verify it works functionally (log output not captured in unit tests)
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let conn = LoggingConnection::with_prefix(inner, "test");

    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO t VALUES (99)", &[])
        .await
        .expect("INSERT 99");

    let rows = conn.query("SELECT v FROM t", &[]).await.expect("SELECT");
    assert_eq!(rows.len(), 1);

    assert_eq!(conn.prefix(), "test");
}

#[tokio::test]
async fn test_metrics_error_counting() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let metrics = Arc::new(ConnectionMetrics::default());
    let conn = MetricsConnection::new(inner, Arc::clone(&metrics));

    // Valid execute
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    // Invalid execute (table does not exist)
    let _ = conn
        .execute("INSERT INTO nonexistent_table_xyz VALUES (1)", &[])
        .await;

    let snap = metrics.snapshot();
    // First execute succeeded, second should have errored
    assert_eq!(snap.executes, 2);
    assert_eq!(snap.errors, 1);
}

#[tokio::test]
async fn test_metrics_into_inner() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let metrics = Arc::new(ConnectionMetrics::default());
    let conn = MetricsConnection::new(inner, Arc::clone(&metrics));

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    // Recover the inner connection
    let recovered = conn.into_inner();
    let rows = recovered
        .query("SELECT id FROM t", &[])
        .await
        .expect("SELECT from recovered conn");
    assert_eq!(rows.len(), 0);
}

#[tokio::test]
async fn test_logging_into_inner() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory");
    let conn = LoggingConnection::new(inner);

    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    let recovered = conn.into_inner();
    let rows = recovered
        .query("SELECT id FROM t", &[])
        .await
        .expect("SELECT from recovered conn");
    assert_eq!(rows.len(), 0);
}
