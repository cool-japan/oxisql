//! Integration tests for `oxisql-sqlite-compat`.
//!
//! All tests use in-memory databases (`:memory:`) or temporary files so that
//! no external services are required.

use oxisql_core::{Connection, TableType, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── basic CRUD ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_memory_open_and_ping() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");
    conn.ping().await.expect("ping failed");
}

#[tokio::test]
async fn test_create_insert_select() {
    let conn = SqliteConnection::open_memory().await.unwrap();

    conn.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .await
    .unwrap();

    let affected = conn
        .execute("INSERT INTO users VALUES ($1, $2)", &[&1i64, &"Alice"])
        .await
        .unwrap();
    // Limbo returns changes() which should be 1 for a single INSERT.
    assert_eq!(affected, 1, "expected 1 affected row");

    let rows = conn.query("SELECT id, name FROM users", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(1)));
    assert_eq!(rows[0].get_by_index(1), Some(&Value::Text("Alice".into())));
}

#[tokio::test]
async fn test_multiple_rows() {
    let conn = SqliteConnection::open_memory().await.unwrap();

    conn.execute("CREATE TABLE t (n INTEGER)", &[])
        .await
        .unwrap();
    for i in 0i64..5 {
        conn.execute("INSERT INTO t VALUES ($1)", &[&i])
            .await
            .unwrap();
    }

    let rows = conn.query("SELECT n FROM t ORDER BY n", &[]).await.unwrap();
    assert_eq!(rows.len(), 5);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.get_by_index(0), Some(&Value::I64(i as i64)));
    }
}

#[tokio::test]
async fn test_null_values() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (a INTEGER, b TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (NULL, NULL)", &[])
        .await
        .unwrap();

    let rows = conn.query("SELECT a, b FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Null));
    assert_eq!(rows[0].get_by_index(1), Some(&Value::Null));
}

#[tokio::test]
async fn test_float_values() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (x REAL)", &[]).await.unwrap();
    conn.execute("INSERT INTO t VALUES ($1)", &[&1.5f64])
        .await
        .unwrap();

    let rows = conn.query("SELECT x FROM t", &[]).await.unwrap();
    if let Some(Value::F64(f)) = rows[0].get_by_index(0) {
        assert!((f - 1.5).abs() < 1e-9, "unexpected float: {f}");
    } else {
        panic!("expected F64, got {:?}", rows[0].get_by_index(0));
    }
}

#[tokio::test]
async fn test_blob_values() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (b BLOB)", &[]).await.unwrap();
    let data: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEF];
    conn.execute("INSERT INTO t VALUES ($1)", &[&data])
        .await
        .unwrap();

    let rows = conn.query("SELECT b FROM t", &[]).await.unwrap();
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]))
    );
}

// ── parameter binding ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_positional_params_ordering() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (a TEXT, b TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES ($1, $2)", &[&"first", &"second"])
        .await
        .unwrap();

    let rows = conn.query("SELECT a, b FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text("first".into())));
    assert_eq!(rows[0].get_by_index(1), Some(&Value::Text("second".into())));
}

#[tokio::test]
async fn test_param_in_where_clause() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'Alice')", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (2, 'Bob')", &[])
        .await
        .unwrap();

    let rows = conn
        .query("SELECT name FROM t WHERE id = $1", &[&2i64])
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text("Bob".into())));
}

#[tokio::test]
async fn test_param_inside_string_literal_not_substituted() {
    // The `$1` inside the string literal must NOT be rewritten to `?`.
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (msg TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES ('literal $1 text')", &[])
        .await
        .unwrap();

    let rows = conn.query("SELECT msg FROM t", &[]).await.unwrap();
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::Text("literal $1 text".into()))
    );
}

// ── transactions ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_transaction_commit() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .unwrap();

    let mut txn = conn.transaction().await.unwrap();
    txn.execute("INSERT INTO t VALUES (42)", &[]).await.unwrap();
    txn.commit().await.unwrap();

    let rows = conn.query("SELECT id FROM t", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(42)));
}

// NOTE: Limbo 0.0.22 does not support ROLLBACK ("ROLLBACK not supported yet").
// This test is ignored until limbo implements transaction rollback.
#[tokio::test]
#[ignore = "limbo 0.0.22 does not implement ROLLBACK — enable when limbo adds support"]
async fn test_transaction_rollback() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .unwrap();

    let mut txn = conn.transaction().await.unwrap();
    txn.execute("INSERT INTO t VALUES (99)", &[]).await.unwrap();
    txn.rollback().await.unwrap();

    let rows = conn.query("SELECT id FROM t", &[]).await.unwrap();
    assert!(rows.is_empty(), "rollback should have removed the row");
}

// NOTE: Limbo 0.0.22 does not support ROLLBACK ("ROLLBACK not supported yet").
// This test is ignored until limbo implements transaction rollback.
#[tokio::test]
#[ignore = "limbo 0.0.22 does not implement ROLLBACK — enable when limbo adds support"]
async fn test_transaction_query() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER, val TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'existing')", &[])
        .await
        .unwrap();

    let mut txn = conn.transaction().await.unwrap();
    txn.execute("INSERT INTO t VALUES (2, 'new')", &[])
        .await
        .unwrap();
    let rows = txn.query("SELECT COUNT(*) FROM t", &[]).await.unwrap();
    // Within the transaction we should see both rows.
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(2)));
    txn.rollback().await.unwrap();

    // After rollback only the original row should exist.
    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(1)));
}

