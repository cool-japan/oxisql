//! Live-connection tests for `BackendInfo::from_postgres_connection` and
//! `BackendInfo::from_mysql_connection` — the facade's connection-scoped
//! `BackendInfo` constructors, as opposed to the static, connectionless
//! `BackendInfo::postgres()` / `BackendInfo::mysql()` dispatchers exercised
//! in `tests/connect.rs`.
//!
//! **Postgres**: reuses the fake-server-over-loopback-TCP technique from
//! `oxisql-postgres/tests/server_version.rs` (see that file's module doc for
//! why a real `TcpListener` is required here rather than `tokio::io::duplex`
//! — `tokio_postgres::connect` dials a real socket), scripting just enough of
//! the startup handshake for a real `oxisql::postgres::PgConnection::connect`
//! to succeed, then asserting `from_postgres_connection` reports the
//! scripted version verbatim.
//!
//! **MySQL**: `oxisql-mysql` has no wire-protocol handshake of its own to
//! script (it delegates entirely to `mysql_async`), so — mirroring
//! `oxisql-mysql/tests/connect.rs`'s own
//! `server_version_fails_gracefully_without_server` test — this file only
//! verifies the no-server path degrades gracefully to `version: None` rather
//! than panicking or propagating an error; the `Some(...)`-returning happy
//! path needs a real server and is left to live-server integration testing.

#[cfg(feature = "postgres")]
mod postgres_live {
    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use oxisql::postgres::{PgConnection, TlsMode};

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

    /// Read and discard a `StartupMessage` (length-prefixed, no tag byte —
    /// see `oxisql-postgres/tests/server_version.rs` for the full rationale).
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
    /// against `BackendInfo::from_postgres_connection(&conn).version`.
    const FAKE_SERVER_VERSION: &str = "15.7 (Fake OxiSQL Facade Test Server)";

    /// Bind a loopback listener and spawn a scripted fake PostgreSQL server —
    /// `AuthenticationOk`, a `server_version` `ParameterStatus`,
    /// `BackendKeyData`, then `ReadyForQuery` — sufficient for
    /// `PgConnection::connect` to succeed. See
    /// `oxisql-postgres/tests/server_version.rs` for why `TlsMode::Disabled`
    /// never triggers an `SSLRequest` here.
    async fn spawn_fake_server() -> (tokio::task::JoinHandle<()>, u16) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let port = listener.local_addr().expect("read local_addr").port();

        let handle = tokio::spawn(async move {
            let (mut server, _peer) = listener.accept().await.expect("accept");

            read_startup_message(&mut server).await;

            server
                .write_all(&encode_message(b'R', &0_i32.to_be_bytes()))
                .await
                .expect("write AuthenticationOk");

            let mut ps_body = Vec::new();
            ps_body.extend_from_slice(b"server_version\0");
            ps_body.extend_from_slice(FAKE_SERVER_VERSION.as_bytes());
            ps_body.push(0);
            server
                .write_all(&encode_message(b'S', &ps_body))
                .await
                .expect("write ParameterStatus(server_version)");

            let mut key_body = Vec::new();
            key_body.extend_from_slice(&4242_i32.to_be_bytes());
            key_body.extend_from_slice(&9999_i32.to_be_bytes());
            server
                .write_all(&encode_message(b'K', &key_body))
                .await
                .expect("write BackendKeyData");

            server
                .write_all(&encode_message(b'Z', b"I"))
                .await
                .expect("write ReadyForQuery");
        });

        (handle, port)
    }

    /// `oxisql::BackendInfo::from_postgres_connection` must populate
    /// `version` from a live connection's handshake — unlike the static,
    /// connectionless `BackendInfo::postgres()`, which always leaves it
    /// `None` (covered by `test_backend_info_postgres` in `tests/connect.rs`).
    #[tokio::test]
    async fn from_postgres_connection_populates_version() {
        let (server_task, port) = spawn_fake_server().await;
        let conn_str = format!("host=127.0.0.1 port={port} user=oxisql_test dbname=oxisql_test");

        let client_script = async {
            let conn = PgConnection::connect(&conn_str, TlsMode::Disabled)
                .await
                .expect("connect to fake server");

            let info = oxisql::BackendInfo::from_postgres_connection(&conn);
            assert_eq!(info.name, "postgres");
            assert_eq!(info.version.as_deref(), Some(FAKE_SERVER_VERSION));
            assert!(info.features.contains(&"tls"));
        };
        tokio::time::timeout(Duration::from_secs(10), client_script)
            .await
            .expect("client script timed out");

        server_task.await.expect("server task panicked");
    }
}

#[cfg(feature = "mysql")]
mod mysql_live {
    use oxisql::mysql::{MyConnection, TlsMode};

    /// `oxisql::BackendInfo::from_mysql_connection` must degrade gracefully
    /// (`version: None`, no panic) when the pool cannot reach a server,
    /// mirroring `oxisql-mysql`'s own
    /// `server_version_fails_gracefully_without_server` test. No live server
    /// needed — see this file's module doc comment for why the `Some(...)`
    /// happy path is not covered here.
    #[tokio::test]
    async fn from_mysql_connection_degrades_gracefully_without_server() {
        let conn = MyConnection::connect("mysql://root@127.0.0.1:19999/test", TlsMode::Disabled)
            .await
            .expect("connect is lazy — pool construction alone should not fail");

        let info = oxisql::BackendInfo::from_mysql_connection(&conn).await;
        assert_eq!(info.name, "mysql");
        assert_eq!(info.version, None);
        assert!(info.features.contains(&"tls"));
    }
}
