use oxisql_core::{Connection, ToSqlValue};
use oxisql_embedded::EmbeddedConnection;

// ── Prepared statement tests ───────────────────────────────────────────────────

#[tokio::test]
async fn test_prepare_basic_query() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[])
        .await
        .expect("CREATE TABLE");
    conn.execute("INSERT INTO t VALUES (1, 'Alice')", &[])
        .await
        .expect("INSERT alice");
    conn.execute("INSERT INTO t VALUES (2, 'Bob')", &[])
        .await
        .expect("INSERT bob");

    let mut stmt = conn
        .prepare("SELECT name FROM t WHERE id = $1")
        .await
        .expect("prepare should succeed");

    let id1: i64 = 1;
    let rows = stmt
        .query(&[&id1 as &dyn ToSqlValue])
        .await
        .expect("query with id=1");
    assert_eq!(rows.len(), 1);

    let id2: i64 = 2;
    let rows2 = stmt
        .query(&[&id2 as &dyn ToSqlValue])
        .await
        .expect("query with id=2");
    assert_eq!(rows2.len(), 1);
}

#[tokio::test]
async fn test_prepare_execute_insert() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE nums (v INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    let mut stmt = conn
        .prepare("INSERT INTO nums VALUES ($1)")
        .await
        .expect("prepare should succeed");

    let v1: i64 = 10;
    let n1 = stmt
        .execute(&[&v1 as &dyn ToSqlValue])
        .await
        .expect("execute insert 10");
    assert_eq!(n1, 1);

    let v2: i64 = 20;
    let n2 = stmt
        .execute(&[&v2 as &dyn ToSqlValue])
        .await
        .expect("execute insert 20");
    assert_eq!(n2, 1);

    let rows = conn
        .query("SELECT v FROM nums ORDER BY v", &[])
        .await
        .expect("SELECT");
    assert_eq!(rows.len(), 2);
}

#[tokio::test]
async fn test_prepare_sql_accessor() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    let sql = "SELECT 1";
    let stmt = conn.prepare(sql).await.expect("prepare should succeed");
    assert_eq!(stmt.sql(), sql);
}

#[tokio::test]
async fn test_prepare_reuse_multiple_times() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");
    for i in 0_i64..10 {
        conn.execute("INSERT INTO t VALUES ($1)", &[&i as &dyn ToSqlValue])
            .await
            .expect("INSERT");
    }

    let mut stmt = conn
        .prepare("SELECT id FROM t WHERE id = $1")
        .await
        .expect("prepare should succeed");
    for i in 0_i64..10 {
        let rows = stmt
            .query(&[&i as &dyn ToSqlValue])
            .await
            .expect("query by id");
        assert_eq!(rows.len(), 1);
    }
}

// ── Schema introspection tests ─────────────────────────────────────────────────
// NOTE: These tests previously expected `Err("schema introspection not supported")`.
// The embedded backend now implements full introspection via the GlueSQL catalog
// (fetch_all_schemas / fetch_schema), so the assertions are updated accordingly.

#[tokio::test]
async fn test_schema_tables_returns_ok() {
    // GlueSQL MemoryStorage exposes table metadata via Store::fetch_all_schemas.
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE my_table (id INT)", &[])
        .await
        .expect("CREATE TABLE");
    let result = conn.tables().await;
    assert!(
        result.is_ok(),
        "embedded tables() should succeed, got: {:?}",
        result
    );
    let tables = result.unwrap();
    assert_eq!(tables.len(), 1);
    assert_eq!(tables[0].name, "my_table");
}

#[tokio::test]
async fn test_schema_columns_returns_ok() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute(
        "CREATE TABLE t (id INTEGER, name TEXT, active BOOLEAN)",
        &[],
    )
    .await
    .expect("CREATE TABLE");

    let result = conn.columns("t").await;
    assert!(
        result.is_ok(),
        "embedded columns() should succeed, got: {:?}",
        result
    );
    let cols = result.unwrap();
    assert_eq!(cols.len(), 3);
    assert_eq!(cols[0].name, "id");
    assert_eq!(cols[1].name, "name");
    assert_eq!(cols[2].name, "active");
}

#[tokio::test]
async fn test_schema_indexes_returns_ok() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    let result = conn.indexes("t").await;
    assert!(
        result.is_ok(),
        "embedded indexes() should succeed, got: {:?}",
        result
    );
    // No explicit indexes were created, so the result should be empty.
    assert!(result.unwrap().is_empty());
}

#[tokio::test]
async fn test_schema_foreign_keys_returns_ok() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");
    conn.execute("CREATE TABLE t (id INTEGER)", &[])
        .await
        .expect("CREATE TABLE");

    let result = conn.foreign_keys("t").await;
    assert!(
        result.is_ok(),
        "embedded foreign_keys() should succeed, got: {:?}",
        result
    );
    // No FK constraints were declared, so the result should be empty.
    assert!(result.unwrap().is_empty());
}
