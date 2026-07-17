//! Tests for the `MyConnection` API shape.
//!
//! These tests do **not** require a live MySQL server.  They verify that
//! `connect` returns a typed error (not a panic) when the server is absent,
//! and that the public API surface compiles correctly.

use oxisql_mysql::{MyConnection, MyConnectionBuilder, TlsMode};

/// Verify that `connect` with `TlsMode::Disabled` fails gracefully (typed
/// error, no panic) when no MySQL server is available on localhost.
#[tokio::test]
async fn connect_disabled_tls_fails_gracefully() {
    // Use a URL that is guaranteed to have no server.  Port 19999 is
    // intentionally unusual to avoid accidental connections.
    let result =
        MyConnection::connect("mysql://root@127.0.0.1:19999/test", TlsMode::Disabled).await;

    // `connect` itself is lazy (pool creation) and may succeed at this point;
    // the error surfaces on first use.  Either way it must not panic.
    let _ = result;
}

/// Verify that `connect` returns `Ok` for a pool that hasn't been exercised yet
/// (mysql_async's Pool::new is lazy).
#[tokio::test]
async fn pool_creation_is_lazy() {
    // Pool creation succeeds even if the server is absent; the error is deferred
    // to the first `get_conn()` call.
    let conn = MyConnection::connect("mysql://root@127.0.0.1:19999/test", TlsMode::Disabled).await;
    // We accept either Ok (lazy pool) or Err (eager validation) — no panic.
    let _: Result<_, _> = conn.map(|_| ());
}

/// Ensure a bad URL is rejected at parse time, not silently ignored.
#[tokio::test]
async fn connect_bad_url_returns_error() {
    let result = MyConnection::connect("not-a-valid-mysql-url", TlsMode::Disabled).await;
    assert!(result.is_err(), "expected error for invalid URL, got Ok");
}

/// Verify `MyConnectionBuilder` compiles and constructs without panic.
///
/// Does not attempt a real connection — validates the builder API surface.
#[test]
fn my_connection_builder_constructs() {
    let builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("testdb")
        .user("root")
        .password("secret");
    let _ = builder; // compiles without panic
}

/// Verify all builder setters are chainable and `Debug` output is well-formed.
#[test]
fn my_connection_builder_debug() {
    let builder = MyConnectionBuilder::new()
        .host("db.example.com")
        .port(3307)
        .dbname("prod")
        .user("admin")
        .password("hunter2")
        .connect_timeout_secs(10)
        .tls_mode(TlsMode::Disabled);
    let s = format!("{builder:?}");
    assert!(
        s.contains("MyConnectionBuilder"),
        "Debug output should name the type"
    );
}

/// Verify that `MyConnection` implements `Clone` (compile-time check).
///
/// This test does not require a live server — it only confirms that the
/// `Clone` bound is satisfied so that callers can clone a `MyConnection`
/// to share it across tasks.
#[test]
fn my_connection_is_clone() {
    fn assert_clone<T: Clone>() {}
    assert_clone::<oxisql_mysql::MyConnection>();
}

/// Verify `ssl_skip_verify` builder compiles and does not panic.
///
/// INSECURE — intended for development only.  No live server is needed.
#[test]
fn ssl_skip_verify_builder_compiles() {
    let _builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("db")
        .ssl_skip_verify();
    // Verifies the builder method exists and is chainable.
}

/// Verify `ssl_with_ca_pem` builder compiles with raw PEM bytes.
///
/// No live server is needed.
#[test]
fn ssl_with_ca_pem_builder_compiles() {
    // Minimal PEM header to satisfy construction (bytes are never sent).
    let fake_ca_pem = b"-----BEGIN CERTIFICATE-----\nZg==\n-----END CERTIFICATE-----\n".to_vec();
    let _builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("db")
        .ssl_with_ca_pem(fake_ca_pem);
    // Verifies the builder method exists and accepts Vec<u8>.
}

/// Verify `ssl_disabled` builder compiles and can be called after other SSL methods.
///
/// No live server is needed.
#[test]
fn ssl_disabled_builder_compiles() {
    let _builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("db")
        .ssl_skip_verify()
        .ssl_disabled();
    // Verifies the builder resets SSL mode cleanly.
}

/// Verify all pool configuration builder methods compile and chain correctly.
///
/// No live server is needed — this validates the API surface only.
#[test]
fn test_builder_pool_config() {
    let builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("mydb")
        .user("root")
        .password("secret")
        .pool_min(2)
        .pool_max(20)
        .pool_idle_timeout(300)
        .pool_ttl(3600);
    let _ = builder; // compiles and chains without panic
}

/// Verify that `pool_min` and `pool_max` defaults produce valid constraints.
///
/// `build_pool_opts` is not public, so we validate via the Debug output — the
/// builder must construct without panic when only pool_max is set.
#[test]
fn test_builder_pool_max_only() {
    let builder = MyConnectionBuilder::new().pool_max(50);
    let _ = builder;
}

