// ── FjallEmbeddedConnection tests ─────────────────────────────────────────────

/// Generate a unique temp directory path for a test, using process ID and
/// sub-second nanosecond timestamp to avoid clashes between concurrent test runs.
fn unique_test_dir(prefix: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "{}_{}_{}",
        prefix,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos(),
    ))
}

#[cfg(feature = "fjall-storage")]
mod fjall_storage_tests {
    use oxisql_core::Connection;
    use oxisql_embedded::FjallEmbeddedConnection;

    #[tokio::test]
    async fn test_fjall_create_and_query() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_basic");
        let _ = std::fs::remove_dir_all(&dir);

        let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db");
        conn.execute("CREATE TABLE items (id INTEGER, name TEXT)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO items VALUES (1, 'Widget')", &[])
            .await
            .expect("INSERT row 1");

        let rows = conn
            .query("SELECT * FROM items", &[])
            .await
            .expect("SELECT");
        assert_eq!(rows.len(), 1, "expected 1 row, got {}", rows.len());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_fjall_persistence() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_persist");
        let _ = std::fs::remove_dir_all(&dir);

        // Write data in the first connection.
        {
            let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db (write)");
            conn.execute("CREATE TABLE log (ts INTEGER, msg TEXT)", &[])
                .await
                .expect("CREATE TABLE log");
            conn.execute("INSERT INTO log VALUES (1000, 'boot')", &[])
                .await
                .expect("INSERT log row");
        }

        // Reopen and verify the data survived.
        let conn2 = FjallEmbeddedConnection::open(&dir).expect("reopen fjall db (read)");
        let rows = conn2
            .query("SELECT * FROM log", &[])
            .await
            .expect("SELECT after reopen");
        assert_eq!(
            rows.len(),
            1,
            "expected 1 row after reopen, got {}",
            rows.len()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_fjall_multiple_tables() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_multi_table");
        let _ = std::fs::remove_dir_all(&dir);

        let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db");
        conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
            .await
            .expect("CREATE TABLE users");
        conn.execute("CREATE TABLE orders (id INTEGER, user_id INTEGER)", &[])
            .await
            .expect("CREATE TABLE orders");

        conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[])
            .await
            .expect("INSERT user 1");
        conn.execute("INSERT INTO users VALUES (2, 'Bob')", &[])
            .await
            .expect("INSERT user 2");
        conn.execute("INSERT INTO orders VALUES (100, 1)", &[])
            .await
            .expect("INSERT order 100");
        conn.execute("INSERT INTO orders VALUES (101, 2)", &[])
            .await
            .expect("INSERT order 101");

        let user_rows = conn
            .query("SELECT * FROM users", &[])
            .await
            .expect("SELECT users");
        assert_eq!(user_rows.len(), 2);

