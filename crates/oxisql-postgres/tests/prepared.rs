//! Integration tests for `PgConnection::prepare` / [`oxisql_postgres::PgPrepared`].
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
mod pg_prepared {
    use oxisql_core::{Connection, Value};
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    // ── helpers ───────────────────────────────────────────────────────────────

    async fn connect() -> PgConnection {
        PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect")
    }

    // ── tests ─────────────────────────────────────────────────────────────────

    /// Prepare a `SELECT` with no parameters and call `query()`.
    #[tokio::test]
    #[ignore]
    async fn test_prepare_and_execute_no_params() {
        let conn = connect().await;
        let mut stmt = conn
            .prepare("SELECT 42::bigint AS n")
            .await
            .expect("prepare");

        assert_eq!(stmt.sql(), "SELECT 42::bigint AS n");

        let rows = stmt.query(&[]).await.expect("query");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("n"), Some(&Value::I64(42)));
    }

    /// Prepare the same SQL twice; both invocations must return correct results.
    ///
    /// The second call exercises the cache hit path (no extra server round-trip,
    /// though we cannot observe that externally — correctness is the gate).
    #[tokio::test]
    #[ignore]
    async fn test_prepare_cache_hit() {
        let conn = connect().await;

        let sql = "SELECT 99::bigint AS v";

        let mut first = conn.prepare(sql).await.expect("prepare first");
        let rows_first = first.query(&[]).await.expect("query first");
        assert_eq!(rows_first.len(), 1);
        assert_eq!(rows_first[0].get("v"), Some(&Value::I64(99)));

        // Second prepare of the same SQL hits the cache.
        let mut second = conn.prepare(sql).await.expect("prepare second (cached)");
        let rows_second = second.query(&[]).await.expect("query second");
        assert_eq!(rows_second.len(), 1);
        assert_eq!(
            rows_second[0].get("v"),
            Some(&Value::I64(99)),
            "cached statement must return identical results"
        );
    }

    /// Prepare a parameterized `SELECT` and execute it with multiple param sets.
    #[tokio::test]
    #[ignore]
    async fn test_prepare_parameterized() {
        let conn = connect().await;

        // Create a temp table and populate it.
        conn.execute(
            "CREATE TEMP TABLE prep_param_test (id BIGINT, val TEXT)",
            &[],
        )
        .await
        .expect("create table");

        let id1: i64 = 1;
        let val1 = "alpha";
        let id2: i64 = 2;
        let val2 = "beta";
        conn.execute(
            "INSERT INTO prep_param_test VALUES ($1, $2), ($3, $4)",
            &[&id1, &val1, &id2, &val2],
        )
        .await
        .expect("insert rows");

        // Prepare a parameterized SELECT.
        let mut stmt = conn
            .prepare("SELECT val FROM prep_param_test WHERE id = $1")
            .await
            .expect("prepare");

        // Execute with first param set.
        let rows = stmt.query(&[&id1]).await.expect("query id=1");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("val"), Some(&Value::Text("alpha".to_string())));

        // Execute with second param set — same statement, different params.
        let rows = stmt.query(&[&id2]).await.expect("query id=2");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get("val"), Some(&Value::Text("beta".to_string())));
    }

    /// Prepare an `INSERT` and call `execute()` twice with different values.
    #[tokio::test]
    #[ignore]
    async fn test_prepare_insert() {
        let conn = connect().await;

        conn.execute(
            "CREATE TEMP TABLE prep_insert_test (id BIGINT, name TEXT)",
            &[],
        )
        .await
        .expect("create table");

        let mut stmt = conn
            .prepare("INSERT INTO prep_insert_test (id, name) VALUES ($1, $2)")
            .await
            .expect("prepare insert");

        // First execution.
        let id_a: i64 = 10;
        let name_a = "Alice";
        let affected = stmt.execute(&[&id_a, &name_a]).await.expect("execute A");
        assert_eq!(affected, 1, "INSERT should affect 1 row");

        // Second execution with different values.
        let id_b: i64 = 20;
        let name_b = "Bob";
        let affected = stmt.execute(&[&id_b, &name_b]).await.expect("execute B");
        assert_eq!(affected, 1, "INSERT should affect 1 row");

        // Verify both rows exist.
        let rows = conn
            .query("SELECT id, name FROM prep_insert_test ORDER BY id", &[])
            .await
            .expect("select");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get("id"), Some(&Value::I64(10)));
        assert_eq!(rows[0].get("name"), Some(&Value::Text("Alice".to_string())));
        assert_eq!(rows[1].get("id"), Some(&Value::I64(20)));
        assert_eq!(rows[1].get("name"), Some(&Value::Text("Bob".to_string())));
    }
}