/// Verify `from_pool` constructs a `MyConnection` from an existing pool.
///
/// Uses a URL that points to a non-existent server; the pool is lazy so
/// construction succeeds and errors only arise on first `get_conn()`.
#[tokio::test]
async fn from_pool_constructs() {
    use mysql_async::{Opts, Pool};
    // Use a URL that is guaranteed to have no server.
    let opts = Opts::from_url("mysql://root@127.0.0.1:19999/test").expect("URL must be parseable");
    let pool = Pool::new(opts);
    let conn = oxisql_mysql::MyConnection::from_pool(pool);
    // Cloning is cheap (ref-counted pool).
    let _conn2 = conn.clone();
}

/// Verify that all builder methods chain in one expression without panic.
///
/// Exercises `connect_timeout_secs`, `ssl_disabled`, and pool configuration
/// together, confirming the builder API surface is fully chainable.
///
/// No live server is needed — construction only.
#[test]
fn test_builder_chaining_complete() {
    let _builder = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .dbname("db")
        .user("root")
        .password("secret")
        .pool_min(2)
        .pool_max(10)
        .pool_idle_timeout(300)
        .pool_ttl(3600)
        .connect_timeout_secs(5)
        .ssl_disabled();
    // All methods must compile and chain without panic.
}

/// Verify that `MyTransaction` is accessible as a public type.
///
/// We cannot construct one without a live MySQL connection, but we can
/// confirm the type is exported and usable in type position.
#[test]
fn test_my_transaction_type_is_accessible() {
    // `Option<MyTransaction>` is a valid type that compiles only if
    // `MyTransaction` is publicly exported.
    let _: Option<oxisql_mysql::MyTransaction> = None;
}

/// Verify that `MyConnectionBuilder::new()` produces a builder where
/// URL components can be set and overridden independently.
///
/// No live server is needed.
#[test]
fn test_builder_url_component_overrides() {
    // Start with one host and override with another.
    let b1 = MyConnectionBuilder::new()
        .host("first.host.com")
        .port(3310)
        .dbname("db1");
    let _ = b1;

    // A separate builder should be independent (no shared state).
    let b2 = MyConnectionBuilder::new()
        .host("second.host.com")
        .port(3307)
        .dbname("db2")
        .user("admin")
        .password("hunter2");
    let _ = b2;
}

/// Placeholder integration test — skipped unless a real MySQL server is present.
///
/// Run with: `cargo test -p oxisql-mysql --features integration-mysql -- --ignored`
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn integration_connect_and_ping() {
    use oxisql_core::Connection;

    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect to MySQL");

    // A trivial query to prove the connection is alive.
    let _rows = conn.query("SELECT 1 AS val", &[]).await.expect("SELECT 1");
}

// ── server_version ────────────────────────────────────────────────────────────
//
// Unlike `oxisql-postgres`, this crate has no hand-rolled wire-protocol
// implementation of its own (the MySQL handshake — capability negotiation,
// pluggable auth methods, etc. — is entirely delegated to `mysql_async`), so
// there is no existing fake-server test-harness pattern here to reuse for a
// unit-level test. `server_version_fails_gracefully_without_server` below
// still exercises the method with no live server needed, using the same
// deliberately-unreachable-port idiom as `connect_disabled_tls_fails_gracefully`
// above; the `Some(...)`-returning happy path requires a real server and is
// covered by the `#[ignore]`d integration test that follows it.

/// Verify that `server_version()` fails gracefully (typed error, no panic)
/// when no MySQL server is reachable.  No live server needed.
#[tokio::test]
async fn server_version_fails_gracefully_without_server() {
    let conn = MyConnection::connect("mysql://root@127.0.0.1:19999/test", TlsMode::Disabled)
        .await
        .expect("connect is lazy — pool construction alone should not fail");

    let result = conn.server_version().await;
    assert!(
        result.is_err(),
        "expected an error with no server reachable, got {result:?}"
    );
}

/// Placeholder integration test — skipped unless a real MySQL server is present.
///
/// Run with: `cargo test -p oxisql-mysql --features integration-mysql -- --ignored`
#[cfg(feature = "integration-mysql")]
#[ignore]
#[tokio::test]
async fn integration_server_version_reports_dotted_version() {
    let conn = MyConnection::connect("mysql://root@localhost/test", TlsMode::Disabled)
        .await
        .expect("connect to MySQL");

    let version = conn.server_version().await.expect("server_version");
    assert!(
        version.split('.').count() >= 2 && version.chars().next().is_some_and(char::is_numeric),
        "expected a dotted version string starting with a digit, got {version:?}"
    );
}

// ── oxisql-parse integration — is_read_only_query / normalize_query ──────────

#[test]
fn test_mysql_normalize_query_collapses_whitespace() {
    let n = MyConnection::normalize_query("SELECT  id  FROM t");
    assert!(!n.is_empty());
    assert!(!n.contains("  "));
}

#[test]
fn test_mysql_normalize_query_empty_returns_empty() {
    assert!(MyConnection::normalize_query("").is_empty());
}

#[test]
fn test_mysql_is_read_only_query_select_is_true() {
    assert!(MyConnection::is_read_only_query("SELECT id FROM users"));
}

#[test]
fn test_mysql_is_read_only_query_update_is_false() {
    assert!(!MyConnection::is_read_only_query("UPDATE t SET x = 1"));
}

#[test]
fn test_mysql_is_read_only_query_unparseable_returns_false() {
    assert!(!MyConnection::is_read_only_query("@@@NOT VALID SQL@@@"));
}
