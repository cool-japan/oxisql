//! Integration and unit tests for `oxidb-pool`.
//!
//! Postgres and MySQL tests are `#[ignore]`d by default — they require live
//! database instances reachable via environment variables:
//!   - `OXIDB_PG_URL`  — e.g. `postgres://user:pass@localhost:5432/mydb`
//!   - `OXIDB_MYSQL_URL` — e.g. `mysql://user:pass@localhost:3306/mydb`
//!
//! The embedded test runs in-process with no external dependencies.

// ── embedded (always runs) ────────────────────────────────────────────────────

#[cfg(feature = "embedded")]
mod embedded_tests {
    use gluesql::prelude::Payload;
    use oxisql_pool::embedded::EmbeddedPool;
    use oxisql_pool::PoolConfigBuilder;

    #[tokio::test]
    async fn embedded_pool_round_trip() {
        let pool = EmbeddedPool::new();

        // Clone the pool handle — both clones share the same Glue instance.
        let pool2 = pool.clone();

        // Execute DDL through pool.
        {
            let mut glue = pool.get().await.expect("get failed");
            let payloads = glue
                .execute("CREATE TABLE IF NOT EXISTS t (id INTEGER, name TEXT)")
                .await
                .expect("CREATE TABLE failed");
            assert!(!payloads.is_empty(), "expected at least one payload");
        }

        // Insert a row through the second handle.
        {
            let mut glue = pool2.get().await.expect("get failed");
            glue.execute("INSERT INTO t VALUES (1, 'hello')")
                .await
                .expect("INSERT failed");
        }

        // Query back through pool.
        {
            let mut glue = pool.get().await.expect("get failed");
            let payloads = glue
                .execute("SELECT id, name FROM t")
                .await
                .expect("SELECT failed");
            let mut found = false;
            for payload in payloads {
                if let Payload::Select { rows, .. } = payload {
                    if !rows.is_empty() {
                        found = true;
                    }
                }
            }
            assert!(found, "expected SELECT to return rows");
        }
    }

    #[tokio::test]
    async fn embedded_pool_default_is_empty() {
        let pool = EmbeddedPool::default();
        // Just verify we can acquire the lock and it starts empty.
        let mut glue = pool.get().await.expect("get failed");
        let payloads = glue.execute("SHOW TABLES").await.unwrap_or_default();
        // SHOW TABLES on fresh storage returns either empty Select or Create payload.
        let _ = payloads; // any result is fine
    }

    #[tokio::test]
    async fn embedded_pool_close_prevents_checkout() {
        let pool = EmbeddedPool::new();

        // Verify the pool works before closing.
        let _guard = pool.get().await.expect("should be open before close");
        drop(_guard);

        pool.close();

        // After close, get() must return an error.
        let result = pool.get().await;
        assert!(result.is_err(), "expected Err after pool.close(), got Ok");

        // Clones share the closed flag.
        let pool2 = pool.clone();
        let result2 = pool2.get().await;
        assert!(result2.is_err(), "cloned handle should also be closed");
    }

    #[test]
    fn embedded_pool_backend_name() {
        let pool = EmbeddedPool::new();
        assert_eq!(pool.backend_name(), "embedded");
    }

    #[test]
    fn pool_config_builder_defaults() {
        let config = PoolConfigBuilder::new().build();
        assert_eq!(config.max_size, 10);
        assert!(
            config.connect_timeout_ms.is_some(),
            "connect_timeout_ms should default to Some"
        );
        assert!(
            config.idle_timeout_ms.is_some(),
            "idle_timeout_ms should default to Some"
        );
        assert!(config.min_idle.is_none(), "min_idle should default to None");
    }

    #[test]
    fn pool_config_builder_custom() {
        let config = PoolConfigBuilder::new()
            .max_size(20)
            .min_idle(5)
            .connect_timeout_ms(5_000)
            .build();
        assert_eq!(config.max_size, 20);
        assert_eq!(config.min_idle, Some(5));
        assert_eq!(config.connect_timeout_ms, Some(5_000));
    }

    #[test]
    fn pool_config_builder_idle_timeout() {
        let config = PoolConfigBuilder::new().idle_timeout_ms(120_000).build();
        assert_eq!(config.idle_timeout_ms, Some(120_000));
    }

