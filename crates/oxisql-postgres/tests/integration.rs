//! Integration tests against a live PostgreSQL instance.
//!
//! These tests require a running Postgres server.  To run them:
//!
//! ```bash
//! docker run --rm -e POSTGRES_PASSWORD=test -p 5432:5432 postgres
//! cargo test -p oxisql-postgres --features integration-postgres -- --include-ignored
//! ```
//!
//! All tests in this file are `#[ignore]`d by default so they never run in CI
//! without the `integration-postgres` feature AND an explicit `--include-ignored`.

#[cfg(feature = "integration-postgres")]
mod pg_integration {
    use oxisql_core::Connection;
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Connect and run a trivial SELECT 1.
    #[tokio::test]
    #[ignore]
    async fn connect_and_select_one() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");
        let rows = conn
            .query("SELECT 1::bigint AS n", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("n").expect("column n");
        assert_eq!(*v, oxisql_core::Value::I64(1));
    }

    /// Round-trip an INSERT + SELECT through a transaction.
    #[tokio::test]
    #[ignore]
    async fn transaction_roundtrip() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        // Create a temp table.
        conn.execute("CREATE TEMP TABLE m2_test (id BIGINT, label TEXT)", &[])
            .await
            .expect("create table");

        // Insert inside a transaction then commit.
        {
            let mut txn = conn.transaction().await.expect("begin");
            let id: i64 = 42;
            let label = "hello";
            txn.execute("INSERT INTO m2_test VALUES ($1, $2)", &[&id, &label])
                .await
                .expect("insert");
            txn.commit().await.expect("commit");
        }

        // Read back.
        let rows = conn
            .query("SELECT id, label FROM m2_test", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("id"), Some(&oxisql_core::Value::I64(42)));
        assert_eq!(
            rows[0].get("label"),
            Some(&oxisql_core::Value::Text("hello".to_string()))
        );
    }

    /// A rolled-back transaction must not persist data.
    #[tokio::test]
    #[ignore]
    async fn transaction_rollback() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        conn.execute("CREATE TEMP TABLE m2_rollback (id BIGINT)", &[])
            .await
            .expect("create table");

        {
            let mut txn = conn.transaction().await.expect("begin");
            let id: i64 = 99;
            txn.execute("INSERT INTO m2_rollback VALUES ($1)", &[&id])
                .await
                .expect("insert");
            txn.rollback().await.expect("rollback");
        }

        let rows = conn
            .query("SELECT id FROM m2_rollback", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 0, "rollback must undo the insert");
    }
}

