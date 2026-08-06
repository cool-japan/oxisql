//! Integration tests for the per-connection multi-database registry: `TEMP`
//! objects (Q11d) and `ATTACH`/`DETACH` (Q11b).
//!
//! Behaviour is checked against upstream SQLite semantics:
//!
//! * `CREATE TEMP TABLE` / `TEMP VIEW` / `TEMP TRIGGER` live in a private
//!   per-connection catalog, are invisible to other connections, and vanish when
//!   the connection closes;
//! * an unqualified name resolves `temp` before `main` before attached
//!   databases, and a schema qualifier addresses exactly the database named;
//! * `ATTACH` opens a real database file whose writes are visible when the file
//!   is reopened directly, and `DETACH` closes it;
//! * the opcodes that used to be `todo!("temp databases not implemented yet")`
//!   -- `CreateBtree`, `Destroy`, `DropTable`, `PageCount`, `ReadCookie`,
//!   `SetCookie` with a non-`main` database -- now execute for real, the last
//!   three via `PRAGMA <schema>.user_version` / `application_id` /
//!   `schema_version` / `page_count`.

use std::sync::Arc;

use limbo_core::{Connection, Database, StepResult, Value};

fn new_io() -> Arc<dyn limbo_core::IO> {
    Arc::new(limbo_core::SyscallIO::new().expect("syscall IO backend"))
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_multidb_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ))
}

