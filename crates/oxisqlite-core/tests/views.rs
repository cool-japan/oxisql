//! Integration tests for `CREATE VIEW` / `DROP VIEW` support in oxisqlite-core.
//!
//! Covers: schema registration, `SELECT` through views (with WHERE/ORDER BY/LIMIT,
//! aggregates, multi-way LEFT JOINs with repeated aliases, `UNION ALL` bodies,
//! views-of-views, explicit column lists), runtime DDL (`CREATE`/`DROP`,
//! duplicate-name and cross-kind errors), write refusal, cycle detection, and
//! persistence across a close/reopen of an on-disk database.

use std::sync::Arc;

use limbo_core::{Connection, Database, MemoryIO, StepResult, Value, IO};

fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory: database");
    let conn = db.connect().expect("connect to :memory: database");
    (io, conn)
}

#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
}

fn to_cell(v: &Value) -> Cell {
    match v {
        Value::Null => Cell::Null,
        Value::Integer(i) => Cell::Int(*i),
        Value::Float(f) => Cell::Real(*f),
        Value::Text(t) => Cell::Text(t.as_str().to_string()),
        other => panic!("unexpected value in test data: {other:?}"),
    }
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn run_query(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) -> Vec<Vec<Cell>> {
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
                let row = stmt.row().expect("row");
                out.push(row.get_values().map(to_cell).collect());
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

fn prepare_err(conn: &Arc<Connection>, sql: &str) -> String {
    match conn.prepare(sql) {
        Ok(_) => panic!("expected {sql:?} to fail to prepare, but it succeeded"),
        Err(e) => e.to_string(),
    }
}

fn ints(rows: &[Vec<Cell>]) -> Vec<i64> {
    rows.iter()
        .map(|r| match r[0] {
            Cell::Int(i) => i,
            ref other => panic!("expected int, got {other:?}"),
        })
        .collect()
}

fn setup_products() -> (Arc<dyn IO>, Arc<Connection>) {
    let (io, conn) = new_conn();
    exec(
        &conn,
        "CREATE TABLE products (id INTEGER, name TEXT, price INTEGER, cat INTEGER)",
    );
    exec(
        &conn,
        "INSERT INTO products VALUES \
         (1,'apple',10,1),(2,'banana',20,1),(3,'carrot',30,2),\
         (4,'daikon',40,2),(5,'egg',50,3)",
    );
    exec(&conn, "CREATE TABLE cats (id INTEGER, label TEXT)");
    exec(
        &conn,
        "INSERT INTO cats VALUES (1,'fruit'),(2,'veg'),(3,'other')",
    );
    (io, conn)
}

#[test]
fn simple_view_roundtrip() {
    let (io, conn) = setup_products();
    exec(
        &conn,
        "CREATE VIEW cheap AS SELECT id, name FROM products WHERE price <= 30",
    );
    let rows = run_query(&io, &conn, "SELECT id FROM cheap ORDER BY id");
    assert_eq!(ints(&rows), vec![1, 2, 3]);
    // View row is visible in sqlite_schema.
    let sch = run_query(
        &io,
        &conn,
        "SELECT name FROM sqlite_schema WHERE type='view' ORDER BY name",
    );
    assert_eq!(sch, vec![vec![Cell::Text("cheap".to_string())]]);
}

#[test]
fn view_where_orderby_limit_and_count() {
    let (io, conn) = setup_products();
    exec(
        &conn,
        "CREATE VIEW v AS SELECT id, name, price FROM products",
    );
    let rows = run_query(
        &io,
        &conn,
        "SELECT id FROM v WHERE price >= 20 ORDER BY price DESC LIMIT 2",
    );
    assert_eq!(ints(&rows), vec![5, 4]);
    let cnt = run_query(&io, &conn, "SELECT count(*) FROM v");
    assert_eq!(ints(&cnt), vec![5]);
    let cnt2 = run_query(&io, &conn, "SELECT count(*) FROM v WHERE price > 25");
    assert_eq!(ints(&cnt2), vec![3]);
}

#[test]
fn view_with_column_aliases_and_explicit_list() {
    let (io, conn) = setup_products();
    // Column AS aliases in the body.
    exec(
        &conn,
        "CREATE VIEW aliased AS SELECT id AS pid, price*2 AS dbl FROM products",
    );
    let rows = run_query(
        &io,
        &conn,
        "SELECT pid, dbl FROM aliased ORDER BY pid LIMIT 1",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1), Cell::Int(20)]]);

    // Explicit column-list form CREATE VIEW v(a,b) AS ...
    exec(
        &conn,
        "CREATE VIEW named(a, b) AS SELECT id, name FROM products",
    );
    let rows = run_query(&io, &conn, "SELECT a, b FROM named ORDER BY a LIMIT 1");
    assert_eq!(
        rows,
        vec![vec![Cell::Int(1), Cell::Text("apple".to_string())]]
    );
    // pragma table_info reports the explicit names.
    let info = run_query(&io, &conn, "PRAGMA table_info(named)");
    let names: Vec<Cell> = info.iter().map(|r| r[1].clone()).collect();
    assert_eq!(
        names,
        vec![Cell::Text("a".to_string()), Cell::Text("b".to_string())]
    );
}