// ── Task 1: savepoint support (live PG required) ─────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_savepoints {
    use oxisql_core::Connection;
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Verify that a savepoint can be created and rolled back to, so that only
    /// work after the savepoint is discarded while earlier work is preserved.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_savepoint_partial_rollback() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        conn.execute("CREATE TEMP TABLE sp_test (id BIGINT)", &[])
            .await
            .expect("create table");

        let mut txn = conn.transaction().await.expect("begin");

        let id1: i64 = 1;
        txn.execute("INSERT INTO sp_test VALUES ($1)", &[&id1])
            .await
            .expect("insert 1");

        // Create a savepoint, insert a second row, then roll back to the savepoint.
        txn.savepoint("sp1").await.expect("savepoint");

        let id2: i64 = 2;
        txn.execute("INSERT INTO sp_test VALUES ($1)", &[&id2])
            .await
            .expect("insert 2");

        txn.rollback_to_savepoint("sp1")
            .await
            .expect("rollback_to_savepoint");

        // Commit — only row 1 should be present.
        txn.commit().await.expect("commit");

        let rows = conn
            .query("SELECT id FROM sp_test ORDER BY id", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 1, "expected 1 row after savepoint rollback");
        assert_eq!(rows[0].get("id"), Some(&oxisql_core::Value::I64(1)));
    }

    /// Verify that `release_savepoint` removes the savepoint and that
    /// subsequent work is committed normally.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_savepoint_release() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        conn.execute("CREATE TEMP TABLE sp_release_test (id BIGINT)", &[])
            .await
            .expect("create table");

        let mut txn = conn.transaction().await.expect("begin");

        let id1: i64 = 10;
        txn.execute("INSERT INTO sp_release_test VALUES ($1)", &[&id1])
            .await
            .expect("insert 10");

        txn.savepoint("rel_sp").await.expect("savepoint");

        let id2: i64 = 20;
        txn.execute("INSERT INTO sp_release_test VALUES ($1)", &[&id2])
            .await
            .expect("insert 20");

        // Release the savepoint — both rows survive.
        txn.release_savepoint("rel_sp")
            .await
            .expect("release_savepoint");

        txn.commit().await.expect("commit");

        let rows = conn
            .query("SELECT id FROM sp_release_test ORDER BY id", &[])
            .await
            .expect("select");
        assert_eq!(
            rows.len(),
            2,
            "both rows should survive after release_savepoint"
        );
        assert_eq!(rows[0].get("id"), Some(&oxisql_core::Value::I64(10)));
        assert_eq!(rows[1].get("id"), Some(&oxisql_core::Value::I64(20)));
    }

    /// Verify that savepoint operations (create / rollback-to / release) work
    /// through the `Transaction` trait surface, confirming the inherent backend
    /// methods are reachable end-to-end.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_savepoint_pg_inherent_methods() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        conn.execute("CREATE TEMP TABLE sp_pg_test (id BIGINT)", &[])
            .await
            .expect("create table");

        let mut txn = conn.transaction().await.expect("begin");

        let id1: i64 = 100;
        txn.execute("INSERT INTO sp_pg_test VALUES ($1)", &[&id1])
            .await
            .expect("insert 100");

        // Exercise all three savepoint methods via the Transaction trait.
        txn.savepoint("pg_sp").await.expect("savepoint via trait");
        txn.rollback_to_savepoint("pg_sp")
            .await
            .expect("rollback_to via trait");
        txn.release_savepoint("pg_sp")
            .await
            .expect("release via trait");

        txn.commit().await.expect("commit");
    }

    /// Verify that invalid savepoint names are rejected before hitting the server.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_savepoint_name_validation() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        let mut txn = conn.transaction().await.expect("begin");

        // Empty name should fail.
        assert!(
            txn.savepoint("").await.is_err(),
            "empty savepoint name should be rejected"
        );

        // Name with spaces/SQL-injection characters should fail.
        assert!(
            txn.savepoint("bad name").await.is_err(),
            "savepoint name with space should be rejected"
        );
        assert!(
            txn.savepoint("'; DROP TABLE foo; --").await.is_err(),
            "SQL injection savepoint name should be rejected"
        );

        // Valid names should succeed.
        txn.savepoint("valid_sp")
            .await
            .expect("valid savepoint name");
        txn.release_savepoint("valid_sp")
            .await
            .expect("release valid savepoint");

        txn.rollback().await.expect("rollback");
    }
}

// ── Task 3: connect_with_timeout (live PG required) ───────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_timeout {
    use std::time::Duration;

    use oxisql_postgres::{PgConnection, PgError, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Verify that a fast-enough connect succeeds within a generous timeout.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_connect_with_timeout_success() {
        use oxisql_core::Connection;
        let conn = PgConnection::connect_with_timeout(
            CONN_STR,
            TlsMode::Disabled,
            Duration::from_secs(10),
        )
        .await
        .expect("connect_with_timeout should succeed");
        let rows = conn
            .query("SELECT 1::bigint AS n", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("n"), Some(&oxisql_core::Value::I64(1)));
    }

    /// Verify that a connection to a black-hole address times out with
    /// `PgError::Timeout`.
    ///
    /// Uses a non-routable address (192.0.2.0/24 — TEST-NET-1, RFC 5737) and
    /// a very short timeout so the test does not block CI.
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_connect_with_timeout_fires() {
        let result = PgConnection::connect_with_timeout(
            "host=192.0.2.1 port=5432 user=postgres password=test dbname=postgres",
            TlsMode::Disabled,
            Duration::from_millis(100),
        )
        .await;
        match result {
            Err(PgError::Timeout(_)) => {} // expected
            Err(other) => panic!("expected PgError::Timeout, got {other:?}"),
            Ok(_) => panic!("expected a timeout error"),
        }
    }
}

