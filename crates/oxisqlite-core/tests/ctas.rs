//! Integration tests for `CREATE TABLE ... AS SELECT` (CTAS).

use limbo_core::{Database, MemoryIO, StepResult, Value};
use std::sync::Arc;

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

fn exec(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Result<(), limbo_core::LimboError> {
    let mut stmt = conn.prepare(sql)?;
    loop {
        match stmt.step()? {
            StepResult::Done => return Ok(()),
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(limbo_core::LimboError::Busy),
        }
    }
}

/// Run a query and collect every row's values as a `Vec<Vec<Value>>`, in
/// result order. Useful for asserting on a whole result set at once.
fn query_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<Vec<Value>> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare `{sql}`: {e:?}"));
    let mut rows = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step `{sql}`: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row");
                let n = stmt.num_columns();
                rows.push((0..n).map(|i| row.get_value(i).clone()).collect());
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => return rows,
            StepResult::Interrupt => panic!("interrupted running `{sql}`"),
        }
    }
}

fn count_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    table: &str,
) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    match query_rows(io, conn, &sql).into_iter().next() {
        Some(row) => match &row[0] {
            Value::Integer(i) => *i,
            other => panic!("expected integer count, got {other:?}"),
        },
        None => panic!("count query `{sql}` returned no row"),
    }
}

/// The persisted DDL text for `table_name`, read back from `sqlite_schema`.
fn schema_sql(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    table_name: &str,
) -> String {
    let sql =
        format!("SELECT sql FROM sqlite_schema WHERE name = '{table_name}' AND type = 'table'");
    let rows = query_rows(io, conn, &sql);
    assert_eq!(
        rows.len(),
        1,
        "expected exactly one sqlite_schema row for {table_name}"
    );
    match &rows[0][0] {
        Value::Text(t) => t.as_str().to_string(),
        other => panic!("expected text sql column, got {other:?}"),
    }
}

#[test]
fn ctas_basic_columns_and_rows() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t1 (x INTEGER, y INTEGER)").expect("create t1");
    exec(
        &io,
        &conn,
        "INSERT INTO t1 VALUES (1, 10), (2, 20), (3, 30)",
    )
    .expect("seed t1");

    exec(
        &io,
        &conn,
        "CREATE TABLE t2 AS SELECT x, y + 1 AS z FROM t1 WHERE x > 1",
    )
    .expect("CREATE TABLE AS SELECT");

    // Column names: bare `x` keeps its name; `y + 1 AS z` keeps the alias.
    let sql = schema_sql(&io, &conn, "t2");
    assert!(
        sql.contains('x'),
        "persisted schema must contain column x: {sql}"
    );
    assert!(
        sql.contains('z'),
        "persisted schema must contain column z: {sql}"
    );
    // A bare column reference must carry over the source column's declared
    // type (INTEGER); the computed `y + 1` column has no declared type.
    assert!(
        sql.to_uppercase().contains("INTEGER"),
        "column x must carry over its source INTEGER type: {sql}"
    );

    assert_eq!(
        count_rows(&io, &conn, "t2"),
        2,
        "WHERE x > 1 keeps 2 of 3 rows"
    );

    let mut rows = query_rows(&io, &conn, "SELECT x, z FROM t2 ORDER BY x");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2), Value::Integer(21)],
            vec![Value::Integer(3), Value::Integer(31)],
        ],
        "CTAS rows must match SELECT x, y+1 FROM t1 WHERE x > 1"
    );
    rows.clear();
}

#[test]
fn ctas_aggregate_select() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE sales (category TEXT, amount INTEGER)",
    )
    .expect("create sales");
    exec(
        &io,
        &conn,
        "INSERT INTO sales VALUES ('a', 10), ('a', 5), ('b', 100)",
    )
    .expect("seed sales");

    exec(
        &io,
        &conn,
        "CREATE TABLE totals AS SELECT category, SUM(amount) AS total FROM sales GROUP BY category",
    )
    .expect("CREATE TABLE AS SELECT with aggregate");

    assert_eq!(
        count_rows(&io, &conn, "totals"),
        2,
        "one row per distinct category"
    );
    let rows = query_rows(
        &io,
        &conn,
        "SELECT category, total FROM totals ORDER BY category",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::build_text("a"), Value::Integer(15)],
            vec![Value::build_text("b"), Value::Integer(100)],
        ],
        "aggregate CTAS must persist the grouped sums"
    );
}

