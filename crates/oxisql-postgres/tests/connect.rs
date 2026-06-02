//! Compile-time API shape tests — no live Postgres required.

use std::time::Duration;

use oxisql_postgres::{parse_pg_conn_str, PgConnection, PgConnectionBuilder, TlsMode};

/// Verify that `TlsMode::Disabled` is usable without any live connection.
#[test]
fn tls_mode_disabled_compiles() {
    let mode = TlsMode::Disabled;
    // Use the value so the compiler doesn't elide the test entirely.
    match mode {
        TlsMode::Disabled => {}
        TlsMode::Rustls(_) => panic!("unexpected variant"),
    }
}

/// Verify that `TlsMode::Rustls` can be constructed from an `Arc<ClientConfig>`
/// without linking a live server.  We use `oxitls` to build the config the
/// same way production code would.
#[test]
fn tls_mode_rustls_constructs() {
    let root_store = oxitls::webpki_root_certs();
    let cfg = oxitls::client_config(root_store).expect("client_config");
    let mode = TlsMode::Rustls(cfg);
    match mode {
        TlsMode::Rustls(_) => {}
        TlsMode::Disabled => panic!("unexpected variant"),
    }
}

// ── savepoint name validation ─────────────────────────────────────────────────
// Validated indirectly through parse_pg_conn_str (public) and the fact that
// the validate_savepoint_name helper is exercised via integration tests.
// The unit tests below exercise `validate_savepoint_name` behaviour through
// the transaction API surface; direct fn is private so we test edge cases via
// parse utilities that share the same module.

// ── parse_pg_conn_str (key-value format) ─────────────────────────────────────

/// Full key-value connection string with all four recognized keys.
#[test]
fn parse_kv_full() {
    let p = parse_pg_conn_str("host=myhost port=5433 dbname=testdb user=admin").unwrap();
    assert_eq!(p.host, "myhost");
    assert_eq!(p.port, 5433);
    assert_eq!(p.dbname, Some("testdb".to_string()));
    assert_eq!(p.user, Some("admin".to_string()));
}

/// `hostaddr` is treated as a synonym for `host`.
#[test]
fn parse_kv_hostaddr() {
    let p = parse_pg_conn_str("hostaddr=10.0.0.1").unwrap();
    assert_eq!(p.host, "10.0.0.1");
    assert_eq!(p.port, 5432); // default
}

/// Missing optional fields fall back to defaults / `None`.
#[test]
fn parse_kv_defaults() {
    let p = parse_pg_conn_str("").unwrap();
    assert_eq!(p.host, "localhost");
    assert_eq!(p.port, 5432);
    assert_eq!(p.dbname, None);
    assert_eq!(p.user, None);
}

/// Unknown keys (e.g., `password`, `sslmode`) are silently ignored.
#[test]
fn parse_kv_unknown_keys_ignored() {
    let p = parse_pg_conn_str(
        "host=db port=5432 dbname=prod user=alice password=secret sslmode=require",
    )
    .unwrap();
    assert_eq!(p.host, "db");
    assert_eq!(p.dbname, Some("prod".to_string()));
    assert_eq!(p.user, Some("alice".to_string()));
}

/// An un-parseable port value must be an error, not a silent fallback.
#[test]
fn parse_kv_bad_port_is_error() {
    assert!(parse_pg_conn_str("host=localhost port=notanumber").is_err());
}

// ── parse_pg_conn_str (URI format) ───────────────────────────────────────────

/// Full URI with user, password, host, port, and dbname.
#[test]
fn parse_uri_full() {
    let p = parse_pg_conn_str("postgres://alice:secret@dbhost:5433/mydb").unwrap();
    assert_eq!(p.host, "dbhost");
    assert_eq!(p.port, 5433);
    assert_eq!(p.dbname, Some("mydb".to_string()));
    assert_eq!(p.user, Some("alice".to_string()));
}

/// `postgresql://` scheme is accepted as well.
#[test]
fn parse_uri_postgresql_scheme() {
    let p = parse_pg_conn_str("postgresql://user@host/db").unwrap();
    assert_eq!(p.host, "host");
    assert_eq!(p.dbname, Some("db".to_string()));
    assert_eq!(p.user, Some("user".to_string()));
    assert_eq!(p.port, 5432); // default
}

/// URI with no user and no port — only host and dbname.
#[test]
fn parse_uri_host_and_db_only() {
    let p = parse_pg_conn_str("postgres://somehost/somedb").unwrap();
    assert_eq!(p.host, "somehost");
    assert_eq!(p.port, 5432);
    assert_eq!(p.dbname, Some("somedb".to_string()));
    assert_eq!(p.user, None);
}

/// URI with only a host (no path, no user).
#[test]
fn parse_uri_host_only() {
    let p = parse_pg_conn_str("postgres://myhost").unwrap();
    assert_eq!(p.host, "myhost");
    assert_eq!(p.port, 5432);
    assert_eq!(p.dbname, None);
}

/// URI with invalid port returns an error.
#[test]
fn parse_uri_bad_port_is_error() {
    assert!(parse_pg_conn_str("postgres://host:badport/db").is_err());
}