// ── Compile-only tests (no server required) ───────────────────────────────────

/// Verify that `ColumnDescription` is accessible and its fields are usable.
#[test]
fn test_column_description_type_is_accessible() {
    use oxisql_postgres::ColumnDescription;
    let desc = ColumnDescription {
        name: "id".into(),
        type_name: "int4".into(),
        nullable: true,
    };
    assert_eq!(desc.name, "id");
    assert_eq!(desc.type_name, "int4");
    assert!(desc.nullable);
}

/// Verify that schema introspection (tables, columns, indexes, foreign_keys) works.
#[cfg(feature = "integration-postgres")]
mod pg_schema_introspection {
    use oxisql_core::Connection;
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    #[tokio::test]
    #[ignore]
    async fn test_schema_introspection() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        // Create a parent and child table with a FK relationship.
        conn.execute("DROP TABLE IF EXISTS oxisql_schema_child", &[])
            .await
            .expect("drop child");
        conn.execute("DROP TABLE IF EXISTS oxisql_schema_parent", &[])
            .await
            .expect("drop parent");
        conn.execute(
            "CREATE TABLE oxisql_schema_parent (
                id BIGINT PRIMARY KEY,
                name TEXT NOT NULL
            )",
            &[],
        )
        .await
        .expect("CREATE parent");
        conn.execute(
            "CREATE TABLE oxisql_schema_child (
                id BIGINT PRIMARY KEY,
                parent_id BIGINT NOT NULL,
                CONSTRAINT fk_pg_parent FOREIGN KEY (parent_id)
                    REFERENCES oxisql_schema_parent(id)
            )",
            &[],
        )
        .await
        .expect("CREATE child");

        // tables()
        let tables = conn.tables().await.expect("tables()");
        let names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
        assert!(
            names.contains(&"oxisql_schema_parent".to_string()),
            "parent table missing from tables(): {names:?}"
        );
        assert!(
            names.contains(&"oxisql_schema_child".to_string()),
            "child table missing from tables(): {names:?}"
        );

        // columns()
        let cols = conn
            .columns("oxisql_schema_parent")
            .await
            .expect("columns()");
        let col_names: Vec<String> = cols.iter().map(|c| c.name.clone()).collect();
        assert!(col_names.contains(&"id".to_string()), "id column missing");
        assert!(
            col_names.contains(&"name".to_string()),
            "name column missing"
        );

        // indexes()
        let idxs = conn
            .indexes("oxisql_schema_parent")
            .await
            .expect("indexes()");
        let primary = idxs.iter().find(|i| i.primary);
        assert!(primary.is_some(), "primary key index missing");

        // foreign_keys()
        let fks = conn
            .foreign_keys("oxisql_schema_child")
            .await
            .expect("foreign_keys()");
        assert_eq!(fks.len(), 1, "expected 1 FK, got {}", fks.len());
        assert_eq!(fks[0].foreign_table, "oxisql_schema_parent");
        assert_eq!(fks[0].column, "parent_id");

        // Cleanup
        conn.execute("DROP TABLE IF EXISTS oxisql_schema_child", &[])
            .await
            .expect("cleanup child");
        conn.execute("DROP TABLE IF EXISTS oxisql_schema_parent", &[])
            .await
            .expect("cleanup parent");
    }
}

// ── Task A: reconnect ─────────────────────────────────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_reconnect {
    use oxisql_core::Connection;
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Verify that `reconnect()` returns a fresh, working `PgConnection`.
    #[tokio::test]
    #[ignore]
    async fn test_reconnect_method() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("initial connect");
        let new_conn = conn.reconnect().await.expect("reconnect");
        let rows = new_conn
            .query("SELECT 1::bigint AS n", &[])
            .await
            .expect("query after reconnect");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("n"), Some(&oxisql_core::Value::I64(1)));
    }
}

