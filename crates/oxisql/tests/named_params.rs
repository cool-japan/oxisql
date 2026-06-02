//! Integration tests for named-parameter support (`execute_named` / `query_named`).
//!
//! These tests require the `embedded` feature so they can use `memory://`
//! without any external database server.

#[cfg(feature = "embedded")]
mod embedded_named_params {
    use oxisql::{Connection, OxiSqlError};

    async fn make_conn() -> Box<dyn Connection> {
        oxisql::connect("memory://")
            .await
            .expect("memory:// must succeed with the embedded feature")
    }

    async fn setup_table(conn: &dyn Connection) {
        conn.execute("CREATE TABLE named_test (id INTEGER, name TEXT)", &[])
            .await
            .expect("CREATE TABLE");

        conn.execute(
            "INSERT INTO named_test VALUES ($1, $2)",
            &[
                &1i64 as &dyn oxisql::ToSqlValue,
                &"Alice" as &dyn oxisql::ToSqlValue,
            ],
        )
        .await
        .expect("INSERT Alice");

        conn.execute(
            "INSERT INTO named_test VALUES ($1, $2)",
            &[
                &2i64 as &dyn oxisql::ToSqlValue,
                &"Bob" as &dyn oxisql::ToSqlValue,
            ],
        )
        .await
        .expect("INSERT Bob");
    }

    #[tokio::test]
    async fn test_query_named_basic() {
        let conn = make_conn().await;
        setup_table(conn.as_ref()).await;

        let rows = conn
            .query_named(
                "SELECT * FROM named_test WHERE id = :id",
                &[("id", &1i64 as &dyn oxisql::ToSqlValue)],
            )
            .await
            .expect("query_named should succeed");

        assert_eq!(rows.len(), 1, "should get exactly one row");
        let id: i64 = rows[0].try_get("id").expect("id column");
        let name: String = rows[0].try_get("name").expect("name column");
        assert_eq!(id, 1);
        assert_eq!(name, "Alice");
    }

    #[tokio::test]
    async fn test_execute_named() {
        let conn = make_conn().await;
        conn.execute(
            "CREATE TABLE execute_named_test (id INTEGER, name TEXT)",
            &[],
        )
        .await
        .expect("CREATE TABLE");

        let affected = conn
            .execute_named(
                "INSERT INTO execute_named_test VALUES (:id, :name)",
                &[
                    ("id", &99i64 as &dyn oxisql::ToSqlValue),
                    ("name", &"Charlie" as &dyn oxisql::ToSqlValue),
                ],
            )
            .await
            .expect("execute_named should succeed");

        // Most embedded backends report 1 affected row for a single INSERT.
        // Some report 0 — either is acceptable; the important thing is no error.
        let _ = affected;

        let rows = conn
            .query(
                "SELECT * FROM execute_named_test WHERE id = $1",
                &[&99i64 as &dyn oxisql::ToSqlValue],
            )
            .await
            .expect("verify SELECT");
        assert_eq!(rows.len(), 1);
        let name: String = rows[0].try_get("name").expect("name column");
        assert_eq!(name, "Charlie");
    }

    #[tokio::test]
    async fn test_named_repeated_param() {
        let conn = make_conn().await;
        setup_table(conn.as_ref()).await;

        // :id appears twice — should produce $1 AND $1 with a single binding.
        let rows = conn
            .query_named(
                "SELECT * FROM named_test WHERE id = :id OR id = :id",
                &[("id", &1i64 as &dyn oxisql::ToSqlValue)],
            )
            .await
            .expect("query_named with repeated param should succeed");

        // At least one result row for id = 1
        assert!(
            !rows.is_empty(),
            "should return at least one row for id = 1"
        );
        for row in &rows {
            let id: i64 = row.try_get("id").expect("id column");
            assert_eq!(id, 1, "all returned rows should have id = 1");
        }
    }

    #[tokio::test]
    async fn test_named_missing_param_error() {
        let conn = make_conn().await;
        setup_table(conn.as_ref()).await;

        let result = conn
            .query_named("SELECT * FROM named_test WHERE id = :missing", &[])
            .await;

        assert!(
            result.is_err(),
            "query_named with missing param should return Err"
        );

        let err = result.expect_err("expected error");
        match err {
            OxiSqlError::Params(msg) => {
                assert!(
                    msg.contains("missing"),
                    "error message should mention missing: {msg}"
                );
            }
            other => panic!("expected OxiSqlError::Params, got {other:?}"),
        }
    }
}
