//! Integration tests for PostgreSQL LISTEN/NOTIFY support.
//!
//! These tests require a live PostgreSQL server.  They are gated with
//! `#[ignore]` so they are skipped in CI unless explicitly opted in with
//! `cargo test -- --ignored`.
//!
//! To run:
//! ```text
//! cargo test -p oxisql-postgres --test notify -- --ignored
//! ```
//!
//! The tests connect to `host=localhost port=5432 user=postgres`.
//! Adjust `PG_CONN_STR` below if your server uses different credentials.

use std::time::Duration;

use oxisql_postgres::{PgConnection, TlsMode};

const PG_CONN_STR: &str = "host=localhost port=5432 user=postgres";

/// Two-connection test: listen on one, notify from the other.
///
/// Verifies that:
/// - `listen()` successfully registers the channel.
/// - `notify()` from a second connection delivers the message.
/// - `recv_timeout()` receives the notification with the correct channel and
///   payload.
/// - `unlisten()` successfully deregisters the channel without error.
#[tokio::test]
#[ignore = "requires a live PostgreSQL server"]
async fn test_listen_notify() {
    let conn1 = PgConnection::connect(PG_CONN_STR, TlsMode::Disabled)
        .await
        .expect("conn1 connect");
    let conn2 = PgConnection::connect(PG_CONN_STR, TlsMode::Disabled)
        .await
        .expect("conn2 connect");

    let channel = "oxisql_test_ch";
    let payload = "hello from oxisql";

    // Subscribe on conn1.
    let mut stream = conn1.listen(channel).await.expect("listen");

    // Give the server a moment to register the LISTEN.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Send notification from conn2.
    conn2.notify(channel, payload).await.expect("notify");

    // Wait up to 2 seconds for the notification to arrive.
    let notif = stream
        .recv_timeout(Duration::from_secs(2))
        .await
        .expect("expected notification, got None");

    assert_eq!(notif.channel, channel);
    assert_eq!(notif.payload, payload);

    // Unsubscribe — should not error.
    stream.unlisten().await.expect("unlisten");
}

/// Verify that `listen()` returns an error for connections created via
/// `from_client` (which have no notification channel).
#[tokio::test]
#[ignore = "requires a live PostgreSQL server"]
async fn test_listen_from_client_fails() {
    let (client, connection) = tokio_postgres::connect(PG_CONN_STR, tokio_postgres::NoTls)
        .await
        .expect("connect raw");
    tokio::spawn(async move {
        let _: Result<_, _> = connection.await;
    });

    let conn = PgConnection::from_client(client);
    let result = conn.listen("some_channel").await;
    assert!(
        result.is_err(),
        "from_client connection should not support listen"
    );
}

/// Verify that invalid channel names are rejected before any server call.
#[tokio::test]
#[ignore = "requires a live PostgreSQL server"]
async fn test_listen_invalid_channel() {
    let conn = PgConnection::connect(PG_CONN_STR, TlsMode::Disabled)
        .await
        .expect("connect");

    // Space in channel name.
    let result = conn.listen("bad channel").await;
    assert!(result.is_err(), "channel with space should fail");

    // Empty channel name.
    let result = conn.listen("").await;
    assert!(result.is_err(), "empty channel should fail");

    // SQL injection attempt.
    let result = conn.listen("ch; DROP TABLE users; --").await;
    assert!(result.is_err(), "SQL injection channel should fail");
}

/// Verify `notify()` validates the channel name.
#[tokio::test]
#[ignore = "requires a live PostgreSQL server"]
async fn test_notify_invalid_channel() {
    let conn = PgConnection::connect(PG_CONN_STR, TlsMode::Disabled)
        .await
        .expect("connect");

    let result = conn.notify("bad channel!", "payload").await;
    assert!(result.is_err(), "invalid channel should be rejected");
}