#[test]
fn view_with_quoted_alias_containing_space() {
    // A body column named by a double-quoted alias with a space must be stored
    // (and reported by PRAGMA table_info) dequoted, exactly like SQLite -- so a
    // later `SELECT "col one" FROM v` resolves it. Regression for a bug where the
    // quotes were kept literally in the derived column name.
    let (io, conn) = setup_products();
    exec(
        &conn,
        "CREATE VIEW spaced AS SELECT id AS \"col one\", price*2 AS \"Dbl Val\" FROM products",
    );

    // The quoted alias resolves through the view.
    let rows = run_query(
        &io,
        &conn,
        "SELECT \"col one\", \"Dbl Val\" FROM spaced ORDER BY \"col one\" LIMIT 1",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1), Cell::Int(20)]]);

    // PRAGMA table_info reports the names WITHOUT quotes, case preserved.
    let info = run_query(&io, &conn, "PRAGMA table_info(spaced)");
    let names: Vec<Cell> = info.iter().map(|r| r[1].clone()).collect();
    assert_eq!(
        names,
        vec![
            Cell::Text("col one".to_string()),
            Cell::Text("Dbl Val".to_string())
        ]
    );

    // The same construct in a plain FROM-clause derived table resolves too.
    let rows = run_query(
        &io,
        &conn,
        "SELECT \"c d\" FROM (SELECT id AS \"c d\" FROM products) ORDER BY \"c d\" LIMIT 1",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1)]]);
}

#[test]
fn view_over_multiway_left_join_repeated_aliases() {
    let (io, conn) = new_conn();
    exec(
        &conn,
        "CREATE TABLE t (id INTEGER, a INTEGER, b INTEGER, c INTEGER)",
    );
    exec(
        &conn,
        "INSERT INTO t VALUES (1, 10, 20, 30), (2, 40, 50, 99)",
    );
    exec(&conn, "CREATE TABLE lut (k INTEGER, v TEXT)");
    exec(
        &conn,
        "INSERT INTO lut VALUES (10,'ten'),(20,'twenty'),(30,'thirty'),(40,'forty'),(50,'fifty')",
    );
    // Same table joined three times under different aliases (mirrors the real
    // driver's param1..param7 pattern).
    exec(
        &conn,
        "CREATE VIEW joined AS \
         SELECT t.id AS id, p1.v AS av, p2.v AS bv, p3.v AS cv \
         FROM t \
         LEFT JOIN lut p1 ON p1.k = t.a \
         LEFT JOIN lut p2 ON p2.k = t.b \
         LEFT JOIN lut p3 ON p3.k = t.c",
    );
    let rows = run_query(&io, &conn, "SELECT id, av, bv, cv FROM joined ORDER BY id");
    assert_eq!(
        rows,
        vec![
            vec![
                Cell::Int(1),
                Cell::Text("ten".to_string()),
                Cell::Text("twenty".to_string()),
                Cell::Text("thirty".to_string()),
            ],
            vec![
                Cell::Int(2),
                Cell::Text("forty".to_string()),
                Cell::Text("fifty".to_string()),
                Cell::Null, // c=99 has no match -> NULL from the LEFT JOIN
            ],
        ]
    );
}