// ── execute_batch ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_execute_batch_semicolon_in_literal() {
    // The `;` inside the string value must not split the INSERT from the CREATE.
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");
    conn.execute_batch("CREATE TABLE t (id INTEGER, val TEXT); INSERT INTO t VALUES (1, 'a;b')")
        .await
        .expect("execute_batch failed");

    let rows = conn
        .query("SELECT val FROM t WHERE id = 1", &[])
        .await
        .expect("query failed");
    assert_eq!(rows.len(), 1, "expected 1 row");
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::Text("a;b".to_string())),
        "semicolon inside literal must be preserved"
    );
}

#[tokio::test]
async fn test_execute_batch_ddl() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute_batch(
        "CREATE TABLE a (x INTEGER);
         CREATE TABLE b (y TEXT);",
    )
    .await
    .unwrap();

    let tables = conn.tables().await.unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"a"), "expected table 'a', got {names:?}");
    assert!(names.contains(&"b"), "expected table 'b', got {names:?}");
}

// ── schema introspection ──────────────────────────────────────────────────────

#[tokio::test]
async fn test_tables_listing() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE foo (id INTEGER)", &[])
        .await
        .unwrap();
    conn.execute("CREATE TABLE bar (val TEXT)", &[])
        .await
        .unwrap();

    let tables = conn.tables().await.unwrap();
    let names: Vec<&str> = tables.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"bar"));
    for t in &tables {
        assert_eq!(t.table_type, TableType::Base);
        assert!(t.schema.is_none());
    }
}

#[tokio::test]
async fn test_columns_introspection() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute(
        "CREATE TABLE items (id INTEGER NOT NULL, label TEXT, price REAL)",
        &[],
    )
    .await
    .unwrap();

    let cols = conn.columns("items").await.unwrap();
    assert_eq!(cols.len(), 3);
    assert_eq!(cols[0].name, "id");
    assert!(!cols[0].nullable); // NOT NULL
    assert_eq!(cols[1].name, "label");
    assert!(cols[1].nullable); // nullable
}

#[tokio::test]
async fn test_indexes_introspection() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .await
        .unwrap();
    conn.execute("CREATE INDEX idx_name ON t (name)", &[])
        .await
        .unwrap();

    let indexes = conn.indexes("t").await.unwrap();
    assert_eq!(indexes.len(), 1);
    assert_eq!(indexes[0].name, "idx_name");
    assert!(!indexes[0].unique);
    assert!(indexes[0].columns.contains(&"name".to_string()));
}

#[tokio::test]
async fn test_unique_index() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (email TEXT)", &[])
        .await
        .unwrap();
    conn.execute("CREATE UNIQUE INDEX idx_email ON t (email)", &[])
        .await
        .unwrap();

    let indexes = conn.indexes("t").await.unwrap();
    assert_eq!(indexes.len(), 1);
    assert!(indexes[0].unique);
}

// ── file-backed persistence ───────────────────────────────────────────────────

#[tokio::test]
async fn test_file_persistence() {
    let tmp = std::env::temp_dir().join(format!(
        "oxisql_compat_test_{}.sqlite3",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    {
        let conn = SqliteConnection::open(tmp.to_str().unwrap()).await.unwrap();
        conn.execute("CREATE TABLE t (n INTEGER)", &[])
            .await
            .unwrap();
        conn.execute("INSERT INTO t VALUES (77)", &[])
            .await
            .unwrap();
    }

    // Re-open and verify persistence.
    let conn2 = SqliteConnection::open(tmp.to_str().unwrap()).await.unwrap();
    let rows = conn2.query("SELECT n FROM t", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(77)));

    // Clean up.
    let _ = std::fs::remove_file(&tmp);
    // Limbo may create a WAL file alongside the DB.
    let wal = tmp.with_extension("sqlite3-wal");
    let _ = std::fs::remove_file(&wal);
}

// ── foreign keys ─────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "limbo 0.0.22 does not preserve FK DDL in sqlite_master"]
async fn test_foreign_keys_basic() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute(
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .await
    .unwrap();
    conn.execute(
        "CREATE TABLE orders (\
            id INTEGER PRIMARY KEY,\
            customer_id INTEGER NOT NULL REFERENCES customers(id)\
        )",
        &[],
    )
    .await
    .unwrap();

    let fks = conn.foreign_keys("orders").await.unwrap();
    assert_eq!(fks.len(), 1, "expected 1 FK on orders table, got {fks:?}");
    assert_eq!(fks[0].column, "customer_id");
    assert_eq!(fks[0].foreign_table, "customers");
    assert_eq!(fks[0].foreign_column, "id");
}

// ── prepared statements ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_prepared_execute() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (n INTEGER)", &[])
        .await
        .unwrap();

    let mut ps = conn.prepare("INSERT INTO t VALUES ($1)").await.unwrap();
    for i in 0i64..3 {
        ps.execute(&[&i]).await.unwrap();
    }

    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await.unwrap();
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(3)));
}

#[tokio::test]
async fn test_prepared_query() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (1, 'Alice')", &[])
        .await
        .unwrap();
    conn.execute("INSERT INTO t VALUES (2, 'Bob')", &[])
        .await
        .unwrap();

    let mut ps = conn
        .prepare("SELECT name FROM t WHERE id = $1")
        .await
        .unwrap();
    let rows = ps.query(&[&2i64]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Text("Bob".into())));
}