        let order_rows = conn
            .query("SELECT * FROM orders", &[])
            .await
            .expect("SELECT orders");
        assert_eq!(order_rows.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_fjall_delete() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_delete");
        let _ = std::fs::remove_dir_all(&dir);

        let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db");
        conn.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO t VALUES (1, 'a')", &[])
            .await
            .expect("INSERT 1");
        conn.execute("INSERT INTO t VALUES (2, 'b')", &[])
            .await
            .expect("INSERT 2");

        conn.execute("DELETE FROM t WHERE id = 1", &[])
            .await
            .expect("DELETE id=1");

        let rows = conn.query("SELECT * FROM t", &[]).await.expect("SELECT");
        assert_eq!(rows.len(), 1, "expected 1 row after delete");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_fjall_drop_table() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_drop");
        let _ = std::fs::remove_dir_all(&dir);

        let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db");
        conn.execute("CREATE TABLE ephemeral (id INTEGER)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO ephemeral VALUES (42)", &[])
            .await
            .expect("INSERT");
        conn.execute("DROP TABLE ephemeral", &[])
            .await
            .expect("DROP TABLE");

        let result = conn.query("SELECT * FROM ephemeral", &[]).await;
        assert!(
            result.is_err(),
            "expected error when querying dropped table"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn test_fjall_param_binding() {
        let dir = crate::unique_test_dir("oxisql_test_fjall_params");
        let _ = std::fs::remove_dir_all(&dir);

        let conn = FjallEmbeddedConnection::open(&dir).expect("open fjall db");
        conn.execute(
            "CREATE TABLE products (id INTEGER, name TEXT, price FLOAT)",
            &[],
        )
        .await
        .expect("CREATE TABLE");

        conn.execute(
            "INSERT INTO products VALUES ($1, $2, $3)",
            &[
                &1i64 as &dyn oxisql_core::ToSqlValue,
                &"Gadget" as &dyn oxisql_core::ToSqlValue,
                &9.99f64 as &dyn oxisql_core::ToSqlValue,
            ],
        )
        .await
        .expect("INSERT with params");

        let rows = conn
            .query(
                "SELECT * FROM products WHERE id = $1",
                &[&1i64 as &dyn oxisql_core::ToSqlValue],
            )
            .await
            .expect("SELECT with param");
        assert_eq!(rows.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ── RedbEmbeddedConnection tests ──────────────────────────────────────────────

#[cfg(feature = "redb-storage")]
mod redb_storage_tests {
    use oxisql_core::{Connection, ToSqlValue};
    use oxisql_embedded::RedbEmbeddedConnection;

    #[tokio::test]
    async fn test_redb_create_table_and_insert() {
        let path = crate::unique_test_dir("oxisql_test_redb_basic").with_extension("redb");
        let _ = std::fs::remove_file(&path);

        let conn = RedbEmbeddedConnection::open(&path).expect("open redb db");
        conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
            .await
            .expect("CREATE TABLE users");
        conn.execute(
            "INSERT INTO users VALUES ($1, $2)",
            &[&1i64 as &dyn ToSqlValue, &"Alice" as &dyn ToSqlValue],
        )
        .await
        .expect("INSERT Alice");

        let rows = conn
            .query("SELECT * FROM users", &[])
            .await
            .expect("SELECT");
        assert_eq!(rows.len(), 1, "expected 1 row");

        let id: i64 = rows[0].try_get("id").expect("id column");
        let name: String = rows[0].try_get("name").expect("name column");
        assert_eq!(id, 1);
        assert_eq!(name, "Alice");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_redb_persistence_across_connections() {
        let path = crate::unique_test_dir("oxisql_test_redb_persist").with_extension("redb");
        let _ = std::fs::remove_file(&path);

        {
            let conn = RedbEmbeddedConnection::open(&path).expect("open redb db (write)");
            conn.execute("CREATE TABLE things (id INTEGER, val TEXT)", &[])
                .await
                .expect("CREATE TABLE things");
            conn.execute(
                "INSERT INTO things VALUES ($1, $2)",
                &[&42i64 as &dyn ToSqlValue, &"hello" as &dyn ToSqlValue],
            )
            .await
            .expect("INSERT things row");
        }

        // Reopen and verify data persists.
        let conn2 = RedbEmbeddedConnection::open(&path).expect("reopen redb db (read)");
        let rows = conn2
            .query("SELECT * FROM things", &[])
            .await
            .expect("SELECT after reopen");
        assert_eq!(rows.len(), 1, "expected 1 row after reopen");
        let val: String = rows[0].try_get("val").expect("val column");
        assert_eq!(val, "hello");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_redb_in_memory_smoke() {
        let conn = RedbEmbeddedConnection::open_in_memory().expect("open in-memory redb");
        conn.execute("CREATE TABLE t (n INTEGER)", &[])
            .await
            .expect("CREATE TABLE t");
        conn.execute("INSERT INTO t VALUES (99)", &[])
            .await
            .expect("INSERT");
        let rows = conn.query("SELECT * FROM t", &[]).await.expect("SELECT");
        assert_eq!(rows.len(), 1);
        let n: i64 = rows[0].try_get("n").expect("n column");
        assert_eq!(n, 99);
    }

    #[tokio::test]
    async fn test_redb_multiple_tables() {
        let path = crate::unique_test_dir("oxisql_test_redb_multi_table").with_extension("redb");
        let _ = std::fs::remove_file(&path);

        let conn = RedbEmbeddedConnection::open(&path).expect("open redb db");
        conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[])
            .await
            .expect("CREATE TABLE users");
        conn.execute("CREATE TABLE orders (id INTEGER, user_id INTEGER)", &[])
            .await
            .expect("CREATE TABLE orders");

        conn.execute("INSERT INTO users VALUES (1, 'Bob')", &[])
            .await
            .expect("INSERT user");
        conn.execute("INSERT INTO orders VALUES (10, 1)", &[])
            .await
            .expect("INSERT order");

        let u_rows = conn
            .query("SELECT * FROM users", &[])
            .await
            .expect("SELECT users");
        let o_rows = conn
            .query("SELECT * FROM orders", &[])
            .await
            .expect("SELECT orders");
        assert_eq!(u_rows.len(), 1);
        assert_eq!(o_rows.len(), 1);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_redb_drop_table() {
        let path = crate::unique_test_dir("oxisql_test_redb_drop").with_extension("redb");
        let _ = std::fs::remove_file(&path);

        let conn = RedbEmbeddedConnection::open(&path).expect("open redb db");
        conn.execute("CREATE TABLE ephemeral (id INTEGER)", &[])
            .await
            .expect("CREATE TABLE");
        conn.execute("INSERT INTO ephemeral VALUES (42)", &[])
            .await
            .expect("INSERT");
        conn.execute("DROP TABLE ephemeral", &[])
            .await
            .expect("DROP TABLE");

        let result = conn.query("SELECT * FROM ephemeral", &[]).await;
        assert!(
            result.is_err(),
            "expected error when querying dropped table"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn test_redb_param_binding() {
        let path = crate::unique_test_dir("oxisql_test_redb_params").with_extension("redb");
        let _ = std::fs::remove_file(&path);

        let conn = RedbEmbeddedConnection::open(&path).expect("open redb db");
        conn.execute(
            "CREATE TABLE products (id INTEGER, name TEXT, price FLOAT)",
            &[],
        )
        .await
        .expect("CREATE TABLE products");

        conn.execute(
            "INSERT INTO products VALUES ($1, $2, $3)",
            &[
                &1i64 as &dyn oxisql_core::ToSqlValue,
                &"Widget" as &dyn oxisql_core::ToSqlValue,
                &5.99f64 as &dyn oxisql_core::ToSqlValue,
            ],
        )
        .await
        .expect("INSERT with params");

        let rows = conn
            .query(
                "SELECT * FROM products WHERE id = $1",
                &[&1i64 as &dyn oxisql_core::ToSqlValue],
            )
            .await
            .expect("SELECT with param");
        assert_eq!(rows.len(), 1);

        let _ = std::fs::remove_file(&path);
    }
}