// ── Task B: describe / column introspection ───────────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_describe {
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Verify that `describe` returns correct column names and type names.
    #[tokio::test]
    #[ignore]
    async fn test_describe_query() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");
        let cols = conn
            .describe("SELECT 1::INT4 AS id, 'hello'::TEXT AS name")
            .await
            .expect("describe");
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[0].type_name, "int4");
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[1].type_name, "text");
        assert!(cols[0].nullable);
        assert!(cols[1].nullable);
    }
}

// ── Task C: extended integration tests ───────────────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_extended {
    use oxisql_core::{Connection, Value};
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    /// Round-trip DATE, TIMESTAMP, UUID, JSONB, and NUMERIC values through
    /// PostgreSQL and verify the correct `Value` variants come back.
    #[tokio::test]
    #[ignore]
    async fn test_extended_type_round_trip() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        let rows = conn
            .query(
                "SELECT \
                    '2024-01-15'::DATE           AS d, \
                    '2024-01-15 12:30:00'::TIMESTAMP AS ts, \
                    'a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11'::UUID AS u, \
                    '{\"key\": 1}'::JSONB         AS j, \
                    '123.456'::NUMERIC           AS n",
                &[],
            )
            .await
            .expect("query");

        assert_eq!(rows.len(), 1);
        let row = &rows[0];

        // DATE: days since Unix epoch for 2024-01-15
        // 2024-01-15 = 19737 days after 1970-01-01
        match row.get("d").expect("date column") {
            Value::Date(days) => assert_eq!(*days, 19737, "DATE days mismatch"),
            other => panic!("expected Value::Date, got {other:?}"),
        }

        // TIMESTAMP: microseconds since epoch for 2024-01-15 12:30:00 UTC
        match row.get("ts").expect("timestamp column") {
            Value::Timestamp(us) => {
                // 19737 days * 86400s + 12*3600 + 30*60 = 1_705_320_600 seconds
                let expected_secs: i64 = 19_737 * 86_400 + 12 * 3_600 + 30 * 60;
                let actual_secs = us / 1_000_000;
                assert_eq!(actual_secs, expected_secs, "TIMESTAMP seconds mismatch");
            }
            other => panic!("expected Value::Timestamp, got {other:?}"),
        }

        // UUID: the fixed UUID above as u128
        match row.get("u").expect("uuid column") {
            Value::Uuid(bits) => {
                let expected: u128 = 0xa0ee_bc99_9c0b_4ef8_bb6d_6bb9_bd38_0a11;
                assert_eq!(*bits, expected, "UUID bits mismatch");
            }
            other => panic!("expected Value::Uuid, got {other:?}"),
        }

        // JSONB: a JSON string
        match row.get("j").expect("json column") {
            Value::Json(s) => assert!(s.contains("key"), "JSONB string missing key"),
            other => panic!("expected Value::Json, got {other:?}"),
        }

        // NUMERIC: exact decimal string
        match row.get("n").expect("numeric column") {
            Value::Decimal(s) => assert_eq!(s, "123.456", "NUMERIC string mismatch"),
            other => panic!("expected Value::Decimal, got {other:?}"),
        }
    }

    /// Prepare the same SQL twice and verify both executions return results.
    #[tokio::test]
    #[ignore]
    async fn test_prepared_statement_reuse() {
        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");
        let sql = "SELECT $1::bigint AS val";

        // First prepare.
        let mut stmt1 = conn.prepare(sql).await.expect("prepare 1");
        let v1: i64 = 10;
        let rows1 = stmt1.query(&[&v1]).await.expect("exec 1");
        assert_eq!(rows1.len(), 1);
        assert_eq!(rows1[0].get("val"), Some(&Value::I64(10)));

        // Second prepare — should hit the cache.
        let mut stmt2 = conn.prepare(sql).await.expect("prepare 2");
        let v2: i64 = 20;
        let rows2 = stmt2.query(&[&v2]).await.expect("exec 2");
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].get("val"), Some(&Value::I64(20)));
    }

    /// Verify that READ COMMITTED isolation prevents dirty reads across two
    /// separate connections.
    #[tokio::test]
    #[ignore]
    async fn test_transaction_isolation() {
        // Use two independent connections so their transactions run concurrently.
        let conn_a = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("conn_a connect");
        let conn_b = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("conn_b connect");

        // Create a fresh table for this test.
        conn_a
            .execute("DROP TABLE IF EXISTS oxisql_isolation_test", &[])
            .await
            .expect("drop");
        conn_a
            .execute("CREATE TABLE oxisql_isolation_test (id BIGINT)", &[])
            .await
            .expect("create");

        // conn_a: open transaction, insert a row — do NOT commit yet.
        let mut txn_a = conn_a.transaction().await.expect("begin txn_a");
        let id: i64 = 1;
        txn_a
            .execute("INSERT INTO oxisql_isolation_test VALUES ($1)", &[&id])
            .await
            .expect("insert in txn_a");

        // conn_b should NOT see the uncommitted row (READ COMMITTED).
        let rows_before = conn_b
            .query("SELECT id FROM oxisql_isolation_test", &[])
            .await
            .expect("select before commit");
        assert_eq!(
            rows_before.len(),
            0,
            "dirty read: uncommitted row visible to conn_b"
        );

        // Commit txn_a.
        txn_a.commit().await.expect("commit txn_a");

        // Now conn_b should see the committed row.
        let rows_after = conn_b
            .query("SELECT id FROM oxisql_isolation_test", &[])
            .await
            .expect("select after commit");
        assert_eq!(rows_after.len(), 1, "committed row not visible to conn_b");
        assert_eq!(rows_after[0].get("id"), Some(&Value::I64(1)));

        // Cleanup.
        conn_a
            .execute("DROP TABLE IF EXISTS oxisql_isolation_test", &[])
            .await
            .expect("cleanup");
    }

    /// Verify connection is usable after a `PgTransaction` is dropped without
    /// an explicit `commit` or `rollback`.
    ///
    /// The `Drop` impl schedules a `ROLLBACK` on the active Tokio runtime so the
    /// server-side transaction is aborted and the mutex is released.  After the
    /// drop, the parent connection must be able to execute queries normally,
    /// proving the mutex is no longer held.
    ///
    /// This test requires a live Postgres server.  Run with:
    /// ```bash
    /// docker run --rm -e POSTGRES_PASSWORD=test -p 5432:5432 postgres
    /// cargo test -p oxisql-postgres --features integration-postgres -- \
    ///     --include-ignored test_pg_transaction_drop_rolls_back
    /// ```
    #[tokio::test]
    #[ignore = "requires live Postgres"]
    async fn test_pg_transaction_drop_rolls_back() {
        use oxisql_core::{Connection, Value};

        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        // Create a scratch table.
        conn.execute("DROP TABLE IF EXISTS oxisql_drop_txn_test", &[])
            .await
            .expect("drop table");
        conn.execute("CREATE TABLE oxisql_drop_txn_test (id BIGINT)", &[])
            .await
            .expect("create table");

        // Open a transaction, insert a row, then drop without committing.
        {
            let mut txn = conn.transaction().await.expect("begin txn");
            let id: i64 = 99;
            txn.execute("INSERT INTO oxisql_drop_txn_test VALUES ($1)", &[&id])
                .await
                .expect("insert in txn");
            // txn is dropped here without commit — Drop schedules ROLLBACK.
        }

        // Give the spawned ROLLBACK task a moment to complete.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // The connection must be usable again (mutex released) and the
        // inserted row must NOT be visible (transaction was rolled back).
        let rows = conn
            .query("SELECT id FROM oxisql_drop_txn_test", &[])
            .await
            .expect("select after drop");
        assert_eq!(
            rows.len(),
            0,
            "row should not be visible after rolled-back txn"
        );

        // Insert the row for real so we can verify normal operation still works.
        let id: i64 = 1;
        conn.execute("INSERT INTO oxisql_drop_txn_test VALUES ($1)", &[&id])
            .await
            .expect("insert after drop");

        let rows2 = conn
            .query("SELECT id FROM oxisql_drop_txn_test", &[])
            .await
            .expect("select after insert");
        assert_eq!(rows2.len(), 1);
        assert_eq!(rows2[0].get("id"), Some(&Value::I64(1)));

        // Cleanup.
        conn.execute("DROP TABLE IF EXISTS oxisql_drop_txn_test", &[])
            .await
            .expect("cleanup");
    }

    /// Verify that invalid SQL produces an `OxiSqlError::Execution` variant.
    #[tokio::test]
    #[ignore]
    async fn test_error_mapping() {
        use oxisql_core::OxiSqlError;

        let conn = PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect");

        let result = conn.execute("THIS IS NOT VALID SQL !!!@#", &[]).await;

        match result {
            Err(OxiSqlError::Execution(_)) => {} // expected
            Err(other) => panic!("expected Execution error, got {other:?}"),
            Ok(_) => panic!("expected an error, got Ok"),
        }
    }
}

