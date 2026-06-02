use oxisql_core::middleware::{RetryConnection, RetryPolicy};
use oxisql_core::Connection;
use oxisql_embedded::EmbeddedConnection;

#[tokio::test]
async fn retry_connection_succeeds_immediately() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory should succeed");
    let conn = RetryConnection::new(inner, RetryPolicy::default());
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE should succeed");
    let rows = conn
        .query("SELECT id FROM t", &[])
        .await
        .expect("SELECT should succeed on empty table");
    assert!(rows.is_empty(), "freshly created table should have no rows");
}

#[tokio::test]
async fn retry_connection_wraps_and_unwraps() {
    let inner = EmbeddedConnection::open_memory().expect("open_memory should succeed");
    let conn = RetryConnection::new(inner, RetryPolicy::default());

    // Verify inner() returns a reference without consuming the wrapper.
    let _ = conn.inner();

    // into_inner() should give back the raw EmbeddedConnection.
    let inner = conn.into_inner();
    // The recovered connection should still be functional.
    inner
        .execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE on recovered inner should succeed");
}

#[tokio::test]
async fn retry_policy_debug_format() {
    let policy = RetryPolicy {
        max_retries: 2,
        initial_delay_ms: 1,
        backoff_factor: 2.0,
        max_delay_ms: 100,
        ..RetryPolicy::default()
    };
    // Verify Debug impl doesn't panic and includes key fields.
    let s = format!("{policy:?}");
    assert!(
        s.contains("RetryPolicy"),
        "debug string should name the struct"
    );
    assert!(
        s.contains("max_retries"),
        "debug string should include max_retries"
    );
}

#[tokio::test]
async fn retry_connection_non_transient_error_not_retried() {
    use oxisql_core::OxiSqlError;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // Build a policy whose predicate always returns false (non-transient).
    let call_count = Arc::new(AtomicU32::new(0));
    let call_count_clone = Arc::clone(&call_count);

    let policy = RetryPolicy {
        max_retries: 3,
        initial_delay_ms: 1,
        backoff_factor: 2.0,
        max_delay_ms: 10,
        predicate: Arc::new(move |_e: &OxiSqlError| {
            call_count_clone.fetch_add(1, Ordering::Relaxed);
            false // never retry
        }),
    };

    let inner = EmbeddedConnection::open_memory().expect("open_memory should succeed");
    let conn = RetryConnection::new(inner, policy);

    // This query against a nonexistent table should fail with a non-transient error.
    let result = conn.query("SELECT id FROM nonexistent_xyz", &[]).await;
    assert!(
        result.is_err(),
        "query on nonexistent table should return Err"
    );
    // Predicate was called once (for the first failure), did not retry.
    assert_eq!(
        call_count.load(Ordering::Relaxed),
        1,
        "predicate should be called exactly once for a non-transient error"
    );
}

#[tokio::test]
async fn retry_tables_delegates_to_inner() {
    // EmbeddedConnection now implements tables() via GlueSQL fetch_all_schemas.
    // Verify that RetryConnection faithfully delegates to the inner connection
    // and returns the correct result.
    let inner = EmbeddedConnection::open_memory().expect("open_memory should succeed");
    let conn = RetryConnection::new(inner, RetryPolicy::default());

    conn.execute("CREATE TABLE some_table (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE should succeed");

    // tables() on embedded now returns Ok with the list of tables.
    let result = conn.tables().await;
    assert!(
        result.is_ok(),
        "tables() on embedded should return Ok, got: {:?}",
        result
    );
    let tables = result.unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "some_table");
}