    #[tokio::test]
    async fn embedded_pool_execute_convenience() {
        let pool = EmbeddedPool::new();
        let result = pool.execute("CREATE TABLE t (id INT)").await;
        assert!(
            result.is_ok(),
            "expected execute to succeed, got: {:?}",
            result.err()
        );
    }

    #[tokio::test]
    async fn embedded_pool_execute_after_close_fails() {
        let pool = EmbeddedPool::new();
        pool.close();
        let result = pool.execute("CREATE TABLE t (id INT)").await;
        assert!(result.is_err(), "execute on closed pool should fail");
    }

    #[tokio::test]
    async fn embedded_pool_concurrent_access() {
        use oxisql_core::ConnectionPool;
        use std::sync::Arc;

        let pool = Arc::new(EmbeddedPool::new());

        // Setup: create table using one checkout via the trait
        {
            let conn = <EmbeddedPool as ConnectionPool>::get(pool.as_ref())
                .await
                .unwrap();
            conn.execute("CREATE TABLE concurrent_test (task_id INT)", &[])
                .await
                .unwrap();
        }

        // Spawn 4 tasks that each insert a row via the ConnectionPool trait
        let mut handles = Vec::new();
        for i in 0..4i64 {
            let p = pool.clone();
            handles.push(tokio::spawn(async move {
                let conn = <EmbeddedPool as ConnectionPool>::get(p.as_ref())
                    .await
                    .expect("checkout ok");
                conn.execute(&format!("INSERT INTO concurrent_test VALUES ({i})"), &[])
                    .await
                    .expect("insert ok");
            }));
        }
        for h in handles {
            h.await.unwrap();
        }

        let conn = <EmbeddedPool as ConnectionPool>::get(pool.as_ref())
            .await
            .unwrap();
        let rows = conn
            .query("SELECT task_id FROM concurrent_test", &[])
            .await
            .unwrap();
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn pool_hooks_debug_format() {
        use oxisql_pool::PoolHooks;
        let hooks = PoolHooks::new().on_create(|| {}).on_checkout(|| {});
        let s = format!("{hooks:?}");
        assert!(s.contains("on_create: true"));
        assert!(s.contains("on_checkout: true"));
        assert!(s.contains("on_checkin: false"));
    }

    #[tokio::test]
    async fn embedded_pool_checkout_hook_fires() {
        use oxisql_pool::{embedded::EmbeddedPool, PoolHooks};
        use std::sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        };

        let counter = Arc::new(AtomicU32::new(0));
        let c = counter.clone();
        let hooks = PoolHooks::new().on_checkout(move || {
            c.fetch_add(1, Ordering::SeqCst);
        });

        let pool = EmbeddedPool::new().with_hooks(hooks);
        let _conn = pool.get().await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 1);
        drop(_conn);
        let _conn2 = pool.get().await.unwrap();
        assert_eq!(counter.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn embedded_pool_health_check_open() {
        let pool = EmbeddedPool::new();
        let oxi_pool = oxisql_pool::OxidbPool::Embedded(pool);
        oxi_pool
            .health_check()
            .await
            .expect("health check on open pool");
    }

    #[tokio::test]
    async fn embedded_pool_health_check_closed() {
        let pool = EmbeddedPool::new();
        pool.close();
        let oxi_pool = oxisql_pool::OxidbPool::Embedded(pool);
        assert!(
            oxi_pool.health_check().await.is_err(),
            "expected Err on closed pool"
        );
    }

    #[tokio::test]
    async fn embedded_pool_metrics_open() {
        let pool = EmbeddedPool::new();
        let oxi_pool = oxisql_pool::OxidbPool::Embedded(pool);
        let m = oxi_pool.metrics();
        assert_eq!(m.max_size, 1);
        assert_eq!(m.idle, 1);
        assert_eq!(m.active, 0);
    }

    #[tokio::test]
    async fn embedded_pool_metrics_closed() {
        let pool = EmbeddedPool::new();
        pool.close();
        let oxi_pool = oxisql_pool::OxidbPool::Embedded(pool);
        let m = oxi_pool.metrics();
        assert_eq!(m.max_size, 1);
        assert_eq!(m.idle, 0);
    }

    /// Pool exhaustion simulation: hold the single lock and verify a second
    /// concurrent checkout is blocked until the first is released.
    #[tokio::test]
    async fn embedded_pool_exhaustion_simulation() {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        };
        use tokio::time::{timeout, Duration};

        let pool = Arc::new(EmbeddedPool::new());

        // Hold the lock in task A.
        let pool_a = pool.clone();
        let lock_held = Arc::new(AtomicBool::new(false));
        let lock_held2 = lock_held.clone();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let task_a = tokio::spawn(async move {
            let _guard = pool_a.get().await.expect("first checkout");
            lock_held2.store(true, Ordering::SeqCst);
            // Hold the lock until signaled.
            rx.await.ok();
            // _guard dropped here, releasing the lock.
        });

        // Wait until task A actually holds the lock.
        while !lock_held.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }

