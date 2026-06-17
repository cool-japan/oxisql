//! Integration tests for `ANALYZE` and the `sqlite_stat1` table.
//!
//! Verifies that `ANALYZE` creates `sqlite_stat1`, writes the expected
//! `(tbl, idx, stat)` rows (including the `N a1 … ak` index statistics),
//! skips empty tables, replaces rather than duplicates on re-analyze, and
//! honours table/index targeting.

use std::sync::Arc;

use limbo_core::{Database, MemoryIO, StepResult, Value};

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

/// Read every `(tbl, idx, stat)` row from `sqlite_stat1`, ordered for
/// deterministic comparison.
fn query_stat1(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
) -> Vec<(String, Option<String>, String)> {
    let mut stmt = conn
        .prepare("SELECT tbl, idx, stat FROM sqlite_stat1 ORDER BY tbl, idx")
        .expect("prepare select sqlite_stat1");
    let mut rows = Vec::new();
    loop {
        match stmt.step().expect("step sqlite_stat1") {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                let tbl = row
                    .get_value(0)
                    .to_text()
                    .expect("tbl column is text")
                    .to_string();
                let idx = row.get_value(1).to_text().map(|s| s.to_string());
                let stat = row
                    .get_value(2)
                    .to_text()
                    .expect("stat column is text")
                    .to_string();
                rows.push((tbl, idx, stat));
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

/// Run a single-column integer query and collect the results, mirroring
/// `query_stat1`'s row-stepping.
fn query_ints(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Vec<i64> {
    let mut stmt = conn.prepare(sql).expect("prepare query_ints");
    let mut rows = Vec::new();
    loop {
        match stmt.step().expect("step query_ints") {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                match row.get_value(0) {
                    Value::Integer(i) => rows.push(*i),
                    other => panic!("expected integer column, got {other:?}"),
                }
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

#[test]
fn analyze_no_index_row_count() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    for i in 0..5 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert");
    }

    exec(&io, &conn, "ANALYZE").expect("analyze");

    let rows = query_stat1(&io, &conn);
    assert_eq!(rows, vec![("t".to_string(), None, "5".to_string())]);
}

#[test]
fn analyze_empty_table_no_row() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");

    exec(&io, &conn, "ANALYZE").expect("analyze");

    // sqlite_stat1 exists but the empty table contributes no row.
    let rows = query_stat1(&io, &conn);
    assert!(rows.is_empty(), "expected no stat rows, got {rows:?}");
}

#[test]
fn analyze_reanalyze_replaces() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    for i in 0..5 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert");
    }
    exec(&io, &conn, "ANALYZE").expect("first analyze");
    assert_eq!(
        query_stat1(&io, &conn),
        vec![("t".to_string(), None, "5".to_string())]
    );

    for i in 5..8 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert");
    }
    exec(&io, &conn, "ANALYZE").expect("second analyze");

    // The row must be replaced, not duplicated.
    let rows = query_stat1(&io, &conn);
    assert_eq!(rows, vec![("t".to_string(), None, "8".to_string())]);
}

#[test]
fn analyze_named_table_target() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    exec(&io, &conn, "CREATE TABLE u(y)").expect("create u");
    for i in 0..3 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert t");
    }
    for i in 0..2 {
        exec(&io, &conn, &format!("INSERT INTO u VALUES ({i})")).expect("insert u");
    }

    exec(&io, &conn, "ANALYZE t").expect("analyze t");
    exec(&io, &conn, "ANALYZE u").expect("analyze u");

    let rows = query_stat1(&io, &conn);
    assert_eq!(
        rows,
        vec![
            ("t".to_string(), None, "3".to_string()),
            ("u".to_string(), None, "2".to_string()),
        ]
    );
}

#[test]
fn analyze_unknown_table_error() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");

    let result = exec(&io, &conn, "ANALYZE does_not_exist");
    assert!(result.is_err(), "ANALYZE of unknown object must error");
}

#[test]
fn analyze_stats_loaded_query_correct() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t(x)").expect("create t");
    for i in 0..7 {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i})")).expect("insert");
    }

    exec(&io, &conn, "ANALYZE").expect("analyze");

    // Writer + reload survived: sqlite_stat1 holds the row-count row.
    assert_eq!(
        query_stat1(&io, &conn),
        vec![("t".to_string(), None, "7".to_string())]
    );

    // Queries planned with statistics loaded must still return correct results.
    assert_eq!(query_ints(&io, &conn, "SELECT count(*) FROM t"), vec![7]);
    assert_eq!(
        query_ints(&io, &conn, "SELECT x FROM t WHERE x = 3"),
        vec![3]
    );
}

#[cfg(feature = "index_experimental")]
mod index_tests {
    use super::{exec, new_mem_db, query_stat1};

    #[test]
    fn analyze_index_multi_col_distinct_prefix() {
        let (io, conn) = new_mem_db();
        exec(&io, &conn, "CREATE TABLE t(a, b)").expect("create t");
        exec(&io, &conn, "CREATE INDEX idx_ab ON t(a, b)").expect("create index");
        for (a, b) in [(1, 1), (1, 2), (1, 3), (2, 1), (2, 2), (2, 3)] {
            exec(&io, &conn, &format!("INSERT INTO t VALUES ({a}, {b})")).expect("insert");
        }

        exec(&io, &conn, "ANALYZE").expect("analyze");

        let rows = query_stat1(&io, &conn);
        // Table row: 6 entries.
        assert!(
            rows.contains(&("t".to_string(), None, "6".to_string())),
            "missing table row, got {rows:?}"
        );
        // Index row: N=6, 2 distinct values of `a` (avg 3), 6 distinct (a,b)
        // prefixes (avg 1) => "6 3 1".
        assert!(
            rows.contains(&(
                "t".to_string(),
                Some("idx_ab".to_string()),
                "6 3 1".to_string()
            )),
            "missing/incorrect index row, got {rows:?}"
        );
    }

    #[test]
    fn analyze_nulleq_grouping() {
        let (io, conn) = new_mem_db();
        exec(&io, &conn, "CREATE TABLE t(a)").expect("create t");
        exec(&io, &conn, "CREATE INDEX idx_a ON t(a)").expect("create index");
        exec(&io, &conn, "INSERT INTO t VALUES (NULL)").expect("insert null 1");
        exec(&io, &conn, "INSERT INTO t VALUES (NULL)").expect("insert null 2");
        exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("insert 1");

        exec(&io, &conn, "ANALYZE").expect("analyze");

        let rows = query_stat1(&io, &conn);
        // Consecutive NULLs collapse into one group, so there are 2 distinct
        // values ({NULL}, {1}) over 3 rows => avg ceil(3/2) = 2 => "3 2".
        assert!(
            rows.contains(&(
                "t".to_string(),
                Some("idx_a".to_string()),
                "3 2".to_string()
            )),
            "NULLs must group into a single distinct value, got {rows:?}"
        );
    }
}
