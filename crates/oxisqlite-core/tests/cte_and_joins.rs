//! Integration tests for three `translate/planner.rs` fixes:
//! - a CTE referenced more than once in the same query (e.g. a self-join) must resolve each
//!   reference independently, instead of the first reference consuming the only parsed copy,
//! - `FROM (t1 JOIN t2 ON ...)` (a parenthesized join used as a grouping construct, not a new
//!   scope) must parse and produce the same rows as the unparenthesized join,
//! - NATURAL JOIN's common-column detection (rewritten from a nested loop to a `HashSet`
//!   precomputation for performance) must still produce correct results.
//!
//! These exercise real bytecode execution end to end against an in-memory database, asserting
//! concrete result values.

use std::sync::Arc;

use limbo_core::{Connection, Database, MemoryIO, StepResult, Value, IO};

fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory: database");
    let conn = db.connect().expect("connect to :memory: database");
    (io, conn)
}

/// A simple owned cell so result assertions can compare without borrowing the statement's row
/// buffer.
#[derive(Debug, Clone, PartialEq)]
enum Cell {
    Int(i64),
    Text(String),
}

fn to_cell(v: &Value) -> Cell {
    match v {
        Value::Integer(i) => Cell::Int(*i),
        Value::Text(t) => Cell::Text(t.as_str().to_string()),
        other => panic!("unexpected value in test data: {other:?}"),
    }
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Run a query to completion, returning all rows as owned cells.
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

/// Run a query expected to fail at prepare time, returning the error's Display text.
fn prepare_err(conn: &Arc<Connection>, sql: &str) -> String {
    match conn.prepare(sql) {
        Ok(_) => panic!("expected {sql:?} to fail to prepare, but it succeeded"),
        Err(e) => e.to_string(),
    }
}

// -----------------------------------------------------------------------
// CTE referenced more than once (self-join, independent per-reference filters)
// -----------------------------------------------------------------------

fn setup_cte_base() -> (Arc<dyn IO>, Arc<Connection>) {
    let (io, conn) = new_conn();
    exec(
        &conn,
        "CREATE TABLE base (id INTEGER, grp INTEGER, val INTEGER)",
    );
    exec(
        &conn,
        "INSERT INTO base VALUES (1, 1, 100), (2, 1, 200), (3, 2, 300), (4, 2, 400)",
    );
    (io, conn)
}

#[test]
fn cte_self_join_with_different_predicates_per_reference() {
    let (io, conn) = setup_cte_base();
    // `c` is referenced twice (aliased `a` and `b`), self-joined on `grp`, with a *different*
    // WHERE filter applied to each reference. If the two references were cross-contaminated
    // (e.g. resolved to the same underlying table id after the fix, or the second reference
    // failed to resolve at all before the fix), this would either fail to prepare, or return
    // rows that violate one side's filter, or miss/duplicate rows.
    let rows = run_query(
        &io,
        &conn,
        "WITH c AS (SELECT id, grp, val FROM base) \
         SELECT a.id, a.val, b.id, b.val \
         FROM c a JOIN c b ON a.grp = b.grp \
         WHERE a.val < 250 AND b.val > 150 \
         ORDER BY a.id, b.id",
    );
    assert_eq!(
        rows,
        vec![
            // a=(1,grp1,100): only b=(2,grp1,200) satisfies b.val>150 within grp1.
            vec![Cell::Int(1), Cell::Int(100), Cell::Int(2), Cell::Int(200)],
            // a=(2,grp1,200): b=(2,grp1,200) satisfies both a.val<250 (as `a`) and b.val>150.
            vec![Cell::Int(2), Cell::Int(200), Cell::Int(2), Cell::Int(200)],
        ],
        "each self-join reference must only see rows matching *its own* WHERE filter"
    );
}

#[test]
fn cte_referenced_three_times_resolves_independently() {
    let (io, conn) = setup_cte_base();
    // A third reference stresses the renumbering allocator beyond a simple pairwise self-join.
    let rows = run_query(
        &io,
        &conn,
        "WITH c AS (SELECT id, grp, val FROM base) \
         SELECT a.id, b.id, d.id \
         FROM c a JOIN c b ON a.grp = b.grp JOIN c d ON b.grp = d.grp \
         WHERE a.id = 1 AND b.id = 2 AND d.grp = 1 \
         ORDER BY d.id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Int(2), Cell::Int(1)],
            vec![Cell::Int(1), Cell::Int(2), Cell::Int(2)]
        ],
    );
}