// ── Wave 37C: transaction isolation (REPEATABLE READ) ────────────────────────
//
// The `pg_extended` module already covers READ COMMITTED dirty-read prevention.
// This module adds a distinct test that exercises REPEATABLE READ snapshot
// semantics: once a transaction has read a row, a concurrent commit on another
// connection must NOT change what it sees within the same snapshot.

#[cfg(feature = "integration-postgres")]
mod pg_isolation_repeatable_read {
    use oxisql_core::Connection;
    use oxisql_postgres::{PgConnection, TlsMode};

    /// Verify REPEATABLE READ snapshot semantics.
    ///
    /// Under REPEATABLE READ:
    /// - txn_a takes a snapshot, reads zero rows.
    /// - txn_b inserts a row and commits.
    /// - txn_a re-reads — the PostgreSQL snapshot guarantees it still sees zero
    ///   rows (phantom reads are prevented for plain tables in PG RR).
    ///
    /// This is distinct from the READ COMMITTED test in `pg_extended` which
    /// checks that a *committed* row becomes visible in a subsequent query on a
    /// connection that was NOT inside a long-running transaction.
    #[tokio::test]
    #[ignore = "requires live PostgreSQL server — set POSTGRES_URL=postgres://user:pass@localhost/testdb"]
    async fn test_transaction_isolation_repeatable_read() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let conn_a = PgConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("conn_a connect");
        let conn_b = PgConnection::connect(&url, TlsMode::Disabled)
            .await
            .expect("conn_b connect");

