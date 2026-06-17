//! Schema re-prepare tests.
//!
//! Verifies that statements cached by the LRU cache are transparently
//! re-compiled when the database schema changes (DDL, CREATE INDEX, ALTER TABLE).
//!
//! Previously a fragile `is_ddl` keyword-prefix heuristic bypassed the cache
//! for statements beginning with CREATE/DROP/ALTER/PRAGMA/VACUUM — but it
//! failed on comment-prefixed DDL and left DML statements stale after
//! subsequent schema changes.
//!
//! These tests exercise the new unified re-prepare-on-schema-change path: the
//! engine signals `SchemaChanged` when a compiled statement's cookie no longer
//! matches the live schema cookie, and `exec_rewritten` discards the stale
//! program, re-prepares, and retries exactly once.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── comment_prefixed_ddl_replay_is_safe ──────────────────────────────────────

/// A DDL statement with a leading block comment must survive being run twice.
///
/// The old `is_ddl` heuristic inspected the first SQL keyword after
/// whitespace.  A leading `/* … */` comment meant the heuristic missed the
/// DDL classification and cached the statement.  The first execution bumped
/// the schema cookie; the second execution of the (now stale) cached statement
/// triggered `SchemaChanged`.  The new unified path re-prepares and retries
/// transparently, so both executions succeed.
#[tokio::test]
async fn comment_prefixed_ddl_replay_is_safe() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    // First execution: compiles and enters the cache.
    conn.execute(
        "/* migration 0001 */ CREATE TABLE IF NOT EXISTS foo (id INTEGER PRIMARY KEY, v TEXT)",
        &[],
    )
    .await
    .expect("first CREATE (comment-prefixed) failed");

    // Second execution: cache hit on a statement compiled with the old schema
    // cookie (before the first run created the table and bumped the cookie).
    // The engine signals SchemaChanged → re-prepare → retry → success.
    conn.execute(
        "/* migration 0001 */ CREATE TABLE IF NOT EXISTS foo (id INTEGER PRIMARY KEY, v TEXT)",
        &[],
    )
    .await
    .expect("second CREATE (re-prepare after SchemaChanged) failed");

    // Verify the table is usable after the double DDL.
    conn.execute(
        "INSERT INTO foo (id, v) VALUES ($1, $2)",
        &[&1i64, &"hello"],
    )
    .await
    .expect("INSERT after double CREATE failed");

    let rows = conn
        .query("SELECT v FROM foo WHERE id = $1", &[&1i64])
        .await
        .expect("SELECT failed");

    assert_eq!(rows.len(), 1, "expected 1 row, got {}", rows.len());
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::Text("hello".to_string())),
        "unexpected value in row 0"
    );
}

// ── schema_change_then_reuse_cached_insert ────────────────────────────────────

/// A cached INSERT executed after a `CREATE INDEX` (which bumps the cookie)
/// must transparently re-prepare and succeed.
///
/// Flow:
/// 1. `CREATE TABLE` → schema cookie N+1.
/// 2. First INSERT compiled at N+1, placed in cache.
/// 3. `CREATE INDEX` → schema cookie N+2.
/// 4. Second INSERT: cache hit, stmt compiled at N+1 ≠ N+2 → `SchemaChanged`
///    → re-prepare at N+2 → retry → both rows present.
#[tokio::test]
async fn schema_change_then_reuse_cached_insert() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");

    let insert_sql = "INSERT INTO t (id, v) VALUES ($1, $2)";

    let v1 = "first";
    conn.execute(insert_sql, &[&1i64, &v1])
        .await
        .expect("first INSERT failed");

    // Bump the schema cookie.
    conn.execute("CREATE INDEX idx ON t(v)", &[])
        .await
        .expect("CREATE INDEX failed");

    // The cached INSERT was compiled before CREATE INDEX; the engine raises
    // SchemaChanged.  exec_rewritten re-prepares and retries transparently.
    let v2 = "second";
    conn.execute(insert_sql, &[&2i64, &v2])
        .await
        .expect("second INSERT after schema change failed");

    let rows = conn
        .query("SELECT id, v FROM t ORDER BY id", &[])
        .await
        .expect("SELECT failed");

    assert_eq!(rows.len(), 2, "expected 2 rows, got {}", rows.len());
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(1)));
    assert_eq!(
        rows[0].get_by_index(1),
        Some(&Value::Text("first".to_string()))
    );
    assert_eq!(rows[1].get_by_index(0), Some(&Value::I64(2)));
    assert_eq!(
        rows[1].get_by_index(1),
        Some(&Value::Text("second".to_string()))
    );
}

// ── alter_add_column_then_cached_stmt ─────────────────────────────────────────

/// A cached INSERT executed after `ALTER TABLE … ADD COLUMN` must transparently
/// re-prepare and succeed.
///
/// Flow:
/// 1. `CREATE TABLE t (a TEXT)` → schema cookie N+1.
/// 2. First `INSERT INTO t (a) VALUES (?)` compiled at N+1, cached.
/// 3. `ALTER TABLE t ADD COLUMN b TEXT` → schema cookie N+2.
/// 4. Second INSERT: cache hit, stale cookie N+1 ≠ N+2 → `SchemaChanged`
///    → re-prepare at N+2 → retry → success.
/// 5. `SELECT a, b FROM t`: both rows present; new column `b` is NULL for both
///    (the pre-ALTER row inherits the NULL default and the re-prepared INSERT
///    only specifies `a`).
#[tokio::test]
async fn alter_add_column_then_cached_stmt() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t (a TEXT)", &[])
        .await
        .expect("CREATE TABLE failed");

    let insert_sql = "INSERT INTO t (a) VALUES ($1)";

    let a1 = "x";
    conn.execute(insert_sql, &[&a1])
        .await
        .expect("first INSERT failed");

    // Bump the schema cookie by adding a column.
    conn.execute("ALTER TABLE t ADD COLUMN b TEXT", &[])
        .await
        .expect("ALTER TABLE ADD COLUMN failed");

    // The cached INSERT is stale; the engine signals SchemaChanged.
    // exec_rewritten re-prepares against the updated schema and retries.
    let a2 = "y";
    conn.execute(insert_sql, &[&a2])
        .await
        .expect("second INSERT after ALTER TABLE failed");

    // Both rows must be present.  The new column `b` is NULL for both because
    // neither INSERT specified it (it receives the column's NULL default).
    let rows = conn
        .query("SELECT a, b FROM t ORDER BY a", &[])
        .await
        .expect("SELECT failed");

    assert_eq!(
        rows.len(),
        2,
        "expected 2 rows after ALTER, got {}",
        rows.len()
    );

    // Row 'x' — inserted before ALTER TABLE.
    assert_eq!(
        rows[0].get_by_index(0),
        Some(&Value::Text("x".to_string())),
        "row 0, column a"
    );
    assert_eq!(
        rows[0].get_by_index(1),
        Some(&Value::Null),
        "row 0, column b"
    );

    // Row 'y' — inserted after ALTER TABLE via the re-prepared statement.
    assert_eq!(
        rows[1].get_by_index(0),
        Some(&Value::Text("y".to_string())),
        "row 1, column a"
    );
    assert_eq!(
        rows[1].get_by_index(1),
        Some(&Value::Null),
        "row 1, column b"
    );
}
