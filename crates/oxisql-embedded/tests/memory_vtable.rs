use oxisql_core::{Connection, Value};
use oxisql_embedded::EmbeddedConnection;

// ── Virtual table tests ──────────────────────────────────────────────────────

/// Register a virtual table and query it with SELECT *.
#[tokio::test]
async fn test_virtual_table_registration() {
    use oxisql_core::Row;
    use std::sync::Arc;

    let mut conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.register_virtual_table(
        "csv_data",
        Arc::new(|| {
            vec![Row::new(
                vec!["id".into(), "name".into()],
                vec![Value::I64(1), Value::Text("Alice".into())],
            )]
        }),
    );

    let rows = conn
        .query("SELECT * FROM csv_data", &[])
        .await
        .expect("SELECT vtable");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::I64(1)));
    assert_eq!(rows[0].get("name"), Some(&Value::Text("Alice".into())));
}

/// Register two rows, query with WHERE name = 'Bob'.
#[tokio::test]
async fn test_virtual_table_where_filter_string() {
    use oxisql_core::Row;
    use std::sync::Arc;

    let mut conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.register_virtual_table(
        "people",
        Arc::new(|| {
            vec![
                Row::new(
                    vec!["id".into(), "name".into()],
                    vec![Value::I64(1), Value::Text("Alice".into())],
                ),
                Row::new(
                    vec!["id".into(), "name".into()],
                    vec![Value::I64(2), Value::Text("Bob".into())],
                ),
            ]
        }),
    );

    let rows = conn
        .query("SELECT * FROM people WHERE name = 'Bob'", &[])
        .await
        .expect("SELECT vtable with filter");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::I64(2)));
}

/// Register a virtual table and query with WHERE id = 1 (integer equality).
#[tokio::test]
async fn test_virtual_table_where_filter_integer() {
    use oxisql_core::Row;
    use std::sync::Arc;

    let mut conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.register_virtual_table(
        "items",
        Arc::new(|| {
            vec![
                Row::new(vec!["id".into()], vec![Value::I64(10)]),
                Row::new(vec!["id".into()], vec![Value::I64(20)]),
                Row::new(vec!["id".into()], vec![Value::I64(30)]),
            ]
        }),
    );

    let rows = conn
        .query("SELECT * FROM items WHERE id = 20", &[])
        .await
        .expect("SELECT vtable int filter");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("id"), Some(&Value::I64(20)));
}

/// Unregister a virtual table and verify it is no longer intercepted.
#[tokio::test]
async fn test_virtual_table_unregister() {
    use oxisql_core::Row;
    use std::sync::Arc;

    let mut conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.register_virtual_table(
        "temp_view",
        Arc::new(|| vec![Row::new(vec!["x".into()], vec![Value::I64(99)])]),
    );

    // Confirm it works before unregister.
    let rows = conn
        .query("SELECT * FROM temp_view", &[])
        .await
        .expect("before unregister");
    assert_eq!(rows.len(), 1);

    conn.unregister_virtual_table("temp_view");

    // After unregister, GlueSQL sees the query and fails (table not found).
    let result = conn.query("SELECT * FROM temp_view", &[]).await;
    assert!(result.is_err(), "unregistered vtable should error");

    // virtual_table_names should now be empty.
    assert!(conn.virtual_table_names().is_empty());
}

/// virtual_table_names returns sorted names.
#[test]
fn test_virtual_table_names_sorted() {
    use oxisql_core::Row;
    use std::sync::Arc;

    let mut conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.register_virtual_table("zzz", Arc::new(|| vec![Row::new(vec![], vec![])]));
    conn.register_virtual_table("aaa", Arc::new(|| vec![Row::new(vec![], vec![])]));
    conn.register_virtual_table("mmm", Arc::new(|| vec![Row::new(vec![], vec![])]));

    assert_eq!(conn.virtual_table_names(), vec!["aaa", "mmm", "zzz"]);
}

// ── B-tree index tests ───────────────────────────────────────────────────────

/// Create an index via SQL, insert rows, verify the index is populated.
#[tokio::test]
async fn test_btree_index_create_and_lookup() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute(
        "CREATE TABLE people (id INTEGER, name TEXT, age INTEGER)",
        &[],
    )
    .await
    .expect("CREATE TABLE");

    // Create the B-tree index via the intercepted CREATE INDEX syntax.
    conn.execute("CREATE INDEX idx_age ON people(age)", &[])
        .await
        .expect("CREATE INDEX");
    assert!(
        conn.has_btree_index("people", "age")
            .expect("has_btree_index"),
        "index should exist after CREATE INDEX"
    );

    // Insert rows — indexes are updated after each successful INSERT.
    conn.execute(
        "INSERT INTO people (id, name, age) VALUES (1, 'Alice', 30)",
        &[],
    )
    .await
    .expect("INSERT Alice");
    conn.execute(
        "INSERT INTO people (id, name, age) VALUES (2, 'Bob', 25)",
        &[],
    )
    .await
    .expect("INSERT Bob");
    conn.execute(
        "INSERT INTO people (id, name, age) VALUES (3, 'Carol', 30)",
        &[],
    )
    .await
    .expect("INSERT Carol");

    // Lookup through the index via the public API.
    use oxisql_embedded::IndexKey;
    let ids = conn
        .lookup_btree_index("people", "age", &IndexKey::Integer(30))
        .expect("lookup_btree_index")
        .expect("index should exist");
    assert_eq!(ids.len(), 2, "two people aged 30");
}

/// Drop an index and verify has_btree_index returns false.
#[tokio::test]
async fn test_btree_index_drop() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE orders (id INTEGER, status TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("CREATE INDEX idx_status ON orders(status)", &[])
        .await
        .expect("CREATE INDEX");
    assert!(conn.has_btree_index("orders", "status").expect("has idx"));

    conn.execute("DROP INDEX idx_status ON orders(status)", &[])
        .await
        .expect("DROP INDEX");
    assert!(
        !conn
            .has_btree_index("orders", "status")
            .expect("has idx after drop"),
        "index should not exist after DROP INDEX"
    );
}

/// create_btree_index / drop_btree_index via Rust API.
#[test]
fn test_btree_index_rust_api() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.create_btree_index("events", "timestamp")
        .expect("create_btree_index");
    assert!(conn.has_btree_index("events", "timestamp").expect("has"));
    conn.drop_btree_index("events", "timestamp")
        .expect("drop_btree_index");
    assert!(!conn
        .has_btree_index("events", "timestamp")
        .expect("has after drop"));
}