#[test]
fn cte_single_reference_still_works_and_respects_alias() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE t (id INTEGER, v INTEGER)");
    exec(&conn, "INSERT INTO t VALUES (1, 10), (2, 20)");
    // A plain (non-multi-referenced) CTE, aliased, must still resolve via the alias.
    let rows = run_query(
        &io,
        &conn,
        "WITH c AS (SELECT id, v FROM t) SELECT x.id, x.v FROM c x ORDER BY x.id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Int(10)],
            vec![Cell::Int(2), Cell::Int(20)],
        ]
    );
}

// -----------------------------------------------------------------------
// `FROM (t1 JOIN t2 ON ...)` -- parenthesized join as a grouping construct
// -----------------------------------------------------------------------

fn setup_join_tables() -> (Arc<dyn IO>, Arc<Connection>) {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE t1 (id INTEGER, name TEXT)");
    exec(
        &conn,
        "CREATE TABLE t2 (id INTEGER, t1_id INTEGER, label TEXT)",
    );
    exec(&conn, "INSERT INTO t1 VALUES (1, 'alice'), (2, 'bob')");
    exec(
        &conn,
        "INSERT INTO t2 VALUES (10, 1, 'x'), (11, 2, 'y'), (12, 1, 'z')",
    );
    (io, conn)
}

#[test]
fn parenthesized_join_matches_unparenthesized_join() {
    let (io, conn) = setup_join_tables();
    let expected = run_query(
        &io,
        &conn,
        "SELECT t1.name, t2.label FROM t1 JOIN t2 ON t1.id = t2.t1_id ORDER BY t2.id",
    );
    let actual = run_query(
        &io,
        &conn,
        "SELECT t1.name, t2.label FROM (t1 JOIN t2 ON t1.id = t2.t1_id) ORDER BY t2.id",
    );
    assert_eq!(
        actual, expected,
        "a parenthesized join must be a pure grouping construct producing the same rows"
    );
    assert_eq!(
        actual,
        vec![
            vec![Cell::Text("alice".to_string()), Cell::Text("x".to_string())],
            vec![Cell::Text("bob".to_string()), Cell::Text("y".to_string())],
            vec![Cell::Text("alice".to_string()), Cell::Text("z".to_string())],
        ]
    );
}

#[test]
fn parenthesized_join_merges_into_parent_scope_not_a_new_subquery_scope() {
    let (io, conn) = setup_join_tables();
    exec(&conn, "CREATE TABLE t3 (t2_id INTEGER, note TEXT)");
    exec(
        &conn,
        "INSERT INTO t3 VALUES (10, 'n1'), (11, 'n2'), (12, 'n3')",
    );
    // t3 (outside the parens) must be able to join against t2 (inside the parens) directly --
    // this only works if the parenthesized join's tables land in the *same* table_references /
    // join order as the surrounding query, not a separate subquery scope.
    let rows = run_query(
        &io,
        &conn,
        "SELECT t1.name, t3.note \
         FROM (t1 JOIN t2 ON t1.id = t2.t1_id) JOIN t3 ON t2.id = t3.t2_id \
         ORDER BY t3.note",
    );
    assert_eq!(
        rows,
        vec![
            vec![
                Cell::Text("alice".to_string()),
                Cell::Text("n1".to_string())
            ],
            vec![Cell::Text("bob".to_string()), Cell::Text("n2".to_string())],
            vec![
                Cell::Text("alice".to_string()),
                Cell::Text("n3".to_string())
            ],
        ]
    );
}

#[test]
fn parenthesized_join_rejects_alias() {
    let (_io, conn) = setup_join_tables();
    // A single alias has no sensible target on a multi-table grouping construct; this must be a
    // clear parse error rather than silently ignored or mis-resolved.
    let err = prepare_err(
        &conn,
        "SELECT * FROM (t1 JOIN t2 ON t1.id = t2.t1_id) AS grouped",
    );
    assert!(
        err.to_lowercase().contains("alias"),
        "expected an alias-related parse error, got: {err}"
    );
}