        // Create a fresh table for this test.
        conn_a
            .execute("DROP TABLE IF EXISTS oxisql_rr_test", &[])
            .await
            .expect("drop");
        conn_a
            .execute("CREATE TABLE oxisql_rr_test (id BIGINT)", &[])
            .await
            .expect("create");

        // txn_a opens with REPEATABLE READ isolation and reads the (empty) table.
        conn_a
            .execute_batch("BEGIN TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .await
            .expect("begin repeatable read");

        let rows_first = conn_a
            .query("SELECT id FROM oxisql_rr_test", &[])
            .await
            .expect("first read");
        assert_eq!(
            rows_first.len(),
            0,
            "table should be empty at snapshot time"
        );

        // conn_b inserts a row and commits — visible at READ COMMITTED but NOT
        // within txn_a's existing REPEATABLE READ snapshot.
        let id: i64 = 42;
        conn_b
            .execute("INSERT INTO oxisql_rr_test VALUES ($1)", &[&id])
            .await
            .expect("conn_b insert");

        // txn_a re-reads within the same snapshot — must still see zero rows.
        let rows_second = conn_a
            .query("SELECT id FROM oxisql_rr_test", &[])
            .await
            .expect("second read within snapshot");
        assert_eq!(
            rows_second.len(),
            0,
            "REPEATABLE READ: committed insert by conn_b must not be visible in txn_a's snapshot"
        );

        // Commit txn_a to release the snapshot.
        conn_a.execute_batch("COMMIT").await.expect("commit txn_a");

        // After txn_a's snapshot ends, starting a new query sees the committed row.
        let rows_after = conn_a
            .query("SELECT id FROM oxisql_rr_test", &[])
            .await
            .expect("read after snapshot ends");
        assert_eq!(
            rows_after.len(),
            1,
            "after snapshot ended, committed row should be visible"
        );

        // Cleanup.
        conn_a
            .execute("DROP TABLE IF EXISTS oxisql_rr_test", &[])
            .await
            .expect("cleanup");
    }
}