fn cleanup(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

struct Db {
    io: Arc<dyn limbo_core::IO>,
    db: Arc<Database>,
    conn: Arc<Connection>,
    path: std::path::PathBuf,
}

fn open(tag: &str) -> Db {
    let path = temp_db_path(tag);
    cleanup(&path);
    open_at(path)
}

fn open_at(path: std::path::PathBuf) -> Db {
    let io = new_io();
    let db = Database::open_file(
        io.clone(),
        path.to_str().expect("temp path is valid UTF-8"),
        false,
    )
    .expect("open database");
    let conn = db.connect().expect("connect");
    Db { io, db, conn, path }
}

/// Close the connection and reopen the same file from scratch.
fn reopen(db: Db) -> Db {
    let Db {
        io: _,
        db: _,
        conn,
        path,
    } = db;
    conn.close().expect("close connection");
    drop(conn);
    open_at(path)
}

fn exec(db: &Db, sql: &str) {
    db.conn
        .execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn exec_on(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn exec_err(db: &Db, sql: &str) -> String {
    match db.conn.execute(sql) {
        Ok(()) => panic!("expected an error from {sql}, but it succeeded"),
        Err(e) => format!("{e}"),
    }
}

fn read_rows_on(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<Connection>,
    sql: &str,
) -> Vec<Vec<Value>> {
    let mut stmt = conn
        .query(sql)
        .unwrap_or_else(|e| panic!("prepare failed for {sql}: {e:?}"))
        .unwrap_or_else(|| panic!("no statement produced for {sql}"));
    let mut out = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step failed for {sql}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                out.push(row.get_values().cloned().collect::<Vec<_>>());
            }
            StepResult::IO => io.run_once().expect("io"),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

fn read_rows(db: &Db, sql: &str) -> Vec<Vec<Value>> {
    read_rows_on(&db.io, &db.conn, sql)
}

fn read_col(db: &Db, sql: &str) -> Vec<Value> {
    read_rows(db, sql)
        .into_iter()
        .map(|mut row| row.drain(..).next().expect("at least one column"))
        .collect()
}

fn ints(values: Vec<Value>) -> Vec<i64> {
    values
        .into_iter()
        .map(|v| match v {
            Value::Integer(i) => i,
            other => panic!("expected an integer, got {other:?}"),
        })
        .collect()
}

fn texts(values: Vec<Value>) -> Vec<String> {
    values
        .into_iter()
        .map(|v| match v {
            Value::Text(t) => t.as_str().to_string(),
            other => panic!("expected text, got {other:?}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// TEMP tables (Q11d)
// ---------------------------------------------------------------------------

#[test]
fn temp_table_round_trips_create_insert_select_drop() {
    let db = open("temp_crud");
    exec(&db, "CREATE TEMP TABLE t (a, b)");
    exec(&db, "INSERT INTO t VALUES (1, 'one'), (2, 'two')");

    assert_eq!(
        ints(read_col(&db, "SELECT a FROM t ORDER BY a")),
        vec![1, 2]
    );
    assert_eq!(
        texts(read_col(&db, "SELECT b FROM temp.t ORDER BY a")),
        vec!["one".to_string(), "two".to_string()]
    );

    // UPDATE and DELETE on a temp table exercise the `db > 0` write path.
    exec(&db, "UPDATE t SET b = 'ONE' WHERE a = 1");
    assert_eq!(
        texts(read_col(&db, "SELECT b FROM t WHERE a = 1")),
        vec!["ONE".to_string()]
    );
    exec(&db, "DELETE FROM t WHERE a = 2");
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![1]);

    exec(&db, "DROP TABLE t");
    let err = exec_err(&db, "SELECT a FROM t");
    assert!(
        err.to_lowercase().contains("t"),
        "dropped temp table must be gone: {err}"
    );
    cleanup(&db.path);
}

#[test]
fn temp_table_is_invisible_to_another_connection() {
    let db = open("temp_invisible");
    exec(&db, "CREATE TEMP TABLE secret (a)");
    exec(&db, "INSERT INTO secret VALUES (42)");

    let other = db.db.connect().expect("second connection");
    let err = other
        .execute("SELECT a FROM secret")
        .expect_err("a temp table must not be visible to another connection");
    let err = format!("{err}");
    assert!(
        err.to_lowercase().contains("secret"),
        "expected 'no such table' for the other connection, got: {err}"
    );

    // The owning connection still sees it.
    assert_eq!(ints(read_col(&db, "SELECT a FROM secret")), vec![42]);
    cleanup(&db.path);
}

#[test]
fn temp_table_does_not_survive_connection_close() {
    let db = open("temp_lifetime");
    exec(&db, "CREATE TABLE persistent (a)");
    exec(&db, "CREATE TEMP TABLE ephemeral (a)");
    exec(&db, "INSERT INTO ephemeral VALUES (7)");
    exec(&db, "INSERT INTO persistent VALUES (7)");

    let db = reopen(db);
    assert_eq!(ints(read_col(&db, "SELECT a FROM persistent")), vec![7]);
    let err = exec_err(&db, "SELECT a FROM ephemeral");
    assert!(
        err.to_lowercase().contains("ephemeral"),
        "a temp table must not outlive its connection: {err}"
    );
    // Nothing about the temp table leaked into the persistent catalog.
    assert!(
        texts(read_col(
            &db,
            "SELECT name FROM sqlite_schema WHERE type = 'table'"
        ))
        .iter()
        .all(|name| name != "ephemeral"),
        "a temp table must never be written to main's sqlite_schema"
    );
    cleanup(&db.path);
}

#[test]
fn temp_table_shadows_a_main_table_of_the_same_name() {
    let db = open("temp_shadow");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (100)");
    exec(&db, "CREATE TEMP TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (1)");

    // Unqualified: temp wins, matching upstream's temp -> main -> attached order.
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![1]);
    // Qualified: each name addresses exactly its own database.
    assert_eq!(ints(read_col(&db, "SELECT a FROM temp.t")), vec![1]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM main.t")), vec![100]);

    // Dropping the temp table un-shadows main's.
    exec(&db, "DROP TABLE temp.t");
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![100]);
    cleanup(&db.path);
}

#[test]
fn temp_view_resolves_against_the_temp_catalog() {
    let db = open("temp_view");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (1), (2), (3)");
    exec(&db, "CREATE TEMP VIEW v AS SELECT a FROM t WHERE a > 1");

    assert_eq!(
        ints(read_col(&db, "SELECT a FROM v ORDER BY a")),
        vec![2, 3]
    );
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM temp.v ORDER BY a")),
        vec![2, 3]
    );

    let db = reopen(db);
    let err = exec_err(&db, "SELECT a FROM v");
    assert!(
        err.to_lowercase().contains("v"),
        "a temp view must not outlive its connection: {err}"
    );
    cleanup(&db.path);
}

#[test]
fn temp_trigger_fires_and_is_not_persisted() {
    // Supersedes the earlier `temporary_trigger_is_rejected_rather_than_silently_persisted`
    // guard: now that `temp` exists, a TEMP trigger is stored in the temp
    // catalog, fires like any other trigger, and disappears with the connection.
    let db = open("temp_trigger");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TABLE log (a)");
    exec(
        &db,
        "CREATE TEMP TRIGGER t_ai AFTER INSERT ON t BEGIN INSERT INTO log VALUES (NEW.a); END",
    );

    exec(&db, "INSERT INTO t VALUES (5)");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM log")),
        vec![5],
        "a TEMP trigger must actually fire"
    );

    // Its row lives in temp's catalog, never in main's sqlite_schema.
    assert!(
        texts(read_col(
            &db,
            "SELECT name FROM sqlite_schema WHERE type = 'trigger'"
        ))
        .is_empty(),
        "a TEMP trigger must not be written to main's sqlite_schema"
    );

    let db = reopen(db);
    exec(&db, "INSERT INTO t VALUES (6)");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM log")),
        vec![5],
        "a TEMP trigger must not survive the connection that created it"
    );
    cleanup(&db.path);
}

#[test]
fn temp_table_ddl_and_dml_exercise_the_former_todo_paths() {
    // The `CreateBtree` / `Destroy` / `DropTable` former-`todo!()` sites, all
    // with `db == 1`. (`PageCount`/`ReadCookie`/`SetCookie` are covered by
    // `pragma_header_cookies_are_per_database` below.)
    let db = open("temp_todo_paths");
    exec(&db, "CREATE TEMP TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (1)");
    exec(&db, "UPDATE t SET a = a + 1");
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![2]);
    exec(&db, "DROP TABLE t");

    // A second create/drop cycle proves the temp pager is reusable, not
    // one-shot.
    exec(&db, "CREATE TEMP TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (9)");
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![9]);
    exec(&db, "DROP TABLE t");
    cleanup(&db.path);
}