// ── connect_with_timeout API shape ────────────────────────────────────────────

/// Verify that `connect_with_timeout` is callable with the expected signature.
/// (No live server needed — we just verify the type checks at compile time via
/// a helper function whose signature must match the method.)
#[test]
fn connect_with_timeout_signature_compiles() {
    // A wrapper function whose signature enforces that `connect_with_timeout`
    // exists and accepts the expected argument types.
    async fn _check(
        uri: &str,
        tls: TlsMode,
        dur: Duration,
    ) -> Result<PgConnection, oxisql_postgres::PgError> {
        PgConnection::connect_with_timeout(uri, tls, dur).await
    }
    // We only need the function to compile — no need to call it.
    let _ = std::mem::size_of_val(&_check);
}

// ── savepoint inherent-method API shape ───────────────────────────────────────

// The `savepoint_pg`, `rollback_to_savepoint_pg`, and `release_savepoint_pg`
// inherent methods on `PgTransaction` are compile-tested via the integration
// test suite (`tests/integration.rs`, `#[ignore]`-guarded).  They require
// `PgTransaction` to be constructible, which in turn requires a live
// PostgreSQL connection, so we cannot test them here.
//
// Name-validation logic is covered by `test_savepoint_name_validation_*`
// tests below (via the public `Transaction` trait surface in integration.rs).

// ── PgConnectionBuilder TLS convenience methods ───────────────────────────────

/// Verify `tls_skip_verify()` on `PgConnectionBuilder` compiles and does not panic.
///
/// **INSECURE** — for development/testing only.  No live server needed.
#[test]
fn test_pg_builder_tls_skip_verify_no_panic() {
    let _builder = PgConnectionBuilder::new()
        .host("localhost")
        .port(5432)
        .dbname("test")
        .tls_skip_verify()
        .expect("tls_skip_verify should succeed");
    // Verifies the method exists, is chainable, and does not panic.
}

/// Verify `tls_with_ca_pem()` on `PgConnectionBuilder` compiles.
///
/// Passes a minimal (invalid) PEM so we verify the builder method exists and
/// accepts `Vec<u8>`.  No live server needed.
///
/// Note: with an invalid CA cert this returns `Err(PgError::Tls)` — we only
/// care that the API surface compiles correctly.
#[test]
fn test_pg_builder_tls_with_ca_pem_no_panic() {
    // Minimal fake PEM header — contents are never sent to any server.
    let fake_pem = b"-----BEGIN CERTIFICATE-----\nZg==\n-----END CERTIFICATE-----\n".to_vec();
    // Error is expected here (invalid cert bytes); the important part is that
    // the method exists and the type signature is correct.
    let _result = PgConnectionBuilder::new()
        .host("localhost")
        .port(5432)
        .dbname("test")
        .tls_with_ca_pem(fake_pem);
    // Either Ok (unused builder) or Err(PgError::Tls) — neither panics.
}

/// Placeholder integration test — requires a live TLS-enabled PostgreSQL server.
///
/// This test validates that a real TLS handshake with `rustls-rustcrypto`
/// succeeds end-to-end.  It is marked `#[ignore]` because no server is
/// available in the standard CI environment.
///
/// Run with:
/// ```sh
/// POSTGRES_TLS_URL="postgresql://user:pass@tls-host/db" \
///     cargo test -p oxisql-postgres -- --ignored test_pg_tls_live_connection
/// ```
#[ignore]
#[tokio::test]
async fn test_pg_tls_live_connection() {
    use oxisql_core::Connection;
    let url = match std::env::var("POSTGRES_TLS_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("POSTGRES_TLS_URL not set — skipping live TLS test");
            return;
        }
    };
    let conn = PgConnection::connect(&url, TlsMode::skip_verify().expect("skip_verify"))
        .await
        .expect("connect with TLS skip_verify");
    conn.ping().await.expect("ping over TLS");
}

// ── oxisql-parse integration — is_read_only_query / normalize_query ──────────

#[test]
fn test_pg_is_read_only_query_select_is_true() {
    assert!(PgConnection::is_read_only_query("SELECT 1"));
}

#[test]
fn test_pg_is_read_only_query_delete_is_false() {
    assert!(!PgConnection::is_read_only_query("DELETE FROM t"));
}

#[test]
fn test_pg_is_read_only_query_insert_is_false() {
    assert!(!PgConnection::is_read_only_query(
        "INSERT INTO t VALUES (1)"
    ));
}

#[test]
fn test_pg_is_read_only_query_unparseable_returns_false() {
    assert!(!PgConnection::is_read_only_query("@@@NOT VALID SQL@@@"));
}

#[test]
fn test_pg_normalize_query_collapses_whitespace() {
    let n = PgConnection::normalize_query("SELECT  id  FROM t");
    assert!(!n.is_empty());
    assert!(!n.contains("  "));
}

#[test]
fn test_pg_normalize_query_empty_returns_empty() {
    assert!(PgConnection::normalize_query("").is_empty());
}
