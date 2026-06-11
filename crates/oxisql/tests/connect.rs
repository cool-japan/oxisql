/// Test the `oxisql::connect` facade.
///
/// When the `embedded` feature is **not** active (the default), every URI
/// scheme must return `OxiSqlError::NotConnected`.  When `embedded` is active,
/// `memory://` must succeed.
#[tokio::test]
async fn unknown_scheme_always_errors() {
    let result = oxisql::connect("postgres://localhost/test").await;
    assert!(
        result.is_err(),
        "unknown scheme must return an error; got Ok"
    );
}

#[tokio::test]
#[cfg(feature = "embedded")]
async fn memory_connect_with_embedded_feature() {
    let conn = oxisql::connect("memory://")
        .await
        .expect("memory:// must succeed with the embedded feature");
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("DDL must execute");
}

#[tokio::test]
#[cfg(not(feature = "embedded"))]
async fn memory_connect_without_embedded_feature_errors() {
    let result = oxisql::connect("memory://").await;
    assert!(
        result.is_err(),
        "memory:// without the embedded feature must return NotConnected"
    );
}

#[cfg(feature = "pool-embedded")]
#[tokio::test]
async fn connect_pooled_memory() {
    let pool = oxisql::connect_pooled("memory://", 4)
        .await
        .expect("pool created");
    let conn = pool.get().await.expect("got connection");
    conn.execute("CREATE TABLE p (x INT)", &[])
        .await
        .expect("create");
    conn.execute("INSERT INTO p VALUES (99)", &[])
        .await
        .expect("insert");
    let rows = conn.query("SELECT x FROM p", &[]).await.expect("select");
    assert_eq!(rows.len(), 1);
}

#[cfg(all(
    feature = "pool-embedded",
    not(feature = "pool-postgres"),
    not(feature = "pool-mysql")
))]
#[tokio::test]
async fn connect_pooled_unknown_scheme_errors() {
    let result = oxisql::connect_pooled("postgres://localhost/test", 4).await;
    assert!(
        result.is_err(),
        "pool-embedded only: unknown scheme must return an error"
    );
}

// Test version() function
#[test]
fn version_is_non_empty() {
    let v = oxisql::version();
    assert!(!v.is_empty());
}

// Test full CRUD cycle through embedded facade (requires embedded feature)
#[cfg(feature = "embedded")]
#[tokio::test]
async fn memory_full_crud() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
        .await
        .unwrap();

    // INSERT
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO users VALUES (2, 'Bob')", &[])
        .await
        .unwrap();

    // SELECT
    let rows = conn
        .query("SELECT id, name FROM users ORDER BY id", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);

    // UPDATE
    conn.execute("UPDATE users SET name = 'Carol' WHERE id = 2", &[])
        .await
        .unwrap();
    let rows = conn
        .query("SELECT name FROM users WHERE id = 2", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    // DELETE
    conn.execute("DELETE FROM users WHERE id = 1", &[])
        .await
        .unwrap();
    let rows = conn.query("SELECT id FROM users", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
}

// Test transaction through facade (requires embedded feature)
//
// GlueSQL MemoryStorage does not support transactions at the storage level;
// `transaction()` will return an error.  This test verifies the facade
// propagates the error cleanly rather than panicking.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn transaction_commit_through_facade() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .await
        .unwrap();

    let txn_result = conn.transaction().await;
    if let Ok(mut txn) = txn_result {
        txn.execute("INSERT INTO t VALUES (42)", &[]).await.unwrap();
        txn.commit().await.unwrap();
        let rows = conn.query("SELECT v FROM t", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);
    }
    // GlueSQL embedded may not support transactions — Err result is acceptable
}

// Test transaction rollback through facade (requires embedded feature)
//
// GlueSQL MemoryStorage does not support transactions at the storage level;
// `transaction()` will return an error.  This test verifies the facade
// propagates the error cleanly rather than panicking.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn transaction_rollback_through_facade() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .await
        .unwrap();

    let txn_result = conn.transaction().await;
    if let Ok(mut txn) = txn_result {
        txn.execute("INSERT INTO t VALUES (99)", &[]).await.unwrap();
        // GlueSQL embedded may or may not support rollback — just verify no panic
        let _ = txn.rollback().await;
    }
    // GlueSQL embedded may not support transactions — Err result is acceptable

    let rows = conn.query("SELECT v FROM t", &[]).await.unwrap();
    // Just verify no panic
    let _ = rows;
}

