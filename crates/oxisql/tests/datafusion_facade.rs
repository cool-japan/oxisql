//! Integration tests for the `oxisql::datafusion` convenience module.
//!
//! These tests verify the `context()` and `register_table()` facade helpers
//! that wrap `oxisql-datafusion` behind the `datafusion` feature flag.

#[cfg(feature = "datafusion")]
mod tests {
    use arrow::datatypes::{DataType, Field, Schema};
    use oxisql::datafusion::{context, register_table};
    use oxisql::Connection;
    use oxisql_embedded::EmbeddedConnection;
    use std::sync::Arc;

    /// `context()` creates a new `OxiSqlContext` that can execute a trivial
    /// query without any registered tables.
    #[tokio::test]
    async fn test_facade_context_trivial_query() {
        let ctx = context();
        let results = ctx
            .execute_sql("SELECT 1 + 1 AS result")
            .await
            .expect("execute_sql");
        assert!(!results.is_empty(), "expected at least one batch");
        assert_eq!(results[0].num_rows(), 1, "SELECT literal must return 1 row");
    }

    /// `register_table()` wires an `EmbeddedConnection` into a DataFusion
    /// context and the table is queryable via `execute_sql`.
    #[tokio::test]
    async fn test_facade_datafusion_register_table() {
        let conn = EmbeddedConnection::open_memory().expect("open_memory");
        conn.execute("CREATE TABLE t (n INTEGER)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO t VALUES (1)", &[])
            .await
            .expect("INSERT 1");
        conn.execute("INSERT INTO t VALUES (2)", &[])
            .await
            .expect("INSERT 2");

        let ctx = context();
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, true)]));
        let conn_arc = Arc::new(conn) as Arc<dyn oxisql::Connection>;
        register_table(&ctx, "t", conn_arc, schema).expect("register_table");

        let results = ctx
            .execute_sql("SELECT n FROM t ORDER BY n")
            .await
            .expect("execute_sql");
        assert!(!results.is_empty(), "expected at least one batch");
        let total_rows: usize = results.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 2, "expected 2 rows from table t");
    }

    /// Registering the same table name twice via the facade returns an error
    /// (DataFusion rejects duplicate table names).
    #[tokio::test]
    async fn test_facade_register_duplicate_returns_error() {
        let conn1 = EmbeddedConnection::open_memory().expect("open_memory");
        conn1
            .execute("CREATE TABLE dup (v INTEGER)", &[])
            .await
            .expect("CREATE TABLE");

        let conn2 = EmbeddedConnection::open_memory().expect("open_memory");
        conn2
            .execute("CREATE TABLE dup (v INTEGER)", &[])
            .await
            .expect("CREATE TABLE");

        let ctx = context();
        let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, true)]));

        let conn1_arc = Arc::new(conn1) as Arc<dyn oxisql::Connection>;
        let conn2_arc = Arc::new(conn2) as Arc<dyn oxisql::Connection>;

        register_table(&ctx, "dup", conn1_arc, Arc::clone(&schema))
            .expect("first registration should succeed");

        let second = register_table(&ctx, "dup", conn2_arc, schema);
        assert!(
            second.is_err(),
            "duplicate table name should return an error"
        );
    }
}