#[test]
fn pragma_header_cookies_are_per_database() {
    // `PageCount`, `ReadCookie` and `SetCookie` with `db > 0` were three of the
    // six `todo!("temp databases not implemented yet")` sites. They now read and
    // write the header of the database actually named, and -- critically -- a
    // write to one database must not be observable in another.
    let db = open("pragma_per_db");
    let side_path = temp_db_path("pragma_per_db_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE t (a)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.s (a)");
    // Materialize `temp` so `PRAGMA temp.*` has a database to talk to.
    exec(&db, "CREATE TEMP TABLE tmp (a)");

    exec(&db, "PRAGMA main.user_version = 11");
    exec(&db, "PRAGMA temp.user_version = 22");
    exec(&db, "PRAGMA side.user_version = 33");

    assert_eq!(ints(read_col(&db, "PRAGMA main.user_version")), vec![11]);
    assert_eq!(ints(read_col(&db, "PRAGMA temp.user_version")), vec![22]);
    assert_eq!(ints(read_col(&db, "PRAGMA side.user_version")), vec![33]);
    // Unqualified still means `main`, as upstream.
    assert_eq!(ints(read_col(&db, "PRAGMA user_version")), vec![11]);

    exec(&db, "PRAGMA side.application_id = 4242");
    assert_eq!(
        ints(read_col(&db, "PRAGMA side.application_id")),
        vec![4242]
    );
    assert_eq!(ints(read_col(&db, "PRAGMA main.application_id")), vec![0]);

    // `page_count` reports each database's own size.
    let main_pages = ints(read_col(&db, "PRAGMA main.page_count"));
    let side_pages = ints(read_col(&db, "PRAGMA side.page_count"));
    let temp_pages = ints(read_col(&db, "PRAGMA temp.page_count"));
    for pages in [&main_pages, &side_pages, &temp_pages] {
        assert_eq!(pages.len(), 1);
        assert!(pages[0] > 0, "every open database has at least page 1");
    }

    // Re-opening the attached file directly must show its own cookie, proving
    // the write went to that file's header rather than main's.
    exec(&db, "DETACH DATABASE side");
    db.conn.close().expect("close owning connection");
    drop(db.conn);
    let reopened = open_at(side_path.clone());
    assert_eq!(ints(read_col(&reopened, "PRAGMA user_version")), vec![33]);
    assert_eq!(
        ints(read_col(&reopened, "PRAGMA application_id")),
        vec![4242]
    );
    cleanup(&reopened.path);
    cleanup(&db.path);
}