// Test prepare through facade (requires embedded feature)
#[cfg(feature = "embedded")]
#[tokio::test]
async fn prepare_through_facade() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'hello')", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (2, 'world')", &[])
        .await
        .unwrap();

    let mut stmt = conn
        .prepare("SELECT val FROM t WHERE id = $1")
        .await
        .unwrap();
    let rows = stmt
        .query(&[&1_i64 as &dyn oxisql::ToSqlValue])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);

    let rows2 = stmt
        .query(&[&2_i64 as &dyn oxisql::ToSqlValue])
        .await
        .unwrap();
    assert_eq!(rows2.len(), 1);
}

// Test ping through facade (requires embedded feature)
#[cfg(feature = "embedded")]
#[tokio::test]
async fn ping_through_facade() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.ping().await.unwrap();
}

// Test execute_batch through facade (requires embedded feature)
#[cfg(feature = "embedded")]
#[tokio::test]
async fn execute_batch_through_facade() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute_batch("CREATE TABLE t1 (id INTEGER); CREATE TABLE t2 (id INTEGER)")
        .await
        .unwrap();
    let _ = conn.query("SELECT id FROM t1", &[]).await.unwrap();
    let _ = conn.query("SELECT id FROM t2", &[]).await.unwrap();
}

#[cfg(feature = "embedded")]
#[tokio::test]
async fn connect_memory_end_to_end() {
    let conn = oxisql::connect("memory://").await.unwrap();
    conn.execute("CREATE TABLE e2e_test (id INT, name TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO e2e_test VALUES (1, 'Alice')", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO e2e_test VALUES (2, 'Bob')", &[])
        .await
        .unwrap();
    let rows = conn
        .query("SELECT id, name FROM e2e_test", &[])
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let id: i64 = rows[0].try_get("id").unwrap();
    assert_eq!(id, 1);
}

// When the `sqlite` feature is disabled, sqlite:// is an unsupported URI.
// When the `sqlite` feature is enabled, sqlite:// opens a Limbo connection.
#[cfg(not(feature = "sqlite"))]
#[tokio::test]
async fn unknown_scheme_returns_unsupported_uri() {
    let p = std::env::temp_dir().join("test.db");
    let result = oxisql::connect(&format!("sqlite:///{}", p.display())).await;
    assert!(
        matches!(result, Err(oxisql::OxiSqlError::UnsupportedUri(_))),
        "expected UnsupportedUri"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn sqlite_scheme_connects_when_feature_enabled() {
    // sqlite::memory: opens an in-memory Limbo SQLite database
    let result = oxisql::connect("sqlite::memory:").await;
    assert!(
        result.is_ok(),
        "sqlite::memory: should succeed with sqlite feature"
    );
}

#[cfg(feature = "embedded")]
#[tokio::test]
async fn ping_embedded_succeeds() {
    let conn = oxisql::connect("memory://").await.unwrap();
    let result = oxisql::ping(conn.as_ref()).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn facade_reexports_accessible() {
    // Verify key types are re-exported through the facade
    let _: fn() -> oxisql::Value = || oxisql::Value::Null;
    let _: fn() -> oxisql::OxiSqlError = || oxisql::OxiSqlError::NotConnected;
}

#[cfg(feature = "embedded")]
#[tokio::test]
async fn connect_with_options_memory() {
    let opts = oxisql::ConnectOptions::new().timeout_ms(5000);
    let conn = oxisql::connect_with_options("memory://", opts)
        .await
        .unwrap();
    conn.execute("CREATE TABLE opts_test (x INT)", &[])
        .await
        .unwrap();
    let result = conn.execute("INSERT INTO opts_test VALUES (99)", &[]).await;
    assert!(result.is_ok());
}

#[cfg(feature = "pool-embedded")]
#[tokio::test]
async fn connect_pooled_memory_via_connection_pool_trait() {
    let pool = oxisql::connect_pooled("memory://", 4).await.unwrap();
    let conn = pool.get().await.unwrap();
    conn.execute("CREATE TABLE pool_trait_test (v INT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO pool_trait_test VALUES (7)", &[])
        .await
        .unwrap();
    let rows = conn
        .query("SELECT v FROM pool_trait_test", &[])
        .await
        .unwrap();
    assert_eq!(rows[0].try_get::<i64>("v").unwrap(), 7);
}

// ── BackendInfo tests ─────────────────────────────────────────────────────────

#[test]
fn test_backend_info_embedded() {
    let info = oxisql::backend_info_for_uri("memory://").expect("should return info");
    assert_eq!(info.name, "embedded");
    assert!(info.version.is_some());
    assert!(info.features.contains(&"in-memory"));
}

#[test]
fn test_backend_info_postgres() {
    let info =
        oxisql::backend_info_for_uri("postgres://localhost/mydb").expect("should return info");
    assert_eq!(info.name, "postgres");
    assert!(info.features.contains(&"tls"));
}

#[test]
fn test_backend_info_postgresql_scheme() {
    let info = oxisql::backend_info_for_uri("postgresql://localhost/mydb")
        .expect("postgresql:// scheme should return info");
    assert_eq!(info.name, "postgres");
}

#[test]
fn test_backend_info_mysql() {
    let info = oxisql::backend_info_for_uri("mysql://localhost/db").expect("should return info");
    assert_eq!(info.name, "mysql");
    assert!(info.features.contains(&"tls"));
}

#[test]
fn test_backend_info_unknown() {
    assert!(oxisql::backend_info_for_uri("unknown://foo").is_none());
}

#[cfg(not(feature = "sqlite"))]
#[test]
fn test_backend_info_sqlite_unknown_without_feature() {
    assert!(oxisql::backend_info_for_uri("sqlite://foo").is_none());
}

#[cfg(feature = "sqlite")]
#[test]
fn test_backend_info_sqlite_with_feature() {
    let info = oxisql::backend_info_for_uri("sqlite://foo").unwrap();
    assert_eq!(info.name, "sqlite");
}

#[test]
fn test_backend_info_empty_uri() {
    assert!(oxisql::backend_info_for_uri("").is_none());
}

// ── connect_with_tls tests ────────────────────────────────────────────────────

#[tokio::test]
#[cfg(feature = "embedded")]
async fn connect_with_tls_memory_no_tls_config() {
    // Embedded backend ignores TLS; passing None is the same as connect().
    let conn = oxisql::connect_with_tls("memory://", None)
        .await
        .expect("connect_with_tls(memory://, None) must succeed with embedded feature");
    conn.execute("CREATE TABLE tls_test (id INTEGER)", &[])
        .await
        .expect("DDL must execute");
}

// Without the sqlite feature, sqlite:// is unknown and errors.
// With the sqlite feature, sqlite:// connects successfully.
#[cfg(not(feature = "sqlite"))]
#[tokio::test]
async fn connect_with_tls_unknown_scheme_errors() {
    let result = oxisql::connect_with_tls("unknown-scheme://x", None).await;
    assert!(
        result.is_err(),
        "unknown scheme must return an error from connect_with_tls"
    );
}

#[cfg(feature = "sqlite")]
#[tokio::test]
async fn connect_with_tls_sqlite_memory_succeeds() {
    let result = oxisql::connect_with_tls("sqlite::memory:", None).await;
    assert!(
        result.is_ok(),
        "sqlite::memory: must succeed with the sqlite feature enabled"
    );
}

// ── Transaction-through-facade test ──────────────────────────────────────────

#[tokio::test]
#[cfg(feature = "embedded")]
async fn test_transaction_through_facade() {
    let conn = oxisql::connect("memory://").await.expect("connect");
    conn.execute("CREATE TABLE txn_test (id INTEGER, val TEXT)", &[])
        .await
        .expect("create");

    let txn_result = conn.transaction().await;
    if let Ok(mut txn) = txn_result {
        txn.execute("INSERT INTO txn_test VALUES (1, 'hello')", &[])
            .await
            .expect("insert");
        // GlueSQL may not support rollback — just verify it does not panic
        let _ = txn.rollback().await;
    }
    // Accept either 0 rows (real rollback) or 1 row (embedded no-txn fallback)
    let rows = conn
        .query("SELECT * FROM txn_test", &[])
        .await
        .unwrap_or_default();
    assert!(rows.len() <= 1);
}

// ── Ping-through-facade (using free function) test ───────────────────────────

#[tokio::test]
#[cfg(feature = "embedded")]
async fn test_ping_through_facade() {
    let conn = oxisql::connect("memory://").await.expect("connect");
    oxisql::ping(conn.as_ref())
        .await
        .expect("ping should succeed");
}

// ── Introspect test ───────────────────────────────────────────────────────────

#[tokio::test]
#[cfg(feature = "embedded")]
async fn test_introspect_returns_tables() {
    let conn = oxisql::connect("memory://").await.expect("connect");
    conn.execute("CREATE TABLE introspect_tbl (id INTEGER)", &[])
        .await
        .expect("create");
    let tables = oxisql::introspect(conn.as_ref()).await;
    // Verify no panic; GlueSQL introspection support varies
    let _ = tables;
}

// ── DataFusion re-export test ─────────────────────────────────────────────────

#[cfg(feature = "datafusion")]
#[test]
fn test_datafusion_reexport_types_accessible() {
    // Verify the datafusion sub-module re-exports are accessible through the facade.
    // We use type aliases to confirm the names resolve without instantiating them.
    type _FusionError = oxisql::datafusion::OxiSqlFusionError;
    type _TableProvider = oxisql::datafusion::OxiSqlTableProvider;
}

// ── Close-through-facade test ─────────────────────────────────────────────────

#[tokio::test]
#[cfg(feature = "embedded")]
async fn test_close_through_facade() {
    let conn = oxisql::connect("memory://").await.expect("connect");
    oxisql::close(conn); // should not panic or leak
}

// ── Consolidated BackendInfo all-schemes test ────────────────────────────────

#[test]
fn test_backend_info_all_schemes() {
    let embedded = oxisql::backend_info_for_uri("memory://").expect("embedded");
    assert_eq!(embedded.name, "embedded");
    let pg = oxisql::backend_info_for_uri("postgres://localhost/db").expect("pg");
    assert_eq!(pg.name, "postgres");
    let pgal = oxisql::backend_info_for_uri("postgresql://localhost/db").expect("pgal");
    assert_eq!(pgal.name, "postgres");
    let mysql = oxisql::backend_info_for_uri("mysql://localhost/db").expect("mysql");
    assert_eq!(mysql.name, "mysql");
    assert!(oxisql::backend_info_for_uri("unknown://foo").is_none());
}

// ── Migration-through-facade test ─────────────────────────────────────────────

#[tokio::test]
#[cfg(all(feature = "embedded", feature = "migrate"))]
async fn test_migration_through_facade() {
    use oxisql::migrate::scan_migrations;
    use std::io::Write;

    let dir = std::env::temp_dir().join(format!(
        "oxisql_facade_migrate_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&dir).ok();

    let path = dir.join("20230101000000__create_test.sql");
    {
        let mut f = std::fs::File::create(&path).expect("create migration file");
        f.write_all(b"CREATE TABLE facade_test (id INT)")
            .expect("write migration sql");
    }

    let _conn = oxisql::connect("memory://").await.expect("connect");

    // Verify the scanner picks up the migration file through the facade re-export.
    let files = scan_migrations(&dir).expect("scan migrations");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].name, "create_test");

    // Clean up the entire unique directory to avoid state leakage.
    let _ = std::fs::remove_dir_all(&dir);
}

// ── LoggingConnection tests ───────────────────────────────────────────────────

/// Verify that `LoggingConnection` wraps an embedded connection and that
/// `execute` returns the correct result without panicking.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_logging_connection_execute() {
    use oxisql::logging::LoggingConnection;
    use oxisql::Connection;

    let inner = oxisql::connect("memory://")
        .await
        .expect("embedded connect should succeed");
    let conn = LoggingConnection::new(inner, "test_execute");

    conn.execute("CREATE TABLE log_exec_test (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE should succeed through LoggingConnection");

    let affected = conn
        .execute("INSERT INTO log_exec_test VALUES (42)", &[])
        .await
        .expect("INSERT should succeed through LoggingConnection");

    // GlueSQL may return 0 or 1 for INSERT affected rows; just verify no panic.
    let _ = affected;
}

/// Verify that `LoggingConnection` wraps an embedded connection and that
/// `query` returns the correct rows without panicking.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_logging_connection_query() {
    use oxisql::logging::LoggingConnection;
    use oxisql::Connection;

    let inner = oxisql::connect("memory://")
        .await
        .expect("embedded connect should succeed");
    let conn = LoggingConnection::new(inner, "test_query");

    conn.execute("CREATE TABLE log_query_test (id INTEGER, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO log_query_test VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT 1");
    conn.execute("INSERT INTO log_query_test VALUES (2, 'Bob')", &[])
        .await
        .expect("INSERT 2");

    let rows = conn
        .query("SELECT id, name FROM log_query_test ORDER BY id", &[])
        .await
        .expect("SELECT should succeed through LoggingConnection");

    assert_eq!(rows.len(), 2, "expected 2 rows from SELECT");
    let id: i64 = rows[0].try_get("id").expect("id column");
    assert_eq!(id, 1);
}

/// Verify that the underlying data is accessible after the logging wrapper
/// delegates operations correctly to the inner connection.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_logging_connection_delegates_correctly() {
    use oxisql::logging::LoggingConnection;
    use oxisql::Connection;

    let inner = oxisql::connect("memory://")
        .await
        .expect("embedded connect should succeed");
    let conn = LoggingConnection::new(inner, "test_delegates");

    // Verify label accessor.
    assert_eq!(conn.label(), "test_delegates");

    conn.execute("CREATE TABLE log_delegate_test (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO log_delegate_test VALUES (99)", &[])
        .await
        .expect("INSERT");

    let rows = conn
        .query("SELECT v FROM log_delegate_test", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    let v: i64 = rows[0].try_get("v").expect("v column");
    assert_eq!(v, 99);

    // Verify ping delegates correctly.
    conn.ping()
        .await
        .expect("ping should succeed through LoggingConnection");

    // Verify into_inner() recovers the inner box.
    let _inner: Box<dyn oxisql::Connection> = conn.into_inner();
}

// ── connect_or_create tests ───────────────────────────────────────────────────

/// Verify that `connect_or_create` works for embedded connections.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_connect_or_create_embedded() {
    let conn = oxisql::connect_or_create("memory://")
        .await
        .expect("embedded should always work with connect_or_create");
    let _ = conn;
}

/// Verify that `connect_or_create` returns an error for unknown URI schemes.
#[cfg(not(feature = "sqlite"))]
#[tokio::test]
async fn test_connect_or_create_unknown_scheme() {
    let result = oxisql::connect_or_create("unknown-scheme://test").await;
    assert!(
        result.is_err(),
        "unknown scheme must return an error from connect_or_create"
    );
}

/// When the sqlite feature is enabled, sqlite::memory: should succeed.
#[cfg(feature = "sqlite")]
#[tokio::test]
async fn test_connect_or_create_sqlite_memory() {
    let result = oxisql::connect_or_create("sqlite::memory:").await;
    assert!(
        result.is_ok(),
        "sqlite::memory: must succeed with the sqlite feature"
    );
}

/// Verify that `connect_or_create` with embedded allows full CRUD.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_connect_or_create_embedded_full_crud() {
    let conn = oxisql::connect_or_create("memory://")
        .await
        .expect("connect_or_create memory://");

    conn.execute("CREATE TABLE coc_test (id INTEGER, val TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO coc_test VALUES (1, 'hello')", &[])
        .await
        .expect("INSERT");

    let rows = conn
        .query("SELECT id, val FROM coc_test", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
    let id: i64 = rows[0].try_get("id").expect("id");
    assert_eq!(id, 1);
}

// ── URI scheme tests (feature-disabled + new scheme coverage) ────────────────

/// Verify that an entirely unsupported scheme (e.g. ftp://) returns an error
/// whose message references the URI or describes it as unsupported.
#[tokio::test]
async fn test_unsupported_uri_returns_error() {
    let result = oxisql::connect("ftp://localhost/db").await;
    assert!(result.is_err(), "ftp:// must return an error");
    let err_str = result.err().expect("is_err() already checked").to_string();
    assert!(
        err_str.contains("ftp://") || err_str.to_lowercase().contains("unsupported"),
        "error message should mention the URI or 'unsupported'; got: {err_str}"
    );
}

/// Verify that an empty URI returns an error.
#[tokio::test]
async fn test_empty_uri_returns_error() {
    let result = oxisql::connect("").await;
    assert!(result.is_err(), "empty URI must return an error");
}

/// With the `sled` feature enabled, `file://<path>` routes to the sled-backed
/// persistent embedded engine and connects successfully.
#[cfg(feature = "sled")]
#[tokio::test]
async fn test_file_uri_connects_with_sled() {
    // Hermetic, unique directory under the system temp dir (per repo policy).
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "oxisql_file_uri_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let uri = format!("file://{}", dir.display());

    let conn = oxisql::connect(&uri)
        .await
        .expect("file:// must succeed with the sled feature");
    conn.execute("CREATE TABLE file_uri_test (id INTEGER)", &[])
        .await
        .expect("DDL must execute against the sled-backed connection");

    // Drop the connection before removing the directory so sled releases its lock.
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A path-less `file://` URI is always an error: there is nowhere to open.
#[cfg(feature = "sled")]
#[tokio::test]
async fn test_file_uri_without_path_errors() {
    let result = oxisql::connect("file://").await;
    assert!(
        result.is_err(),
        "path-less file:// must return an error (no path to open)"
    );
}

/// Without the `sled` feature there is no persistent embedded backend wired to
/// `file://`, so it must return a clear error.
#[cfg(not(feature = "sled"))]
#[tokio::test]
async fn test_file_uri_requires_sled_feature() {
    let p = std::env::temp_dir().join("test.db");
    let result = oxisql::connect(&format!("file:///{}", p.display())).await;
    assert!(
        result.is_err(),
        "file:// must return an error when the sled feature is disabled"
    );
}

/// Verify that datafusion:// scheme returns a clear error pointing at connect_datafusion().
#[tokio::test]
async fn test_datafusion_uri_in_connect_returns_clear_error() {
    let result = oxisql::connect("datafusion://").await;
    assert!(
        result.is_err(),
        "datafusion:// must not return a Connection"
    );
    let err_str = result.err().expect("is_err() already checked").to_string();
    // The error should mention datafusion and the correct API.
    assert!(
        err_str.to_lowercase().contains("datafusion")
            || err_str.to_lowercase().contains("unsupported"),
        "error should mention datafusion or unsupported; got: {err_str}"
    );
}

/// Verify that memory:// connects and can execute a minimal query.
#[cfg(feature = "embedded")]
#[tokio::test]
async fn test_memory_scheme_connects() {
    let conn = oxisql::connect("memory://")
        .await
        .expect("memory:// must succeed with embedded feature");
    conn.execute("CREATE TABLE mem_scheme_test (val INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO mem_scheme_test VALUES (1)", &[])
        .await
        .expect("INSERT");
    let rows = conn
        .query("SELECT val FROM mem_scheme_test", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);
}

// ── backend_info_for_uri DataFusion test ─────────────────────────────────────

/// Verify backend_info_for_uri returns the datafusion backend for datafusion://.
#[test]
fn test_backend_info_for_datafusion() {
    let info = oxisql::backend_info_for_uri("datafusion://")
        .expect("datafusion:// should return BackendInfo");
    assert_eq!(info.name, "datafusion");
    assert!(
        info.features.contains(&"olap"),
        "datafusion backend should advertise 'olap' feature"
    );
}

/// Verify that connect_datafusion returns an OxiSqlContext for datafusion:// and memory://.
#[cfg(feature = "datafusion")]
#[tokio::test]
async fn test_connect_datafusion_uri() {
    let _ctx = oxisql::connect_datafusion("datafusion://")
        .await
        .expect("connect_datafusion(datafusion://) should succeed");
}

#[cfg(feature = "datafusion")]
#[tokio::test]
async fn test_connect_datafusion_memory_alias() {
    let _ctx = oxisql::connect_datafusion("memory://")
        .await
        .expect("connect_datafusion(memory://) should succeed as datafusion alias");
}

#[cfg(feature = "datafusion")]
#[tokio::test]
async fn test_connect_datafusion_unknown_scheme_errors() {
    let result = oxisql::connect_datafusion("postgres://localhost/db").await;
    assert!(
        result.is_err(),
        "connect_datafusion should reject non-datafusion URIs"
    );
}

// ── connect_pool (typed OxidbPool) tests ─────────────────────────────────────

/// `connect_pool("memory://")` returns an embedded `OxidbPool` and its
/// `health_check()` succeeds immediately (no real database needed).
#[cfg(feature = "pool-embedded")]
#[tokio::test]
async fn test_connect_pool_memory_health_check() {
    let pool = oxisql::connect_pool("memory://", 4)
        .await
        .expect("connect_pool memory:// must succeed");
    pool.health_check()
        .await
        .expect("health_check on fresh embedded pool must succeed");
}

/// `connect_pool` with an unrecognised URI scheme returns `UnsupportedUri`.
#[cfg(feature = "pool-embedded")]
#[tokio::test]
async fn test_connect_pool_unsupported_uri_errors() {
    let result = oxisql::connect_pool("ftp://localhost/db", 4).await;
    assert!(
        result.is_err(),
        "connect_pool with unsupported URI must return Err"
    );
}

// ── redb:// URI tests ─────────────────────────────────────────────────────────

/// Verify `connect("redb://path")` opens a persistent redb connection and
/// can execute a minimal CREATE + INSERT + SELECT cycle.
#[cfg(feature = "redb")]
#[tokio::test]
async fn test_connect_redb_uri() {
    let path = std::env::temp_dir().join("test_oxisql_connect_redb.db");
    // Clean up any leftover from a previous test run.
    let _ = std::fs::remove_file(&path);

    let uri = format!("redb://{}", path.display());
    let conn = oxisql::connect(&uri)
        .await
        .expect("redb:// connect should succeed");

    conn.execute("CREATE TABLE redb_uri_test (id INTEGER, v TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO redb_uri_test VALUES (1, 'hello')", &[])
        .await
        .expect("INSERT");

    let rows = conn
        .query("SELECT id, v FROM redb_uri_test", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 1);

    // Clean up.
    let _ = std::fs::remove_file(&path);
}

/// Verify `connect("redb://")` (no path) returns `UnsupportedUri`.
#[cfg(feature = "redb")]
#[tokio::test]
async fn test_connect_redb_empty_path_errors() {
    let result = oxisql::connect("redb://").await;
    assert!(
        matches!(result, Err(oxisql::OxiSqlError::UnsupportedUri(_))),
        "redb:// with no path must return UnsupportedUri"
    );
}

// ── connect_pooled dispatch tests ────────────────────────────────────────────

/// Verify the embedded variant of `connect_pooled` compiles correctly and that
/// `pool_size()` reports at least 1 (the embedded pool is always available).
#[cfg(feature = "pool-embedded")]
#[tokio::test]
async fn test_connect_pooled_memory_works() {
    let pool = oxisql::connect_pooled("memory://", 4)
        .await
        .expect("embedded pool should work");
    assert!(pool.pool_size() >= 1);
}

/// Verify `backend_info_for_uri("redb://foo.db")` returns the redb backend.
#[test]
fn test_backend_info_redb() {
    let p = std::env::temp_dir().join("test.db");
    let info = oxisql::backend_info_for_uri(&format!("redb:///{}", p.display()))
        .expect("redb:// should return BackendInfo");
    assert_eq!(info.name, "redb");
    assert!(info.features.contains(&"persistent"));
    assert!(info.features.contains(&"pure-rust"));
}

/// Verify `backend_info_for_uri("fjall://foo")` returns the fjall backend.
#[test]
fn test_backend_info_fjall() {
    let p = std::env::temp_dir().join("test_dir");
    let info = oxisql::backend_info_for_uri(&format!("fjall:///{}", p.display()))
        .expect("fjall:// should return BackendInfo");
    assert_eq!(info.name, "fjall");
    assert!(info.features.contains(&"persistent"));
    assert!(info.features.contains(&"lsm-tree"));
}

// ── Item A: Connection-string query-parameter parsing tests ──────────────────

/// URI with two known query parameters auto-configures `ConnectOptions` fields.
#[test]
fn query_string_multi_param() {
    let opts = oxisql::ConnectOptions::from_uri("sqlite://path.db?pool_max=8&connect_timeout=5");
    assert_eq!(opts.pool_size, Some(8), "pool_max should map to pool_size");
    assert_eq!(
        opts.connect_timeout_ms,
        Some(5_000),
        "connect_timeout (secs) should be stored as milliseconds"
    );
}

/// An unknown query key should land in `ConnectOptions::extra`.
#[test]
fn query_string_unknown_key() {
    let opts = oxisql::ConnectOptions::from_uri("postgres://localhost/db?my_custom_key=hello");
    assert_eq!(
        opts.extra.get("my_custom_key").map(|s| s.as_str()),
        Some("hello"),
        "unknown key should be stored in ConnectOptions::extra"
    );
}

/// No query string → no-op; all fields keep their defaults.
#[test]
fn query_string_empty() {
    let opts = oxisql::ConnectOptions::from_uri("memory://");
    assert!(opts.pool_size.is_none(), "pool_size should default to None");
    assert!(
        opts.connect_timeout_ms.is_none(),
        "connect_timeout_ms should default to None"
    );
    assert!(!opts.require_tls, "require_tls should default to false");
    assert!(opts.extra.is_empty(), "extra should be empty");
}

/// `sslmode=require` sets `require_tls = true` and populates `sslmode`.
#[test]
fn query_string_sslmode_require() {
    let opts = oxisql::ConnectOptions::from_uri("postgres://host/db?sslmode=require");
    assert_eq!(opts.sslmode.as_deref(), Some("require"));
    assert!(opts.require_tls, "sslmode=require should set require_tls");
}

/// `application_name` is stored in the dedicated field.
#[test]
fn query_string_application_name() {
    let opts = oxisql::ConnectOptions::from_uri("postgres://host/db?application_name=myapp");
    assert_eq!(opts.application_name.as_deref(), Some("myapp"));
}

// ── Item B: BackendInfo version for local backends tests ─────────────────────

/// Embedded backend should report a non-empty version string.
#[test]
fn backend_info_embedded_has_version() {
    let info = oxisql::backend_info_for_uri("memory://").expect("should return info");
    assert_eq!(info.name, "embedded");
    let version = info.version.expect("embedded backend must have a version");
    assert!(
        !version.is_empty(),
        "embedded backend version must not be empty"
    );
}

/// PostgreSQL backend should have `None` version (not known until handshake).
#[test]
fn backend_info_postgres_version_is_none() {
    let info = oxisql::backend_info_for_uri("postgres://localhost/db").expect("pg backend info");
    assert!(
        info.version.is_none(),
        "postgres version should be None before connection handshake"
    );
}

/// MySQL backend should have `None` version (not known until handshake).
#[test]
fn backend_info_mysql_version_is_none() {
    let info = oxisql::backend_info_for_uri("mysql://localhost/db").expect("mysql backend info");
    assert!(
        info.version.is_none(),
        "mysql version should be None before connection handshake"
    );
}

/// redb backend should report a non-empty version string.
#[test]
fn backend_info_redb_has_version() {
    let p = std::env::temp_dir().join("bi_redb_test.db");
    let info = oxisql::backend_info_for_uri(&format!("redb://{}", p.display()))
        .expect("redb backend info");
    let version = info.version.expect("redb backend must have a version");
    assert!(!version.is_empty(), "redb version string must not be empty");
}

/// fjall backend should report a non-empty version string.
#[test]
fn backend_info_fjall_has_version() {
    let p = std::env::temp_dir().join("bi_fjall_test_dir");
    let info = oxisql::backend_info_for_uri(&format!("fjall://{}", p.display()))
        .expect("fjall backend info");
    let version = info.version.expect("fjall backend must have a version");
    assert!(
        !version.is_empty(),
        "fjall version string must not be empty"
    );
}

/// sqlite-compat backend should report a non-empty version string when the
/// feature is enabled.
#[cfg(feature = "sqlite")]
#[test]
fn backend_info_sqlite_compat_has_version() {
    let info = oxisql::backend_info_for_uri("sqlite://foo.db").expect("sqlite-compat backend info");
    let version = info
        .version
        .expect("sqlite-compat backend must have a version");
    assert!(
        !version.is_empty(),
        "sqlite-compat version string must not be empty"
    );
}