#[test]
fn ctas_join_select() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
    )
    .expect("create users");
    exec(
        &io,
        &conn,
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, amount INTEGER)",
    )
    .expect("create orders");
    exec(
        &io,
        &conn,
        "INSERT INTO users VALUES (1, 'alice'), (2, 'bob')",
    )
    .expect("seed users");
    exec(
        &io,
        &conn,
        "INSERT INTO orders VALUES (1, 1, 50), (2, 2, 75), (3, 1, 25)",
    )
    .expect("seed orders");

    exec(
        &io,
        &conn,
        "CREATE TABLE user_orders AS \
         SELECT users.name AS name, orders.amount AS amount \
         FROM users JOIN orders ON users.id = orders.user_id",
    )
    .expect("CREATE TABLE AS SELECT with JOIN");

    assert_eq!(count_rows(&io, &conn, "user_orders"), 3);
    let rows = query_rows(
        &io,
        &conn,
        "SELECT name, amount FROM user_orders ORDER BY amount",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::build_text("alice"), Value::Integer(25)],
            vec![Value::build_text("alice"), Value::Integer(50)],
            vec![Value::build_text("bob"), Value::Integer(75)],
        ],
        "joined CTAS rows must match the JOIN's result set"
    );
}

#[test]
fn ctas_empty_result_set() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE src (x INTEGER)").expect("create src");
    exec(&io, &conn, "INSERT INTO src VALUES (1), (2), (3)").expect("seed src");

    exec(
        &io,
        &conn,
        "CREATE TABLE empty_dst AS SELECT x FROM src WHERE x > 100",
    )
    .expect("CREATE TABLE AS SELECT with an empty result set must still create the table");

    // The table must exist (with the right schema) even though it's empty.
    let sql = schema_sql(&io, &conn, "empty_dst");
    assert!(
        sql.contains('x'),
        "empty CTAS table must still have column x: {sql}"
    );
    assert_eq!(
        count_rows(&io, &conn, "empty_dst"),
        0,
        "no rows matched the WHERE clause"
    );

    // The table must also accept ordinary inserts afterward, proving it's a
    // fully-formed, ordinary table rather than some degenerate placeholder.
    exec(&io, &conn, "INSERT INTO empty_dst VALUES (42)").expect("insert into empty CTAS table");
    assert_eq!(count_rows(&io, &conn, "empty_dst"), 1);
}

#[test]
fn ctas_duplicate_column_name_errors() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t1 (x INTEGER)").expect("create t1");
    let result = exec(&io, &conn, "CREATE TABLE t2 AS SELECT x, x FROM t1");
    assert!(
        result.is_err(),
        "CTAS with duplicate result column names must be rejected, not silently create an \
         ambiguous table"
    );
}

fn open_file_db(path: &std::path::Path) -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(limbo_core::SyscallIO::new().expect("new io"));
    let db = Database::open_file(io.clone(), path.to_str().expect("utf8 path"), false)
        .expect("open file db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

fn temp_db_path(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "oxisqlite_ctas_test_{}_{}_{}.db",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time went backwards")
            .as_nanos()
    ))
}

fn cleanup_db_files(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{}-wal", path.display()));
    let _ = std::fs::remove_file(format!("{}-shm", path.display()));
}

#[test]
fn ctas_persists_after_reopen() {
    let path = temp_db_path("persists_after_reopen");
    cleanup_db_files(&path);

    {
        let (io, conn) = open_file_db(&path);
        exec(&io, &conn, "CREATE TABLE t1 (x INTEGER, y TEXT)").expect("create t1");
        exec(&io, &conn, "INSERT INTO t1 VALUES (1, 'a'), (2, 'b')").expect("seed t1");
        exec(&io, &conn, "CREATE TABLE t2 AS SELECT x, y FROM t1").expect("CTAS");
        conn.close().expect("clean close");
    }

    // Reopen: `t2`'s schema and data must both survive purely from what was
    // persisted to disk (the CREATE TABLE AS SELECT statement itself never
    // runs again).
    let (io, conn) = open_file_db(&path);
    let sql = schema_sql(&io, &conn, "t2");
    assert!(
        sql.contains('x') && sql.contains('y'),
        "t2 schema must survive reopen: {sql}"
    );
    assert_eq!(
        count_rows(&io, &conn, "t2"),
        2,
        "t2 rows must survive reopen"
    );
    let rows = query_rows(&io, &conn, "SELECT x, y FROM t2 ORDER BY x");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::build_text("a")],
            vec![Value::Integer(2), Value::build_text("b")],
        ]
    );

    cleanup_db_files(&path);
}