#[test]
fn schema_qualified_pragma_that_cannot_be_routed_is_refused() {
    // A pragma whose value does not live in a per-database header would be
    // answered from `main` if it were allowed through -- an answer about the
    // wrong database. It is a typed error instead.
    let db = open("pragma_refused");
    exec(&db, "CREATE TEMP TABLE t (a)");
    let err = exec_err(&db, "PRAGMA temp.cache_size = 100");
    assert!(
        err.contains("main database only"),
        "an unroutable schema-qualified pragma must be refused: {err}"
    );
    let err = exec_err(&db, "PRAGMA nosuchdb.user_version");
    assert!(
        err.contains("unknown database nosuchdb"),
        "an unknown schema qualifier must be reported as such: {err}"
    );
    // The unqualified and `main.`-qualified forms keep working unchanged.
    exec(&db, "PRAGMA cache_size = 100");
    exec(&db, "PRAGMA main.cache_size = 100");
    cleanup(&db.path);
}

// ---------------------------------------------------------------------------
// ATTACH / DETACH (Q11b)
// ---------------------------------------------------------------------------

#[test]
fn attach_round_trips_through_a_real_file() {
    let db = open("attach_roundtrip");
    let side_path = temp_db_path("attach_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.t (a, b)");
    exec(&db, "INSERT INTO side.t VALUES (1, 'x'), (2, 'y')");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM side.t ORDER BY a")),
        vec![1, 2]
    );

    // UPDATE on an attached table: the write path with `db > 1`.
    exec(&db, "UPDATE side.t SET b = 'Z' WHERE a = 2");
    assert_eq!(
        texts(read_col(&db, "SELECT b FROM side.t WHERE a = 2")),
        vec!["Z".to_string()]
    );

    exec(&db, "DETACH DATABASE side");
    db.conn.close().expect("close owning connection");
    drop(db.conn);

    // Reopen the attached file on its own: the data must be there.
    let reopened = open_at(side_path.clone());
    assert_eq!(
        texts(read_col(&reopened, "SELECT b FROM t ORDER BY a")),
        vec!["x".to_string(), "Z".to_string()]
    );
    cleanup(&reopened.path);
    cleanup(&db.path);
}

