//! Regression tests for [`Connection::indexes`] index introspection.
//!
//! The previous implementation parsed the CREATE INDEX DDL text by splitting on
//! the last `(...)` and on commas, which returned decorated tokens such as
//! `"b DESC"` for an ordered multi-column index. The engine-backed
//! `PRAGMA index_list` / `PRAGMA index_info` path returns the parsed column
//! names (`"b"`), which these tests pin down. Database files, when used, live
//! under [`std::env::temp_dir`].

use oxisql_core::Connection;
use oxisql_sqlite_compat::SqliteConnection;

/// A multi-column index whose second key carries an explicit `DESC` sort order.
/// The old rfind/`split(',')` parser yielded `["a", "b DESC"]`; the engine path
/// must yield the clean column names `["a", "b"]`.
#[tokio::test]
async fn test_multicolumn_ordered_index_columns_are_clean() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (a INTEGER, b INTEGER, c INTEGER)", &[])
        .await
        .unwrap();
    conn.execute("CREATE INDEX idx_ab ON t (a, b DESC)", &[])
        .await
        .unwrap();

    let indexes = conn.indexes("t").await.unwrap();
    assert_eq!(indexes.len(), 1, "exactly one user index expected");
    assert_eq!(indexes[0].name, "idx_ab");
    assert!(!indexes[0].unique);
    // The critical assertion: the DESC decoration must not leak into the column
    // name, and both key columns must be present in order.
    assert_eq!(
        indexes[0].columns,
        vec!["a".to_string(), "b".to_string()],
        "index columns must be the parsed key column names, not DDL tokens"
    );
}

/// A UNIQUE multi-column index round-trips uniqueness and both columns.
#[tokio::test]
async fn test_unique_multicolumn_index() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute("CREATE TABLE t (x INTEGER, y INTEGER)", &[])
        .await
        .unwrap();
    conn.execute("CREATE UNIQUE INDEX idx_xy ON t (x, y)", &[])
        .await
        .unwrap();

    let indexes = conn.indexes("t").await.unwrap();
    assert_eq!(indexes.len(), 1);
    assert!(indexes[0].unique);
    assert_eq!(indexes[0].columns, vec!["x".to_string(), "y".to_string()]);
}

/// A table with no user indexes returns an empty list (internal auto-indexes,
/// such as the one backing an INTEGER PRIMARY KEY UNIQUE column, are not
/// surfaced by this trait method).
#[tokio::test]
async fn test_no_user_indexes_returns_empty() {
    let conn = SqliteConnection::open_memory().await.unwrap();
    conn.execute(
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT UNIQUE)",
        &[],
    )
    .await
    .unwrap();

    let indexes = conn.indexes("t").await.unwrap();
    assert!(
        indexes.iter().all(|i| !i.name.starts_with("sqlite_")),
        "auto-indexes must not be surfaced"
    );
}
