#![cfg(feature = "blocking")]
//! Integration tests for the `blocking` feature of `oxisql-sqlite-compat`.
//!
//! These are synchronous tests — no `#[tokio::test]` or `.await`.

use oxisql_sqlite_compat::blocking::SqliteConnectionBlocking;

#[test]
fn open_memory_and_ping() {
    let conn = SqliteConnectionBlocking::open_memory().expect("open_memory");
    conn.ping().expect("ping");
}

#[test]
fn execute_and_query_basic_crud() {
    let conn = SqliteConnectionBlocking::open_memory().expect("open_memory");

    conn.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .expect("create table");

    conn.execute("INSERT INTO users VALUES ($1, $2)", &[&1i64, &"Alice"])
        .expect("insert");

    let rows = conn
        .query("SELECT id, name FROM users", &[])
        .expect("select");
    assert_eq!(rows.len(), 1);

    conn.execute("UPDATE users SET name = $1 WHERE id = $2", &[&"Bob", &1i64])
        .expect("update");

    conn.execute("DELETE FROM users WHERE id = $1", &[&1i64])
        .expect("delete");

    let rows_after = conn
        .query("SELECT id FROM users", &[])
        .expect("select after delete");
    assert_eq!(rows_after.len(), 0);
}

#[test]
fn transaction_commit() {
    let conn = SqliteConnectionBlocking::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .expect("create");

    let mut txn = conn.transaction().expect("begin transaction");
    txn.execute("INSERT INTO t VALUES ($1)", &[&42i64])
        .expect("insert in txn");
    txn.commit().expect("commit");

    let rows = conn
        .query("SELECT v FROM t", &[])
        .expect("select after commit");
    assert_eq!(rows.len(), 1);
}

#[test]
fn transaction_rollback() {
    let conn = SqliteConnectionBlocking::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE t (v INTEGER)", &[])
        .expect("create");

    let mut txn = conn.transaction().expect("begin transaction");
    txn.execute("INSERT INTO t VALUES ($1)", &[&99i64])
        .expect("insert in txn");
    txn.rollback().expect("rollback");

    let rows = conn
        .query("SELECT v FROM t", &[])
        .expect("select after rollback");
    assert_eq!(rows.len(), 0);
}

#[test]
fn prepare_execute_and_query() {
    let conn = SqliteConnectionBlocking::open_memory().expect("open_memory");
    conn.execute("CREATE TABLE items (id INTEGER, label TEXT)", &[])
        .expect("create");

    let mut stmt = conn
        .prepare("INSERT INTO items VALUES ($1, $2)")
        .expect("prepare insert");
    stmt.execute(&[&1i64, &"foo"]).expect("execute prepared");
    stmt.execute(&[&2i64, &"bar"]).expect("execute prepared 2");

    let mut sel = conn
        .prepare("SELECT id, label FROM items WHERE id = $1")
        .expect("prepare select");
    let rows = sel.query(&[&1i64]).expect("query prepared");
    assert_eq!(rows.len(), 1);
    assert_eq!(sel.sql(), "SELECT id, label FROM items WHERE id = $1");
}