#[test]
fn view_with_union_all_body() {
    let (io, conn) = setup_products();
    exec(
        &conn,
        "CREATE VIEW combo AS \
         SELECT id, 'p' AS src FROM products WHERE cat = 1 \
         UNION ALL \
         SELECT id, 'c' AS src FROM cats",
    );
    let rows = run_query(&io, &conn, "SELECT id, src FROM combo ORDER BY src, id");
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Text("c".to_string())],
            vec![Cell::Int(2), Cell::Text("c".to_string())],
            vec![Cell::Int(3), Cell::Text("c".to_string())],
            vec![Cell::Int(1), Cell::Text("p".to_string())],
            vec![Cell::Int(2), Cell::Text("p".to_string())],
        ]
    );
    // count(*) and a JOIN against the compound view both work.
    let cnt = run_query(&io, &conn, "SELECT count(*) FROM combo");
    assert_eq!(ints(&cnt), vec![5]);
    let joined = run_query(
        &io,
        &conn,
        "SELECT c.id FROM combo c JOIN cats ct ON ct.id = c.id WHERE c.src='c' ORDER BY c.id",
    );
    assert_eq!(ints(&joined), vec![1, 2, 3]);
}

#[test]
fn view_referencing_view_nested() {
    let (io, conn) = setup_products();
    exec(
        &conn,
        "CREATE VIEW l1 AS SELECT id, price FROM products WHERE price >= 20",
    );
    exec(
        &conn,
        "CREATE VIEW l2 AS SELECT id FROM l1 WHERE price <= 40",
    );
    exec(&conn, "CREATE VIEW l3 AS SELECT id FROM l2 WHERE id <> 3");
    let rows = run_query(&io, &conn, "SELECT id FROM l3 ORDER BY id");
    assert_eq!(ints(&rows), vec![2, 4]);
}

#[test]
fn drop_view_and_if_exists() {
    let (io, conn) = setup_products();
    exec(&conn, "CREATE VIEW v AS SELECT id FROM products");
    assert_eq!(
        ints(&run_query(&io, &conn, "SELECT count(*) FROM v")),
        vec![5]
    );
    exec(&conn, "DROP VIEW v");
    // Gone from sqlite_schema and no longer resolvable.
    let sch = run_query(
        &io,
        &conn,
        "SELECT count(*) FROM sqlite_schema WHERE type='view'",
    );
    assert_eq!(ints(&sch), vec![0]);
    let err = prepare_err(&conn, "SELECT * FROM v");
    assert!(err.to_lowercase().contains("v"), "unexpected error: {err}");
    // DROP VIEW IF EXISTS on a missing view is a no-op.
    exec(&conn, "DROP VIEW IF EXISTS v");
    // DROP VIEW on a missing view without IF EXISTS errors.
    let err = prepare_err(&conn, "DROP VIEW nope");
    assert!(err.contains("no such view"), "unexpected error: {err}");
}

#[test]
fn create_view_duplicate_name_errors() {
    let (_io, conn) = setup_products();
    exec(&conn, "CREATE VIEW v AS SELECT id FROM products");
    let err = prepare_err(&conn, "CREATE VIEW v AS SELECT id FROM products");
    assert!(err.contains("already exists"), "unexpected error: {err}");
    // IF NOT EXISTS suppresses the error.
    exec(
        &conn,
        "CREATE VIEW IF NOT EXISTS v AS SELECT id FROM products",
    );
    // A view named like an existing table also conflicts.
    let err = prepare_err(&conn, "CREATE VIEW products AS SELECT 1");
    assert!(err.contains("already exists"), "unexpected error: {err}");
}