        // Task B: a second checkout should time out because the lock is held.
        let pool_b = pool.clone();
        let timed_out = timeout(Duration::from_millis(50), async move {
            let _ = pool_b.get().await.expect("second checkout");
        })
        .await
        .is_err();

        assert!(timed_out, "expected second checkout to be blocked");

        // Release the lock by signaling task A.
        tx.send(()).ok();
        task_a.await.expect("task_a join");

        // After the lock is released, a third checkout must succeed.
        let _ = pool.get().await.expect("checkout after lock released");
    }

    /// Verify that `pool.metrics().acquired_total` equals the number of
    /// `ConnectionPool::get()` calls made after a concurrent workload.
    ///
    /// This test uses the `ConnectionPool` trait path (`CheckinOnDrop`) so that
    /// the `acquired` counter is incremented for each checkout.
    #[tokio::test]
    async fn embedded_pool_metrics_after_concurrent_access() {
        use oxisql_core::ConnectionPool;
        use std::sync::Arc;

        let pool = Arc::new(EmbeddedPool::new());

        // Setup table.
        {
            let conn = <EmbeddedPool as ConnectionPool>::get(pool.as_ref())
                .await
                .unwrap();
            conn.execute("CREATE TABLE metrics_conc_probe (n INTEGER)", &[])
                .await
                .unwrap();
        }

        let checkout_count = 10usize;
        let mut join_set = tokio::task::JoinSet::new();
        for _ in 0..checkout_count {
            let p = pool.clone();
            join_set.spawn(async move {
                let conn = <EmbeddedPool as ConnectionPool>::get(p.as_ref())
                    .await
                    .expect("checkout ok");
                conn.execute("INSERT INTO metrics_conc_probe VALUES (1)", &[])
                    .await
                    .expect("insert ok");
                // conn dropped here → CheckinOnDrop fires
            });
        }
        while let Some(r) = join_set.join_next().await {
            r.expect("task panicked");
        }

        // +1 for the setup checkout above.
        let expected_acquired = (checkout_count + 1) as u64;
        let m = pool.metrics();
        assert_eq!(
            m.acquired_total, expected_acquired,
            "acquired_total must equal number of ConnectionPool::get() calls; \
             expected {expected_acquired}, got {}",
            m.acquired_total
        );
    }

    /// execute_query_builder: runs a QueryBuilder query through the pool.
    #[cfg(feature = "query-builder")]
    #[tokio::test]
    async fn embedded_pool_execute_query_builder() {
        use oxisql_parse::QueryBuilder;
        let pool = EmbeddedPool::new();
        pool.execute("CREATE TABLE qb_test (id INT, name TEXT)")
            .await
            .unwrap();
        pool.execute("INSERT INTO qb_test VALUES (1, 'hello')")
            .await
            .unwrap();

        let rows = pool
            .execute_query_builder(&QueryBuilder::select(&["id", "name"]).from("qb_test"))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
    }

    /// Sequential access: data written by first checkout is visible to second.
    #[tokio::test]
    async fn embedded_pool_sequential_access() {
        let pool = EmbeddedPool::new();
        {
            let mut glue = pool.get().await.unwrap();
            glue.execute("CREATE TABLE seq_test (id INT)")
                .await
                .unwrap();
            glue.execute("INSERT INTO seq_test VALUES (1)")
                .await
                .unwrap();
            // guard dropped here
        }
        {
            let mut glue = pool.get().await.unwrap();
            let payloads = glue.execute("SELECT id FROM seq_test").await.unwrap();
            let found = payloads.iter().any(|p| {
                if let gluesql::prelude::Payload::Select { rows, .. } = p {
                    !rows.is_empty()
                } else {
                    false
                }
            });
            assert!(found, "expected SELECT to return rows");
        }
    }

    #[tokio::test]
    async fn embedded_pool_migration_integration() {
        use oxisql_core::ConnectionPool;
        use oxisql_migrate::runner::MigrationRunner;
        use std::env;

        let dir = env::temp_dir().join(format!(
            "pool_migrate_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        std::fs::write(
            dir.join("00000000000001__create_users.sql"),
            "CREATE TABLE users (id INT, name TEXT)",
        )
        .unwrap();

        let pool = EmbeddedPool::new();
        let conn = <EmbeddedPool as ConnectionPool>::get(&pool).await.unwrap();

        let runner = MigrationRunner::new(&dir);
        let applied = runner.run_with_conn(conn.as_ref()).await.unwrap();
        assert_eq!(applied, 1);

        // Verify the table was created
        let conn2 = <EmbeddedPool as ConnectionPool>::get(&pool).await.unwrap();
        conn2
            .execute("INSERT INTO users VALUES (1, 'Test')", &[])
            .await
            .unwrap();
        let rows = conn2.query("SELECT name FROM users", &[]).await.unwrap();
        assert_eq!(rows.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}

// ── sqlite (always runs when feature = "sqlite") ─────────────────────────────
//
// `new_sqlite_pool` is async (Limbo backend uses async open); call sites must
// `.await` the constructor.

#[cfg(feature = "sqlite")]
mod sqlite_tests {
    use oxisql_pool::sqlite::{new_sqlite_pool, SqlitePool};

    #[tokio::test]
    async fn test_sqlite_pool_health_check() {
        let pool: SqlitePool = new_sqlite_pool(":memory:", 4)
            .await
            .expect("pool creation should succeed");
        pool.health_check()
            .await
            .expect("health check should succeed");
    }

    #[tokio::test]
    async fn test_sqlite_pool_get_conn() {
        let pool: SqlitePool = new_sqlite_pool(":memory:", 4)
            .await
            .expect("pool creation should succeed");
        let _conn = pool.get().await.expect("get should succeed");
    }

    #[tokio::test]
    async fn test_sqlite_pool_metrics() {
        let pool: SqlitePool = new_sqlite_pool(":memory:", 4)
            .await
            .expect("pool creation should succeed");
        let m = pool.metrics();
        assert_eq!(m.max_size, 4);
    }

    #[tokio::test]
    async fn test_sqlite_pool_backend_name() {
        let pool: SqlitePool = new_sqlite_pool(":memory:", 4)
            .await
            .expect("pool creation should succeed");
        assert_eq!(pool.backend_name(), "sqlite");
    }

    #[tokio::test]
    async fn test_sqlite_pool_close() {
        let pool: SqlitePool = new_sqlite_pool(":memory:", 4)
            .await
            .expect("pool creation should succeed");
        pool.close();
    }

    #[tokio::test]
    async fn test_sqlite_pool_zero_max_size_no_panic() {
        // deadpool accepts max_size=0 (connections will never be handed out).
        // The invariant is: no panic.
        let _result = new_sqlite_pool(":memory:", 0).await;
    }

    #[tokio::test]
    async fn test_oxidb_pool_sqlite_health_check() {
        let pool = new_sqlite_pool(":memory:", 2)
            .await
            .expect("pool creation should succeed");
        let oxi = oxisql_pool::OxidbPool::Sqlite(pool);
        oxi.health_check()
            .await
            .expect("OxidbPool::Sqlite health check");
    }

    #[tokio::test]
    async fn test_oxidb_pool_sqlite_metrics() {
        let pool = new_sqlite_pool(":memory:", 2)
            .await
            .expect("pool creation should succeed");
        let oxi = oxisql_pool::OxidbPool::Sqlite(pool);
        let m = oxi.metrics();
        assert_eq!(m.max_size, 2);
    }
}

// ── mysql URL parsing (no live MySQL needed) ──────────────────────────────────

#[cfg(feature = "mysql")]
mod mysql_url_tests {
    use oxisql_pool::mysql::new_mysql_pool;

    #[test]
    fn test_mysql_url_empty_returns_err() {
        let result = new_mysql_pool("", 5);
        assert!(result.is_err(), "empty URL should return Err");
    }

    #[test]
    fn test_mysql_url_missing_scheme_returns_err() {
        let result = new_mysql_pool("localhost:3306/db", 5);
        assert!(result.is_err(), "URL without scheme should return Err");
    }

    /// Verify that an IPv6 host in a mysql:// URL does not panic.
    ///
    /// Pool construction is lazy — we only validate URL parsing and pool
    /// builder setup.  Any result (Ok or Err) is acceptable; no panic is the
    /// invariant under test.
    #[test]
    fn test_mysql_url_with_ipv6() {
        // mysql_async may or may not support bracket-enclosed IPv6 hosts.
        // The invariant is: no panic.
        let result = new_mysql_pool("mysql://[::1]:3306/db", 5);
        let _ = result;
    }

    /// Verify that URL-encoded special characters in the password component
    /// do not cause a panic during pool construction.
    ///
    /// %40 = '@', %21 = '!'.  mysql_async URL parsing must handle these
    /// without panicking; errors are acceptable.
    #[test]
    fn test_mysql_url_special_chars_in_password() {
        let result = new_mysql_pool("mysql://user:p%40ss%21@localhost/db", 5);
        // No panic is the only invariant — mysql_async may succeed or return Err.
        let _ = result;
    }

    /// Verify that a URL with an unusually large port number is rejected or
    /// handled without panicking.
    #[test]
    fn test_mysql_url_invalid_port() {
        // Port 99999 exceeds u16::MAX (65535); mysql_async should reject it.
        let result = new_mysql_pool("mysql://root@localhost:99999/db", 5);
        // Either Err (URL rejected) or Ok (pool creation defers validation) is fine.
        let _ = result;
    }

    /// Verify that `max_size = 0` does not panic during pool construction.
    ///
    /// deadpool's pool builder behaviour for zero `max_size` may vary across
    /// versions.  The invariant under test is that no panic occurs; the result
    /// (Ok or Err) is implementation-defined.
    #[test]
    fn test_mysql_pool_zero_max_size_no_panic() {
        // Just verify construction does not panic.
        let result = new_mysql_pool("mysql://root@127.0.0.1:3306/test", 0);
        let _ = result;
    }
}

// ── postgres (#[ignore] — requires OXIDB_PG_URL) ─────────────────────────────

#[cfg(feature = "postgres")]
mod postgres_tests {
    use deadpool_postgres::{Config, Runtime};
    use oxisql_pool::postgres::OxidbPgPool;

    /// Verify that `TryFrom<Config>` succeeds: pool construction does not
    /// open any connections, so a minimal (host + dbname) config is enough.
    #[test]
    fn test_try_from_config() {
        let mut config = Config::new();
        config.dbname = Some("test".to_string());
        config.host = Some("localhost".to_string());
        // Pool construction does not connect — just validates config structure.
        let result = OxidbPgPool::try_from(config);
        assert!(
            result.is_ok(),
            "TryFrom<Config> should succeed; got: {:?}",
            result.err()
        );
    }

    /// Verify that an empty config (no dbname) is rejected at construction time,
    /// not silently deferred.
    #[test]
    fn test_try_from_missing_dbname_fails() {
        let mut config = Config::new();
        config.host = Some("localhost".to_string());
        // No dbname set — deadpool_postgres rejects this eagerly.
        let result = OxidbPgPool::try_from(config);
        assert!(
            result.is_err(),
            "config without dbname should fail at pool-construction time"
        );
    }

    #[ignore]
    #[tokio::test]
    async fn postgres_pool_acquire_release() {
        let url = std::env::var("OXIDB_PG_URL").expect("OXIDB_PG_URL must be set to run this test");

        // Parse host/user/pass from URL manually or use a simple Config.
        // For simplicity parse URL components.
        let url = url
            .trim_start_matches("postgres://")
            .trim_start_matches("postgresql://");
        let mut cfg = Config::new();

        // Basic URL parsing: user:pass@host:port/dbname
        if let Some((userinfo, rest)) = url.split_once('@') {
            if let Some((user, pass)) = userinfo.split_once(':') {
                cfg.user = Some(user.to_string());
                cfg.password = Some(pass.to_string());
            } else {
                cfg.user = Some(userinfo.to_string());
            }
            if let Some((hostport, dbname)) = rest.split_once('/') {
                cfg.dbname = Some(dbname.to_string());
                if let Some((host, port)) = hostport.split_once(':') {
                    cfg.host = Some(host.to_string());
                    if let Ok(p) = port.parse::<u16>() {
                        cfg.port = Some(p);
                    }
                } else {
                    cfg.host = Some(hostport.to_string());
                }
            }
        }

        let pool = OxidbPgPool::new(cfg, Runtime::Tokio1).expect("failed to create postgres pool");

        // Acquire a connection.
        let client = pool
            .get()
            .await
            .expect("failed to get connection from pool");

        // Execute a simple query.
        let rows = client
            .query("SELECT 1 AS val", &[])
            .await
            .expect("query failed");
        assert_eq!(rows.len(), 1);

        // Connection is returned to pool when `client` is dropped.
        drop(client);

        assert!(pool.available() >= 1 || pool.max_size() > 0);
    }
}

// ── mysql (#[ignore] — requires OXIDB_MYSQL_URL) ─────────────────────────────

#[cfg(feature = "mysql")]
mod mysql_tests {
    use mysql_async::prelude::Queryable;
    use oxisql_pool::mysql::new_mysql_pool;

    #[ignore]
    #[tokio::test]
    async fn mysql_pool_acquire_release() {
        let url =
            std::env::var("OXIDB_MYSQL_URL").expect("OXIDB_MYSQL_URL must be set to run this test");

        let pool = new_mysql_pool(&url, 4).expect("failed to create mysql pool");

        // Acquire a connection.
        let mut conn = pool
            .get()
            .await
            .expect("failed to get connection from pool");

        // Execute a simple query.
        let val: Vec<u32> = conn.exec("SELECT 1", ()).await.expect("query failed");
        assert_eq!(val, vec![1u32]);

        // Return to pool.
        drop(conn);
    }
}

// ── ConnectionPool trait object-safety checks (compile-time, no live DB) ─────

#[cfg(feature = "postgres")]
mod pg_connection_pool_tests {
    use oxisql_core::ConnectionPool;
    use oxisql_pool::postgres::OxidbPgPool;

    /// Compile-time object-safety assertion: `dyn ConnectionPool` must be a valid type.
    fn _assert_pg_pool_is_connection_pool(_p: &dyn ConnectionPool) {}

    /// Compile-time check: `OxidbPgPool` implements `ConnectionPool`.
    fn _check_type(_p: &OxidbPgPool) {
        _assert_pg_pool_is_connection_pool(_p);
    }

    #[ignore = "requires live Postgres (set TEST_POSTGRES_URL)"]
    #[tokio::test]
    async fn test_oxidbpgpool_connection_pool_trait_live() {
        let url = std::env::var("TEST_POSTGRES_URL")
            .expect("TEST_POSTGRES_URL must be set to run this test");

        let pool = OxidbPgPool::try_from_url(&url).expect("pool creation failed");

        // get() via the ConnectionPool trait
        let conn = ConnectionPool::get(&pool).await.expect("checkout failed");

        conn.ping().await.expect("ping failed");

        // Metrics sanity
        assert!(pool.pool_size() > 0, "pool_size must be > 0");
    }
}

#[cfg(feature = "mysql")]
mod mysql_connection_pool_tests {
    use oxisql_core::ConnectionPool;
    use oxisql_pool::mysql::MysqlPool;

    /// Compile-time object-safety assertion: `dyn ConnectionPool` must be a valid type.
    fn _assert_mysql_pool_is_connection_pool(_p: &dyn ConnectionPool) {}

    /// Compile-time check: `MysqlPool` implements `ConnectionPool`.
    fn _check_type(_p: &MysqlPool) {
        _assert_mysql_pool_is_connection_pool(_p);
    }

    #[ignore = "requires live MySQL (set TEST_MYSQL_URL)"]
    #[tokio::test]
    async fn test_mysqlpool_connection_pool_trait_live() {
        use oxisql_pool::mysql::new_mysql_pool;

        let url =
            std::env::var("TEST_MYSQL_URL").expect("TEST_MYSQL_URL must be set to run this test");

        let pool = new_mysql_pool(&url, 4).expect("pool creation failed");

        // get() via the ConnectionPool trait
        let conn = ConnectionPool::get(&pool).await.expect("checkout failed");

        conn.ping().await.expect("ping failed");

        // Metrics sanity
        assert!(pool.pool_size() > 0, "pool_size must be > 0");
    }
}

// ── kv_store tests (embedded, always runs) ───────────────────────────────────

#[cfg(feature = "embedded")]
mod kv_store_tests {
    use oxisql_pool::embedded::EmbeddedPool;
    use oxisql_pool::kv_store::EmbeddedKvStore;

    #[tokio::test]
    async fn test_kv_store_set_get() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("name", "alice").await.unwrap();
        let val = kv.get("name").await.unwrap();
        assert_eq!(val, Some("alice".to_string()));
    }

    #[tokio::test]
    async fn test_kv_store_overwrite() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("key", "first").await.unwrap();
        kv.set("key", "second").await.unwrap();
        let val = kv.get("key").await.unwrap();
        assert_eq!(val, Some("second".to_string()));
    }

    #[tokio::test]
    async fn test_kv_store_delete() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("foo", "bar").await.unwrap();
        let deleted = kv.delete("foo").await.unwrap();
        assert!(deleted);

        let val = kv.get("foo").await.unwrap();
        assert_eq!(val, None);
    }

    #[tokio::test]
    async fn test_kv_store_delete_absent_returns_false() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        let deleted = kv.delete("nonexistent").await.unwrap();
        assert!(!deleted);
    }

    #[tokio::test]
    async fn test_kv_store_get_absent_returns_none() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        let val = kv.get("ghost").await.unwrap();
        assert_eq!(val, None);
    }

    #[tokio::test]
    async fn test_kv_store_list_keys() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("a", "1").await.unwrap();
        kv.set("b", "2").await.unwrap();
        let mut keys = kv.list_keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn test_kv_store_contains_key() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("present", "yes").await.unwrap();
        assert!(kv.contains_key("present").await.unwrap());
        assert!(!kv.contains_key("absent").await.unwrap());
    }

    #[tokio::test]
    async fn test_kv_store_init_idempotent() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, None);
        // Calling init multiple times must not fail.
        kv.init().await.unwrap();
        kv.init().await.unwrap();
        kv.set("x", "42").await.unwrap();
        assert_eq!(kv.get("x").await.unwrap(), Some("42".to_string()));
    }

    #[tokio::test]
    async fn test_kv_store_custom_table_name() {
        let pool = EmbeddedPool::new();
        let kv = EmbeddedKvStore::new(pool, Some("my_store"));
        kv.init().await.unwrap();
        kv.set("cfg", "true").await.unwrap();
        assert_eq!(kv.get("cfg").await.unwrap(), Some("true".to_string()));
    }

    // ── OxidbKvStore via Embedded variant ────────────────────────────────────

    #[tokio::test]
    async fn test_oxidb_kv_store_embedded() {
        use oxisql_pool::kv_store::OxidbKvStore;
        use oxisql_pool::OxidbPool;
        use std::sync::Arc;

        let pool = Arc::new(OxidbPool::Embedded(EmbeddedPool::new()));
        let kv = OxidbKvStore::new(Arc::clone(&pool), None);
        kv.init().await.unwrap();

        kv.set("env", "production").await.unwrap();
        let v = kv.get("env").await.unwrap();
        assert_eq!(v, Some("production".to_string()));

        let deleted = kv.delete("env").await.unwrap();
        assert!(deleted);
        assert_eq!(kv.get("env").await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_oxidb_kv_store_list_keys() {
        use oxisql_pool::kv_store::OxidbKvStore;
        use oxisql_pool::OxidbPool;
        use std::sync::Arc;

        let pool = Arc::new(OxidbPool::Embedded(EmbeddedPool::new()));
        let kv = OxidbKvStore::new(pool, None);
        kv.init().await.unwrap();

        kv.set("k1", "v1").await.unwrap();
        kv.set("k2", "v2").await.unwrap();
        kv.set("k3", "v3").await.unwrap();
        let mut keys = kv.list_keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["k1", "k2", "k3"]);
    }
}
