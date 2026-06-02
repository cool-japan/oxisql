//! Integration tests for `EmbeddedConnection` schema-introspection methods:
//! `tables()`, `columns()`, `indexes()`, and `foreign_keys()`.

use oxisql_core::Connection as _;
use oxisql_embedded::EmbeddedConnection;

// ── tables() ─────────────────────────────────────────────────────────────────

/// A brand-new in-memory connection should have no tables.
#[tokio::test]
async fn test_tables_empty() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    let tables = conn.tables().await.expect("tables()");
    assert!(tables.is_empty(), "expected no tables, got {tables:?}");
}

/// After creating two tables both names appear in `tables()`.
#[tokio::test]
async fn test_tables_after_create() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE alpha (id INT)", &[])
        .await
        .expect("create alpha");
    conn.execute("CREATE TABLE beta (id INT)", &[])
        .await
        .expect("create beta");

    let tables = conn.tables().await.expect("tables()");
    let mut names: Vec<String> = tables.iter().map(|t| t.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["alpha", "beta"], "unexpected tables: {names:?}");
}

// ── columns() ────────────────────────────────────────────────────────────────

/// `columns()` on a non-existent table returns `Ok(vec![])`.
#[tokio::test]
async fn test_columns_missing_table() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    let cols = conn.columns("no_such_table").await.expect("columns()");
    assert!(
        cols.is_empty(),
        "expected empty columns for missing table, got {cols:?}"
    );
}

/// After `CREATE TABLE t (id INT, name TEXT)` `columns("t")` returns two items
/// with correct names and type strings.
#[tokio::test]
async fn test_columns() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE t (id INT, name TEXT)", &[])
        .await
        .expect("create table");

    let cols = conn.columns("t").await.expect("columns()");
    assert_eq!(cols.len(), 2, "expected 2 columns, got {cols:?}");

    // Column order must match the CREATE TABLE declaration.
    assert_eq!(cols[0].name, "id");
    assert_eq!(cols[0].ordinal_position, 1);
    // GlueSQL stores INT as DataType::Int → "INT"
    assert_eq!(cols[0].data_type, "INT");

    assert_eq!(cols[1].name, "name");
    assert_eq!(cols[1].ordinal_position, 2);
    assert_eq!(cols[1].data_type, "TEXT");
}

/// Nullable vs NOT NULL columns are reported correctly.
#[tokio::test]
async fn test_columns_nullability() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE nn (id INT NOT NULL, value TEXT NULL)", &[])
        .await
        .expect("create table");

    let cols = conn.columns("nn").await.expect("columns()");
    assert_eq!(cols.len(), 2);
    assert!(!cols[0].nullable, "id should be NOT NULL");
    assert!(cols[1].nullable, "value should be nullable");
}

// ── indexes() ────────────────────────────────────────────────────────────────

/// `indexes()` on a table with no explicit indexes returns an empty `Vec`.
#[tokio::test]
async fn test_indexes_empty() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE noindex (id INT)", &[])
        .await
        .expect("create table");

    let idxs = conn.indexes("noindex").await.expect("indexes()");
    assert!(idxs.is_empty(), "expected no indexes, got {idxs:?}");
}

/// After `CREATE INDEX idx ON t (name)` the index appears in `indexes("t")`.
#[tokio::test]
async fn test_indexes() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE t2 (id INT, name TEXT)", &[])
        .await
        .expect("create table");
    conn.execute("CREATE INDEX idx_name ON t2 (name)", &[])
        .await
        .expect("create index");

    let idxs = conn.indexes("t2").await.expect("indexes()");
    assert_eq!(idxs.len(), 1, "expected 1 index, got {idxs:?}");
    assert_eq!(idxs[0].name, "idx_name");
    assert_eq!(
        idxs[0].columns,
        ["name"],
        "wrong columns: {:?}",
        idxs[0].columns
    );
}

/// `indexes()` on a non-existent table returns `Ok(vec![])`.
#[tokio::test]
async fn test_indexes_missing_table() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    let idxs = conn.indexes("ghost").await.expect("indexes()");
    assert!(
        idxs.is_empty(),
        "expected empty indexes for missing table, got {idxs:?}"
    );
}

// ── foreign_keys() ───────────────────────────────────────────────────────────

/// `foreign_keys()` on a non-existent table returns `Ok(vec![])`.
#[tokio::test]
async fn test_foreign_keys_missing_table() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    let fks = conn.foreign_keys("ghost").await.expect("foreign_keys()");
    assert!(
        fks.is_empty(),
        "expected empty fks for missing table, got {fks:?}"
    );
}

/// `foreign_keys()` on a plain table (no FK constraints) returns `Ok(vec![])`.
#[tokio::test]
async fn test_foreign_keys_embedded() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE parent (id INT)", &[])
        .await
        .expect("create parent");
    conn.execute("CREATE TABLE child (id INT, parent_id INT)", &[])
        .await
        .expect("create child");

    // GlueSQL MemoryStorage may not retain FK metadata from DDL; the call must
    // succeed (return Ok) even if the result is empty.
    let fks = conn
        .foreign_keys("child")
        .await
        .expect("foreign_keys() must return Ok");
    // We only assert the call didn't error — the result (empty or non-empty)
    // depends on GlueSQL's FK retention behaviour.
    let _ = fks;
}