#[test]
fn drop_table_on_view_and_drop_view_on_table_error() {
    let (_io, conn) = setup_products();
    exec(&conn, "CREATE VIEW v AS SELECT id FROM products");
    let err = prepare_err(&conn, "DROP TABLE v");
    assert!(
        err.contains("use DROP VIEW to delete view"),
        "unexpected error: {err}"
    );
    let err = prepare_err(&conn, "DROP VIEW products");
    assert!(
        err.contains("use DROP TABLE to delete table"),
        "unexpected error: {err}"
    );
}

#[test]
fn writes_through_view_error() {
    let (_io, conn) = setup_products();
    exec(&conn, "CREATE VIEW v AS SELECT id, name FROM products");
    for sql in [
        "INSERT INTO v (id, name) VALUES (99, 'x')",
        "UPDATE v SET name = 'y' WHERE id = 1",
        "DELETE FROM v WHERE id = 1",
    ] {
        let err = prepare_err(&conn, sql);
        assert!(
            err.contains("cannot modify") && err.contains("because it is a view"),
            "unexpected error for {sql}: {err}"
        );
    }
}

#[test]
fn cyclic_views_created_at_runtime_do_not_hang() {
    let (_io, conn) = setup_products();
    // Create a self-reference cycle via a rename-free two-step: a -> b, then
    // recreate b -> a. Since a already references (the future) b, planning the
    // cycle must hit the depth guard, not stack-overflow.
    exec(&conn, "CREATE VIEW a AS SELECT id FROM products");
    exec(&conn, "CREATE VIEW b AS SELECT id FROM a");
    exec(&conn, "DROP VIEW a");
    exec(&conn, "CREATE VIEW a AS SELECT id FROM b");
    // Now a -> b -> a is a cycle. Querying must fail cleanly (depth guard),
    // not hang or crash.
    let err = prepare_err(&conn, "SELECT * FROM a");
    assert!(
        err.contains("view nesting") || err.contains("circular"),
        "unexpected error: {err}"
    );
}

#[test]
fn view_persists_across_reopen() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("oxisql_view_test_{}.db", std::process::id()));
    let path_str = path.to_str().expect("utf-8 temp path").to_string();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{path_str}-wal"));

    // Real file-backed I/O (MemoryIO would not persist across two opens).
    {
        let io: Arc<dyn IO> = Arc::new(limbo_core::SyscallIO::new().expect("syscall io"));
        let db = Database::open_file(io.clone(), &path_str, false).expect("open db file");
        let conn = db.connect().expect("connect");
        exec(&conn, "CREATE TABLE t (id INTEGER, v INTEGER)");
        exec(&conn, "INSERT INTO t VALUES (1,10),(2,20),(3,30)");
        exec(&conn, "CREATE VIEW big AS SELECT id FROM t WHERE v >= 20");
        exec(&conn, "CREATE VIEW big2 AS SELECT id FROM big WHERE id = 3");
        let rows = run_query(&io, &conn, "SELECT id FROM big2");
        assert_eq!(ints(&rows), vec![3]);
        conn.close().expect("close");
    }

    {
        let io: Arc<dyn IO> = Arc::new(limbo_core::SyscallIO::new().expect("syscall io"));
        let db = Database::open_file(io.clone(), &path_str, false).expect("reopen db file");
        let conn = db.connect().expect("reconnect");
        // The view (and the nested view) must be repopulated from sqlite_schema.
        let rows = run_query(&io, &conn, "SELECT id FROM big ORDER BY id");
        assert_eq!(ints(&rows), vec![2, 3]);
        let rows = run_query(&io, &conn, "SELECT id FROM big2");
        assert_eq!(ints(&rows), vec![3]);
        let sch = run_query(
            &io,
            &conn,
            "SELECT count(*) FROM sqlite_schema WHERE type='view'",
        );
        assert_eq!(ints(&sch), vec![2]);
        conn.close().expect("close");
    }

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(format!("{path_str}-wal"));
}
