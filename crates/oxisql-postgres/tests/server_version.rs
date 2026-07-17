//! End-to-end tests for `PgConnection::server_version()` against a scripted
//! fake PostgreSQL server on a real loopback TCP socket.
//!
//! `PgConnection::connect` delegates the wire-level startup handshake
//! entirely to `tokio_postgres::connect`, which dials a real
//! `tokio::net::TcpStream` rather than accepting an arbitrary
//! `AsyncRead + AsyncWrite` stream. That means — unlike the hand-rolled
//! replication handshake helpers in `src/replication/auth.rs`, which are
//! generic over the stream type and so can be driven over an in-memory
//! `tokio::io::duplex` pipe — it cannot be redirected to anything but a real
//! socket. This mirrors exactly the reasoning documented on
//! `src/replication/stream.rs`'s
//! `start_logical_replication_end_to_end_over_loopback_tcp` test, so this
//! file follows the same fix: bind a real `TcpListener` on `127.0.0.1:0` and
//! script a minimal fake server on the accepted connection.
//!
//! The scripted `ParameterStatus(server_version)` message below is the exact
//! message type `PgConnection::connect` reads via
//! `tokio_postgres::Connection::parameter` before spawning the connection
//! driver away — see `connection.rs`'s `server_version` field doc comment.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use oxisql_postgres::{PgConnection, TlsMode};

// ── Wire-format helpers (hand-rolled; mirrors the identically-named helper
// in `src/replication/auth.rs`'s test module, minus the `bytes` dependency —
// this crate's `bytes` dependency is not exposed to integration tests, and a
// plain `Vec<u8>` is simple enough here) ────────────────────────────────────

/// Encode one length-prefixed backend-style message: `tag`, then a
/// big-endian `u32` length (covering itself and `body`), then `body`.
fn encode_message(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(5 + body.len());
    buf.push(tag);
    let len = u32::try_from(body.len() + 4).expect("test message body fits in u32");
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(body);
    buf
}

/// Read and discard a `StartupMessage`: a big-endian `u32` length (covering
/// itself), followed by that many further bytes (protocol version plus
/// NUL-terminated key/value parameters and a final NUL). Unlike every other
/// frontend message, `StartupMessage` has no leading tag byte — it is sent
/// before the wire protocol has negotiated tagged-message framing.
async fn read_startup_message(stream: &mut tokio::net::TcpStream) {
    let mut len_buf = [0_u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .expect("read StartupMessage length");
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut rest = vec![0_u8; len - 4];
    stream
        .read_exact(&mut rest)
        .await
        .expect("read StartupMessage body");
}

/// The `server_version` value the fake server reports; asserted verbatim
/// against `PgConnection::server_version()`.
const FAKE_SERVER_VERSION: &str = "16.4 (Fake OxiSQL Test Server)";

/// Bind a loopback listener and spawn a scripted fake PostgreSQL server that
/// performs just enough of the startup handshake — `AuthenticationOk` (PG
/// "trust" auth: no password requested), a `server_version`
/// `ParameterStatus`, `BackendKeyData`, then `ReadyForQuery` — for
/// `tokio_postgres::connect` to succeed.
///
/// `TlsMode::Disabled` (`tokio_postgres::NoTls`) never triggers an
/// `SSLRequest` in the first place: `NoTls::can_connect()` returns `false`,
/// and `tokio-postgres`'s default `sslmode` is `"prefer"` — its own
/// `connect_tls` logic skips TLS negotiation whenever the configured
/// connector cannot connect under `"prefer"`, going straight to
/// `StartupMessage`. So that is always the first thing this fake server
/// reads.
///
/// Returns the server task's `JoinHandle` (await it after the client-side
/// script completes, to propagate any assertion failure inside the fake
/// server rather than silently dropping it) and the port it is bound to.
async fn spawn_fake_server() -> (tokio::task::JoinHandle<()>, u16) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback listener");
    let port = listener.local_addr().expect("read local_addr").port();

    let handle = tokio::spawn(async move {
        let (mut server, _peer) = listener.accept().await.expect("accept");

        read_startup_message(&mut server).await;

        // AuthenticationOk — a 4-byte big-endian auth-type code of 0.
        server
            .write_all(&encode_message(b'R', &0_i32.to_be_bytes()))
            .await
            .expect("write AuthenticationOk");

        // ParameterStatus(server_version) — the message `PgConnection::connect`
        // reads before handing the connection driver off to be spawned.
        let mut ps_body = Vec::new();
        ps_body.extend_from_slice(b"server_version\0");
        ps_body.extend_from_slice(FAKE_SERVER_VERSION.as_bytes());
        ps_body.push(0);
        server
            .write_all(&encode_message(b'S', &ps_body))
            .await
            .expect("write ParameterStatus(server_version)");

        // BackendKeyData — process ID and secret key, 4 bytes each.
        let mut key_body = Vec::new();
        key_body.extend_from_slice(&4242_i32.to_be_bytes());
        key_body.extend_from_slice(&9999_i32.to_be_bytes());
        server
            .write_all(&encode_message(b'K', &key_body))
            .await
            .expect("write BackendKeyData");

        // ReadyForQuery(Idle) — marks the end of the startup handshake.
        server
            .write_all(&encode_message(b'Z', b"I"))
            .await
            .expect("write ReadyForQuery");
    });

    (handle, port)
}

/// `PgConnection::connect` must capture `server_version` from the handshake
/// and expose it via `server_version()`.
#[tokio::test]
async fn server_version_is_populated_from_parameter_status_handshake() {
    let (server_task, port) = spawn_fake_server().await;
    let conn_str = format!("host=127.0.0.1 port={port} user=oxisql_test dbname=oxisql_test");

    let client_script = async {
        let conn = PgConnection::connect(&conn_str, TlsMode::Disabled)
            .await
            .expect("connect to fake server");
        assert_eq!(conn.server_version(), Some(FAKE_SERVER_VERSION));
    };
    tokio::time::timeout(Duration::from_secs(10), client_script)
        .await
        .expect("client script timed out");

    server_task.await.expect("server task panicked");
}

/// `PgConnection::from_client` has no `tokio_postgres::Connection` driver
/// available (only the caller-supplied `Client`), so `server_version()` must
/// gracefully report `None` rather than panicking or fabricating a value.
#[tokio::test]
async fn server_version_is_none_for_from_client_connections() {
    let (server_task, port) = spawn_fake_server().await;
    let conn_str = format!("host=127.0.0.1 port={port} user=oxisql_test dbname=oxisql_test");

    let client_script = async {
        let (client, connection) = tokio_postgres::connect(&conn_str, tokio_postgres::NoTls)
            .await
            .expect("tokio_postgres::connect to fake server");
        // Drive the connection in the background, same as any real caller
        // would have to — `from_client` assumes this has already happened.
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let conn = PgConnection::from_client(client);
        assert_eq!(conn.server_version(), None);
    };
    tokio::time::timeout(Duration::from_secs(10), client_script)
        .await
        .expect("client script timed out");

    server_task.await.expect("server task panicked");
}