#[test]
fn attached_table_is_reachable_unqualified_when_nothing_shadows_it() {
    let db = open("attach_unqualified");
    let side_path = temp_db_path("attach_unqual_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE only_main (a)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.only_side (a)");
    exec(&db, "INSERT INTO only_side VALUES (3)");

    assert_eq!(ints(read_col(&db, "SELECT a FROM only_side")), vec![3]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM side.only_side")), vec![3]);
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn main_shadows_an_attached_table_of_the_same_name() {
    let db = open("attach_shadow");
    let side_path = temp_db_path("attach_shadow_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (100)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.t (a)");
    exec(&db, "INSERT INTO side.t VALUES (200)");

    // `main` outranks an attached database for an unqualified name.
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![100]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM side.t")), vec![200]);

    // ... and `temp` outranks both.
    exec(&db, "CREATE TEMP TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (300)");
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![300]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM main.t")), vec![100]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM side.t")), vec![200]);
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn cross_database_statement_reads_both_databases() {
    let db = open("attach_cross");
    let side_path = temp_db_path("attach_cross_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE m (a)");
    exec(&db, "INSERT INTO m VALUES (1), (2), (3)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.s (a)");

    // One statement writing an attached table from a main-table scan: both
    // pagers are in the same transaction.
    exec(&db, "INSERT INTO side.s SELECT a FROM m WHERE a > 1");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM side.s ORDER BY a")),
        vec![2, 3]
    );

    // And a join across the two databases.
    let joined = ints(read_col(
        &db,
        "SELECT m.a FROM m JOIN side.s ON m.a = side.s.a ORDER BY m.a",
    ));
    assert_eq!(joined, vec![2, 3]);
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn attach_rejects_duplicate_and_reserved_aliases() {
    let db = open("attach_dup");
    let side_path = temp_db_path("attach_dup_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    let err = exec_err(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    assert!(
        err.contains("already in use"),
        "re-attaching an alias must fail: {err}"
    );
    let err = exec_err(&db, &format!("ATTACH DATABASE '{side}' AS main"));
    assert!(
        err.contains("already in use"),
        "'main' must not be re-attachable: {err}"
    );
    let err = exec_err(&db, &format!("ATTACH DATABASE '{side}' AS temp"));
    assert!(
        err.contains("already in use"),
        "'temp' must not be re-attachable: {err}"
    );
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn detach_error_paths_are_typed() {
    let db = open("detach_errors");
    let side_path = temp_db_path("detach_errors_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    let err = exec_err(&db, "DETACH DATABASE nosuch");
    assert!(
        err.contains("no such database"),
        "detaching an unknown alias must be a typed error: {err}"
    );
    let err = exec_err(&db, "DETACH DATABASE main");
    assert!(
        err.contains("cannot detach"),
        "'main' must never be detachable: {err}"
    );
    let err = exec_err(&db, "DETACH DATABASE temp");
    assert!(
        err.contains("cannot detach"),
        "'temp' must never be detachable: {err}"
    );

    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "BEGIN");
    let err = exec_err(&db, "DETACH DATABASE side");
    assert!(
        err.to_lowercase().contains("transaction"),
        "DETACH inside a transaction must be refused: {err}"
    );
    exec(&db, "COMMIT");
    exec(&db, "DETACH DATABASE side");

    // After DETACH the alias no longer resolves as a database.
    let err = exec_err(&db, "SELECT * FROM side.anything");
    assert!(
        err.contains("unknown database side"),
        "a detached alias must stop resolving: {err}"
    );
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn attach_inside_a_transaction_is_refused() {
    let db = open("attach_in_txn");
    let side_path = temp_db_path("attach_in_txn_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "BEGIN");
    let err = exec_err(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    assert!(
        err.to_lowercase().contains("transaction"),
        "ATTACH inside a transaction must be refused: {err}"
    );
    exec(&db, "COMMIT");
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn unknown_schema_qualifier_is_an_error_not_a_silent_main_lookup() {
    let db = open("unknown_qualifier");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "INSERT INTO t VALUES (1)");

    let err = exec_err(&db, "SELECT a FROM nosuchdb.t");
    assert!(
        err.contains("unknown database nosuchdb"),
        "an unknown schema qualifier must not silently resolve against main: {err}"
    );
    cleanup(&db.path);
}

#[test]
fn attached_memory_database_works_without_touching_the_filesystem() {
    let db = open("attach_memory");
    exec(&db, "ATTACH DATABASE ':memory:' AS scratch");
    exec(&db, "CREATE TABLE scratch.t (a)");
    exec(&db, "INSERT INTO scratch.t VALUES (1), (2)");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM scratch.t ORDER BY a")),
        vec![1, 2]
    );
    exec(&db, "DETACH DATABASE scratch");
    cleanup(&db.path);
}

#[test]
fn attach_with_key_is_refused_rather_than_silently_unencrypted() {
    let db = open("attach_key");
    let side_path = temp_db_path("attach_key_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");
    let err = exec_err(&db, &format!("ATTACH DATABASE '{side}' AS s KEY 'secret'"));
    assert!(
        err.to_lowercase().contains("key"),
        "ATTACH ... KEY must be refused, never quietly ignored: {err}"
    );
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn temp_and_attached_databases_are_per_connection() {
    let db = open("per_connection");
    let side_path = temp_db_path("per_connection_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.t (a)");

    let other = db.db.connect().expect("second connection");
    let err = other
        .execute("SELECT a FROM side.t")
        .expect_err("another connection must not see this connection's ATTACH");
    assert!(
        format!("{err}").to_lowercase().contains("side"),
        "ATTACH is per-connection, like upstream: {err}"
    );

    // The second connection has its own registry: it can attach a database of
    // its own under any alias, including one the first connection is using.
    let own_path = temp_db_path("per_connection_own");
    cleanup(&own_path);
    let own = own_path.to_str().expect("temp path is valid UTF-8");
    exec_on(&other, &format!("ATTACH DATABASE '{own}' AS side"));
    exec_on(&other, "CREATE TABLE side.mine (a)");
    exec_on(&other, "INSERT INTO side.mine VALUES (11)");
    assert_eq!(
        read_rows_on(&db.io, &other, "SELECT a FROM side.mine").len(),
        1
    );
    // ... and the first connection's `side` still has no such table.
    let err = db
        .conn
        .execute("SELECT a FROM side.mine")
        .expect_err("each connection's aliases are its own");
    assert!(
        format!("{err}").to_lowercase().contains("mine"),
        "aliases must not leak between connections: {err}"
    );
    other.close().expect("close second connection");
    cleanup(&own_path);
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn drop_table_in_an_attached_database_removes_only_that_table() {
    // `DROP TABLE` is the most database-sensitive DDL path: it scans the owning
    // `sqlite_schema`, destroys b-trees, and (for the root-page-move case) opens
    // a second schema cursor. All of that must address the attached database.
    let db = open("attach_drop");
    let side_path = temp_db_path("attach_drop_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE keep (a)");
    exec(&db, "INSERT INTO keep VALUES (1)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.gone (a)");
    exec(&db, "CREATE TABLE side.stays (a)");
    exec(&db, "INSERT INTO side.gone VALUES (2)");
    exec(&db, "INSERT INTO side.stays VALUES (3)");

    exec(&db, "DROP TABLE side.gone");
    let err = exec_err(&db, "SELECT a FROM side.gone");
    assert!(
        err.to_lowercase().contains("gone"),
        "the dropped table must be gone from the attached database: {err}"
    );
    assert_eq!(ints(read_col(&db, "SELECT a FROM side.stays")), vec![3]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM keep")), vec![1]);

    exec(&db, "DETACH DATABASE side");
    db.conn.close().expect("close owning connection");
    drop(db.conn);
    let reopened = open_at(side_path.clone());
    assert_eq!(
        texts(read_col(
            &reopened,
            "SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name"
        )),
        vec!["stays".to_string()],
        "the drop must be persisted in the attached file"
    );
    cleanup(&reopened.path);
    cleanup(&db.path);
}

#[test]
fn explicit_transaction_spanning_two_databases_commits_and_rolls_back() {
    // Outside autocommit, the multi-database commit walk only runs at COMMIT --
    // and ROLLBACK has to roll back every database the transaction touched, not
    // just `main`.
    let db = open("multi_txn");
    let side_path = temp_db_path("multi_txn_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");

    exec(&db, "CREATE TABLE m (a)");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));
    exec(&db, "CREATE TABLE side.s (a)");
    exec(&db, "CREATE TEMP TABLE t (a)");

    exec(&db, "BEGIN");
    exec(&db, "INSERT INTO m VALUES (1)");
    exec(&db, "INSERT INTO side.s VALUES (2)");
    exec(&db, "INSERT INTO t VALUES (3)");
    exec(&db, "COMMIT");
    assert_eq!(ints(read_col(&db, "SELECT a FROM m")), vec![1]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM side.s")), vec![2]);
    assert_eq!(ints(read_col(&db, "SELECT a FROM t")), vec![3]);

    exec(&db, "BEGIN");
    exec(&db, "INSERT INTO m VALUES (10)");
    exec(&db, "INSERT INTO side.s VALUES (20)");
    exec(&db, "INSERT INTO t VALUES (30)");
    exec(&db, "ROLLBACK");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM m")),
        vec![1],
        "ROLLBACK must undo main"
    );
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM side.s")),
        vec![2],
        "ROLLBACK must undo the attached database too"
    );
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM t")),
        vec![3],
        "ROLLBACK must undo temp too"
    );
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn ddl_on_an_auxiliary_database_inside_an_explicit_transaction() {
    // DDL emits `CreateBtree`/`OpenWrite`/`ParseSchema` against the auxiliary
    // database. Inside an explicit transaction the auxiliary write transaction
    // stays open across statements and is only ended at COMMIT, so the lazy
    // `begin_aux_txn` path must be re-entrant.
    let db = open("aux_ddl_txn");
    let side_path = temp_db_path("aux_ddl_txn_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));

    exec(&db, "BEGIN");
    exec(&db, "CREATE TABLE side.a (x)");
    exec(&db, "CREATE TEMP TABLE b (x)");
    exec(&db, "INSERT INTO side.a VALUES (1)");
    exec(&db, "INSERT INTO b VALUES (2)");
    exec(&db, "COMMIT");

    assert_eq!(ints(read_col(&db, "SELECT x FROM side.a")), vec![1]);
    assert_eq!(ints(read_col(&db, "SELECT x FROM b")), vec![2]);

    // And DROP inside a transaction, on both auxiliary databases.
    exec(&db, "BEGIN");
    exec(&db, "DROP TABLE side.a");
    exec(&db, "DROP TABLE b");
    exec(&db, "COMMIT");
    assert!(exec_err(&db, "SELECT x FROM side.a")
        .to_lowercase()
        .contains("a"));
    assert!(exec_err(&db, "SELECT x FROM b")
        .to_lowercase()
        .contains("b"));
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn temp_pragma_materializes_the_temp_database_on_demand() {
    // `PRAGMA temp.user_version` must work on a connection that has never
    // created a temp object -- the database is created on demand, exactly as a
    // `CREATE TEMP TABLE` would create it.
    let db = open("temp_pragma_lazy");
    assert_eq!(ints(read_col(&db, "PRAGMA temp.user_version")), vec![0]);
    exec(&db, "PRAGMA temp.user_version = 7");
    assert_eq!(ints(read_col(&db, "PRAGMA temp.user_version")), vec![7]);
    assert_eq!(
        ints(read_col(&db, "PRAGMA main.user_version")),
        vec![0],
        "writing temp's header must not touch main's"
    );
    cleanup(&db.path);
}

/// `CREATE INDEX` / `DROP INDEX` only exist with the `index_experimental`
/// feature; when they do, they must build the index b-tree in the database that
/// owns the table, not in `main`.
#[cfg(feature = "index_experimental")]
#[test]
fn index_on_a_temp_or_attached_table_lives_in_that_database() {
    let db = open("aux_index");
    let side_path = temp_db_path("aux_index_side");
    cleanup(&side_path);
    let side = side_path.to_str().expect("temp path is valid UTF-8");
    exec(&db, &format!("ATTACH DATABASE '{side}' AS side"));

    exec(&db, "CREATE TEMP TABLE t (a, b)");
    exec(&db, "CREATE INDEX t_a ON t (a)");
    exec(&db, "INSERT INTO t VALUES (2, 'x'), (1, 'y')");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM t ORDER BY a")),
        vec![1, 2]
    );
    assert_eq!(
        texts(read_col(&db, "SELECT b FROM t WHERE a = 1")),
        vec!["y".to_string()]
    );

    exec(&db, "CREATE TABLE side.s (a, b)");
    exec(&db, "CREATE INDEX side.s_a ON s (a)");
    exec(&db, "INSERT INTO side.s VALUES (5, 'p'), (4, 'q')");
    assert_eq!(
        texts(read_col(&db, "SELECT b FROM side.s WHERE a = 4")),
        vec!["q".to_string()]
    );
    // Neither index reached main's catalog.
    assert!(
        texts(read_col(
            &db,
            "SELECT name FROM main.sqlite_schema WHERE type = 'index'"
        ))
        .is_empty(),
        "an index on a temp/attached table must not be written to main"
    );

    exec(&db, "DROP INDEX t_a");
    assert_eq!(
        ints(read_col(&db, "SELECT a FROM t ORDER BY a")),
        vec![1, 2]
    );
    cleanup(&side_path);
    cleanup(&db.path);
}

#[test]
fn qualified_sqlite_master_alias_resolves() {
    // `sqlite_master` is an accepted alias for `sqlite_schema` in a qualified
    // reference too.
    let db = open("qualified_master");
    exec(&db, "CREATE TABLE t (a)");
    exec(&db, "CREATE TEMP TABLE tt (a)");
    assert_eq!(
        texts(read_col(
            &db,
            "SELECT name FROM main.sqlite_master WHERE type = 'table'"
        )),
        vec!["t".to_string()]
    );
    assert_eq!(
        texts(read_col(
            &db,
            "SELECT name FROM temp.sqlite_master WHERE type = 'table'"
        )),
        vec!["tt".to_string()]
    );
    // The unqualified name still means main's, as upstream.
    assert_eq!(
        texts(read_col(
            &db,
            "SELECT name FROM sqlite_master WHERE type = 'table'"
        )),
        vec!["t".to_string()]
    );
    cleanup(&db.path);
}