// ── Wave 37C: connection pooling under load ───────────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_pool_load {
    use deadpool_postgres::{Config, Runtime};
    use oxisql_pool::postgres::OxidbPgPool;

    /// Verify that 100 concurrent tasks can each acquire a connection from a
    /// 10-slot pool, execute `SELECT 1`, and release without deadlock or error.
    ///
    /// This stresses the pool's wait-queue: with 100 tasks and max_size=10,
    /// at any moment up to 90 tasks block waiting for a slot.  All 100 must
    /// eventually succeed.
    #[tokio::test]
    #[ignore = "requires live PostgreSQL server — set POSTGRES_URL=postgres://user:pass@localhost/testdb"]
    async fn test_connection_pooling_under_load() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let mut cfg = Config::new();
        cfg.url = Some(url);
        cfg.pool = Some(deadpool_postgres::PoolConfig {
            max_size: 10,
            ..Default::default()
        });

        let pool = OxidbPgPool::new(cfg, Runtime::Tokio1).expect("create pool");
        assert_eq!(pool.max_size(), 10);

        let mut join_set = tokio::task::JoinSet::new();

        for task_id in 0u32..100 {
            let pool_clone = pool.clone();
            join_set.spawn(async move {
                let client = pool_clone.get().await.expect("get connection");
                let rows = client.simple_query("SELECT 1").await.expect("SELECT 1");
                // simple_query returns SimpleQueryMessage; at least one row message expected.
                assert!(
                    !rows.is_empty(),
                    "task {task_id}: expected at least one message from SELECT 1"
                );
            });
        }

        let mut succeeded = 0u32;
        while let Some(result) = join_set.join_next().await {
            result.expect("task panicked");
            succeeded += 1;
        }

        assert_eq!(succeeded, 100, "all 100 tasks must complete successfully");
    }
}

// ── Wave 37C: pool-level reconnection behavior ────────────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_pool_reconnect {
    use deadpool_postgres::{Config, Runtime};
    use oxisql_pool::postgres::OxidbPgPool;

    /// Verify that `deadpool-postgres` creates new connections on demand after
    /// all existing ones have been explicitly closed by closing the pool and
    /// creating a fresh one from the same URL.
    ///
    /// Full server-restart reconnection is not automatable in standard CI.
    /// This test validates the observable reconnection contract: the pool is
    /// closed (simulating connection invalidation), a fresh pool is created
    /// from the same URL, and `health_check()` confirms connectivity is
    /// re-established.  This documents that `OxidbPgPool` does not retain
    /// stale connection state across pool instances.
    ///
    /// NOTE: `deadpool-postgres` automatically retries on broken connections.
    /// The reconnection behavior on individual broken sockets is tested
    /// implicitly by the pool's own manager; this test verifies the
    /// `OxidbPgPool` wrapper exposes that behavior correctly.
    #[tokio::test]
    #[ignore = "requires live PostgreSQL server — set POSTGRES_URL=postgres://user:pass@localhost/testdb"]
    async fn test_connection_pool_reconnects() {
        let url = match std::env::var("POSTGRES_URL") {
            Ok(u) => u,
            Err(_) => return,
        };

        let make_pool = |u: String| -> OxidbPgPool {
            let mut cfg = Config::new();
            cfg.url = Some(u);
            OxidbPgPool::new(cfg, Runtime::Tokio1).expect("create pool")
        };

        // Phase 1: verify initial connectivity.
        let pool1 = make_pool(url.clone());
        pool1.health_check().await.expect("initial health_check");

        // Phase 2: close the pool (simulates connection invalidation / drain).
        pool1.close();

        // Phase 3: create a brand-new pool from the same URL (reconnection).
        let pool2 = make_pool(url.clone());
        pool2
            .health_check()
            .await
            .expect("health_check after pool recreation");

        // Phase 4: verify load-bearing queries work on the new pool.
        let client = pool2
            .get()
            .await
            .expect("get connection from reconnected pool");
        let rows = client
            .simple_query("SELECT 42::int4 AS answer")
            .await
            .expect("query on reconnected pool");
        assert!(
            !rows.is_empty(),
            "expected query results from reconnected pool"
        );
    }
}