// -----------------------------------------------------------------------
// NATURAL JOIN (nested-loop -> HashSet rewrite): result-set equality
// -----------------------------------------------------------------------

#[test]
fn natural_join_two_tables_common_column() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE t1 (id INTEGER, name TEXT)");
    exec(&conn, "CREATE TABLE t2 (id INTEGER, extra TEXT)");
    exec(&conn, "INSERT INTO t1 VALUES (1, 'a'), (2, 'b'), (3, 'c')");
    exec(&conn, "INSERT INTO t2 VALUES (1, 'x'), (2, 'y'), (4, 'z')");

    // `ORDER BY t1.id` (qualified) rather than bare `id`: this engine's bare-identifier
    // resolution for ORDER BY does not yet special-case USING/NATURAL-joined columns the way
    // `SELECT *` does (see `plan::select_star`'s `join_info.using` dedup), so a bare `id` here
    // is flagged ambiguous across t1/t2 -- a separate, pre-existing limitation unrelated to the
    // NATURAL JOIN common-column-detection rewrite under test.
    let rows = run_query(
        &io,
        &conn,
        "SELECT * FROM t1 NATURAL JOIN t2 ORDER BY t1.id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Text("a".to_string()), Cell::Text("x".to_string())],
            vec![Cell::Int(2), Cell::Text("b".to_string()), Cell::Text("y".to_string())],
        ],
        "NATURAL JOIN should match rows on the shared `id` column and drop the unmatched t1.id=3 / t2.id=4"
    );
}

#[test]
fn natural_join_three_tables_multiple_common_columns() {
    let (io, conn) = new_conn();
    // t1 and t2 share `id`; t2 and t3 share `id` AND `grp`, exercising the left-column-set
    // precomputation across *multiple* already-joined left tables at once.
    exec(&conn, "CREATE TABLE t1 (id INTEGER, grp INTEGER, a TEXT)");
    exec(&conn, "CREATE TABLE t2 (id INTEGER, grp INTEGER, b TEXT)");
    exec(&conn, "CREATE TABLE t3 (id INTEGER, grp INTEGER, c TEXT)");
    exec(&conn, "INSERT INTO t1 VALUES (1, 9, 'a1'), (2, 9, 'a2')");
    exec(&conn, "INSERT INTO t2 VALUES (1, 9, 'b1'), (2, 9, 'b2')");
    exec(&conn, "INSERT INTO t3 VALUES (1, 9, 'c1'), (3, 9, 'c3')");

    // `t1.id`/`t1.grp` (qualified) for the same reason as the bare-`id` note above: this
    // engine's bare-identifier resolution doesn't dedupe USING/NATURAL-joined columns outside
    // `SELECT *`, so unqualified `id`/`grp` here (shared by all three tables) would be flagged
    // ambiguous independent of the rewrite under test. `a`/`b`/`c` are unique per-table names
    // and need no qualification.
    let rows = run_query(
        &io,
        &conn,
        "SELECT t1.id, t1.grp, a, b, c FROM t1 NATURAL JOIN t2 NATURAL JOIN t3 ORDER BY t1.id",
    );
    assert_eq!(
        rows,
        vec![vec![
            Cell::Int(1),
            Cell::Int(9),
            Cell::Text("a1".to_string()),
            Cell::Text("b1".to_string()),
            Cell::Text("c1".to_string()),
        ]],
        "only id=1 matches across all three tables on both shared columns (id, grp)"
    );
}

#[test]
fn natural_join_no_common_columns_is_a_parse_error() {
    let (_io, conn) = new_conn();
    exec(&conn, "CREATE TABLE t1 (a TEXT)");
    exec(&conn, "CREATE TABLE t2 (b TEXT)");
    let err = prepare_err(&conn, "SELECT * FROM t1 NATURAL JOIN t2");
    assert!(
        err.contains("No columns found to NATURAL join on"),
        "got: {err}"
    );
}
