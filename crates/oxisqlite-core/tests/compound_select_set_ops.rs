//! Integration tests for `translate/compound_select.rs` / `translate/select.rs`'s compound-SELECT
//! support: `INTERSECT`, `EXCEPT`, compound `LIMIT`/`OFFSET`, compound `ORDER BY`, and a `WITH`
//! clause shared across every arm of a compound SELECT.
//!
//! All of these rely on the ephemeral-unique-index machinery UNION already used (see
//! `compound_select::create_compound_dedupe_index`), which requires `index_experimental`.

#![cfg(feature = "index_experimental")]

use std::sync::Arc;

use limbo_core::{Connection, Database, MemoryIO, StepResult, Value, IO};

fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory: database");
    let conn = db.connect().expect("connect to :memory: database");
    (io, conn)
}

/// A simple owned, orderable cell so result assertions can compare without borrowing the
/// statement's row buffer, and so whole result sets can be order-independently sorted when a test
/// is deliberately not exercising ORDER BY.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Cell {
    Null,
    Int(i64),
    Text(String),
}

fn to_cell(v: &Value) -> Cell {
    match v {
        Value::Null => Cell::Null,
        Value::Integer(i) => Cell::Int(*i),
        Value::Text(t) => Cell::Text(t.as_str().to_string()),
        other => panic!("unexpected value in test data: {other:?}"),
    }
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

/// Run a query to completion, returning all rows as owned cells, in the order the engine produced
/// them.
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

/// Run a query and return its rows sorted, for tests that assert *set* equality (no ORDER BY in
/// the query under test, so the engine's natural row order is not part of what's being verified).
fn run_query_sorted(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) -> Vec<Vec<Cell>> {
    let mut rows = run_query(io, conn, sql);
    rows.sort();
    rows
}

fn ints(vals: &[i64]) -> Vec<Vec<Cell>> {
    vals.iter().map(|&n| vec![Cell::Int(n)]).collect()
}

// -----------------------------------------------------------------------
// INTERSECT
// -----------------------------------------------------------------------

#[test]
fn intersect_keeps_only_keys_present_on_both_sides_deduplicated() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    // `2` and `3` are duplicated on one side or the other; each must appear at most once in the
    // INTERSECT result, and `1`/`4` (present on only one side) must not appear at all.
    exec(&conn, "INSERT INTO a VALUES (1), (2), (2), (3)");
    exec(&conn, "INSERT INTO b VALUES (2), (3), (3), (4)");
    let rows = run_query_sorted(&io, &conn, "SELECT x FROM a INTERSECT SELECT x FROM b");
    assert_eq!(rows, ints(&[2, 3]));
}

#[test]
fn intersect_empty_when_no_overlap() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (3), (4)");
    let rows = run_query(&io, &conn, "SELECT x FROM a INTERSECT SELECT x FROM b");
    assert_eq!(rows, Vec::<Vec<Cell>>::new());
}

#[test]
fn intersect_compares_the_whole_row_not_just_one_column() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER, y INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER, y INTEGER)");
    // (1,1) matches fully; (1,2) vs (1,3) share x but differ on y, so must NOT match.
    exec(&conn, "INSERT INTO a VALUES (1, 1), (1, 2)");
    exec(&conn, "INSERT INTO b VALUES (1, 1), (1, 3)");
    let rows = run_query_sorted(
        &io,
        &conn,
        "SELECT x, y FROM a INTERSECT SELECT x, y FROM b",
    );
    assert_eq!(rows, vec![vec![Cell::Int(1), Cell::Int(1)]]);
}

#[test]
fn intersect_treats_null_as_equal_to_null() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (NULL), (1)");
    exec(&conn, "INSERT INTO b VALUES (NULL), (2)");
    // Unlike `NULL = NULL` (which is NULL, not true), SQL's set-operation membership treats NULL
    // as equal to NULL, so `NULL` is common to both sides and survives the intersection; `1` and
    // `2` (each present on only one side) do not.
    let rows = run_query(&io, &conn, "SELECT x FROM a INTERSECT SELECT x FROM b");
    assert_eq!(rows, vec![vec![Cell::Null]]);
}

// -----------------------------------------------------------------------
// EXCEPT
// -----------------------------------------------------------------------

#[test]
fn except_keeps_only_left_keys_absent_from_the_right_deduplicated() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    // `1` and `3` are duplicated on the left; each must appear at most once in the result.
    exec(&conn, "INSERT INTO a VALUES (1), (1), (2), (3), (3)");
    exec(&conn, "INSERT INTO b VALUES (2), (2), (4)");
    let rows = run_query_sorted(&io, &conn, "SELECT x FROM a EXCEPT SELECT x FROM b");
    assert_eq!(rows, ints(&[1, 3]));
}

#[test]
fn except_empty_when_right_side_is_a_superset() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (1), (2), (3)");
    let rows = run_query(&io, &conn, "SELECT x FROM a EXCEPT SELECT x FROM b");
    assert_eq!(rows, Vec::<Vec<Cell>>::new());
}

#[test]
fn except_right_side_rows_absent_from_left_are_harmless() {
    // Regression test for the EXCEPT implementation strategy: `Insn::IdxDelete` errors
    // (`LimboError::Corrupt`) if the given key isn't present in the target index, so subtracting
    // a right-hand row that was never on the left must not blow up -- it just has nothing to do.
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1)");
    exec(&conn, "INSERT INTO b VALUES (99), (100), (101)");
    let rows = run_query(&io, &conn, "SELECT x FROM a EXCEPT SELECT x FROM b");
    assert_eq!(rows, vec![vec![Cell::Int(1)]]);
}

#[test]
fn except_treats_null_as_equal_to_null() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (NULL), (1)");
    exec(&conn, "INSERT INTO b VALUES (NULL)");
    // The left-hand NULL is removed because NULL is (for set-operation purposes) present on the
    // right too; `1` survives since it's absent from the right.
    let rows = run_query(&io, &conn, "SELECT x FROM a EXCEPT SELECT x FROM b");
    assert_eq!(rows, vec![vec![Cell::Int(1)]]);
}

// -----------------------------------------------------------------------
// Left-to-right grouping of mixed compound-operator chains
// -----------------------------------------------------------------------

#[test]
fn mixed_union_except_chain_groups_left_to_right() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (2), (3)");
    exec(&conn, "INSERT INTO c VALUES (1), (3)");
    // a={1,2}, b={2,3}, c={1,3}. SQLite groups compound chains strictly left-to-right (never by
    // any kind of operator precedence), so `A UNION B EXCEPT C` means `(A UNION B) EXCEPT C`:
    //   (A UNION B) EXCEPT C = ({1,2} u {2,3}) - {1,3} = {1,2,3} - {1,3} = {2}
    // These particular sets are chosen so the *other* possible grouping, `A UNION (B EXCEPT C)`,
    // gives a genuinely different (and thus wrong, if that's what got computed instead) answer:
    //   A UNION (B EXCEPT C) = {1,2} u ({2,3} - {1,3}) = {1,2} u {2} = {1,2}
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION SELECT x FROM b EXCEPT SELECT x FROM c",
    );
    assert_eq!(rows, vec![vec![Cell::Int(2)]]);
}

#[test]
fn mixed_intersect_union_chain_groups_left_to_right() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (2), (3)");
    exec(&conn, "INSERT INTO c VALUES (4)");
    // a={1,2}, b={2,3}, c={4}. Left-to-right: `A INTERSECT B UNION C` means
    // `(A INTERSECT B) UNION C`:
    //   (A INTERSECT B) UNION C = {2} u {4} = {2,4}
    // The other possible grouping, `A INTERSECT (B UNION C)`, again differs:
    //   A INTERSECT (B UNION C) = {1,2} n {2,3,4} = {2}
    let rows = run_query_sorted(
        &io,
        &conn,
        "SELECT x FROM a INTERSECT SELECT x FROM b UNION SELECT x FROM c",
    );
    assert_eq!(rows, ints(&[2, 4]));
}

#[test]
fn three_way_intersect_chain_groups_left_to_right() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2), (3)");
    exec(&conn, "INSERT INTO b VALUES (2), (3), (4)");
    exec(&conn, "INSERT INTO c VALUES (3), (4), (5)");
    // INTERSECT happens to be associative, so this mainly exercises that chained (not just
    // pairwise) INTERSECT works: (A n B) n C = {2,3} n {3,4,5} = {3}.
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a INTERSECT SELECT x FROM b INTERSECT SELECT x FROM c",
    );
    assert_eq!(rows, vec![vec![Cell::Int(3)]]);
}

// -----------------------------------------------------------------------
// Regression: plain UNION / UNION ALL chains (unchanged machinery) still work
// -----------------------------------------------------------------------

#[test]
fn union_three_way_chain_still_dedupes_across_every_arm() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (2), (3)");
    exec(&conn, "INSERT INTO c VALUES (3), (4)");
    let rows = run_query_sorted(
        &io,
        &conn,
        "SELECT x FROM a UNION SELECT x FROM b UNION SELECT x FROM c",
    );
    assert_eq!(rows, ints(&[1, 2, 3, 4]));
}

#[test]
fn union_all_then_union_dedupes_only_the_final_combination() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (1)");
    exec(&conn, "INSERT INTO b VALUES (1)");
    exec(&conn, "INSERT INTO c VALUES (1), (2)");
    // (A UNION ALL B) UNION C: A UNION ALL B = {1,1,1} (no dedup), then UNION with C dedupes the
    // *combined* result down to {1,2}.
    let rows = run_query_sorted(
        &io,
        &conn,
        "SELECT x FROM a UNION ALL SELECT x FROM b UNION SELECT x FROM c",
    );
    assert_eq!(rows, ints(&[1, 2]));
}

// -----------------------------------------------------------------------
// Compound LIMIT / OFFSET
// -----------------------------------------------------------------------

#[test]
fn union_all_limit_offset_paginates_the_combined_stream() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (10), (20), (30)");
    exec(&conn, "INSERT INTO b VALUES (40), (50), (60)");
    // UNION ALL streams a's rows then b's rows (no reordering, no dedup): 10,20,30,40,50,60.
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION ALL SELECT x FROM b LIMIT 3 OFFSET 2",
    );
    assert_eq!(rows, ints(&[30, 40, 50]));
}

#[test]
fn union_all_offset_skips_rows_across_an_arm_boundary() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (3), (4)");
    // OFFSET 3 must skip 1,2 (all of `a`) AND the first row of `b` (3), leaving only 4 -- proving
    // the OFFSET countdown is shared across arms, not reset per arm. (`LIMIT -1` = unbounded --
    // SQLite's grammar requires OFFSET to be paired with a LIMIT clause.)
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION ALL SELECT x FROM b LIMIT -1 OFFSET 3",
    );
    assert_eq!(rows, ints(&[4]));
}

#[test]
fn union_dedup_order_by_limit_offset_paginates_the_deduplicated_result() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2), (3)");
    exec(&conn, "INSERT INTO b VALUES (3), (4), (5)");
    // Deduplicated combined set is {1,2,3,4,5}; OFFSET 2 LIMIT 2 -> {3,4}.
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION SELECT x FROM b ORDER BY x LIMIT 2 OFFSET 2",
    );
    assert_eq!(rows, ints(&[3, 4]));
}

#[test]
fn intersect_limit_applies_to_the_intersection_result() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2), (3), (4)");
    exec(&conn, "INSERT INTO b VALUES (2), (3), (4), (5)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a INTERSECT SELECT x FROM b ORDER BY x LIMIT 2",
    );
    assert_eq!(rows, ints(&[2, 3]));
}

#[test]
fn except_offset_applies_to_the_difference_result() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2), (3), (4)");
    exec(&conn, "INSERT INTO b VALUES (3)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a EXCEPT SELECT x FROM b ORDER BY x LIMIT -1 OFFSET 1",
    );
    assert_eq!(rows, ints(&[2, 4]));
}

// -----------------------------------------------------------------------
// Compound ORDER BY
// -----------------------------------------------------------------------

#[test]
fn union_all_order_by_interleaves_rows_from_both_arms() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (5), (9)");
    exec(&conn, "INSERT INTO b VALUES (2), (6), (8)");
    // A naive "sort each arm, then concatenate" implementation would produce 1,5,9,2,6,8. A
    // correct single sort over the combined result must interleave: 1,2,5,6,8,9.
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION ALL SELECT x FROM b ORDER BY x",
    );
    assert_eq!(rows, ints(&[1, 2, 5, 6, 8, 9]));
}

#[test]
fn union_order_by_column_name_descending() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (3)");
    exec(&conn, "INSERT INTO b VALUES (2), (4)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION SELECT x FROM b ORDER BY x DESC",
    );
    assert_eq!(rows, ints(&[4, 3, 2, 1]));
}

#[test]
fn union_order_by_ordinal_position() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER, y TEXT)");
    exec(&conn, "CREATE TABLE b (x INTEGER, y TEXT)");
    exec(&conn, "INSERT INTO a VALUES (1, 'a')");
    exec(&conn, "INSERT INTO b VALUES (2, 'b'), (3, 'c')");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x, y FROM a UNION SELECT x, y FROM b ORDER BY 1 DESC",
    );
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(3), Cell::Text("c".to_string())],
            vec![Cell::Int(2), Cell::Text("b".to_string())],
            vec![Cell::Int(1), Cell::Text("a".to_string())],
        ]
    );
}

#[test]
fn except_order_by_sorts_the_difference_result() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (5), (1), (3)");
    exec(&conn, "INSERT INTO b VALUES (1)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a EXCEPT SELECT x FROM b ORDER BY x",
    );
    assert_eq!(rows, ints(&[3, 5]));
}

#[test]
fn intersect_order_by_sorts_the_intersection_result() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (5), (1), (3), (2)");
    exec(&conn, "INSERT INTO b VALUES (1), (2), (5)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a INTERSECT SELECT x FROM b ORDER BY x DESC",
    );
    assert_eq!(rows, ints(&[5, 2, 1]));
}

// -----------------------------------------------------------------------
// WITH / CTE shared across compound-SELECT arms
// -----------------------------------------------------------------------

#[test]
fn cte_referenced_by_both_union_arms_dedupes_the_overlap() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE base (id INTEGER, val INTEGER)");
    exec(&conn, "INSERT INTO base VALUES (1, 10), (2, 20), (3, 30)");
    let rows = run_query_sorted(
        &io,
        &conn,
        "WITH c AS (SELECT id, val FROM base) \
         SELECT id, val FROM c UNION SELECT id, val FROM c WHERE val > 15",
    );
    // The second arm's rows (id=2,3) are a subset of the first arm's (id=1,2,3); UNION must
    // dedupe them down to exactly base's 3 rows, not return 5.
    assert_eq!(
        rows,
        vec![
            vec![Cell::Int(1), Cell::Int(10)],
            vec![Cell::Int(2), Cell::Int(20)],
            vec![Cell::Int(3), Cell::Int(30)],
        ]
    );
}

#[test]
fn cte_referenced_by_intersect_arms_resolves_independently_per_arm() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE base (id INTEGER)");
    exec(&conn, "INSERT INTO base VALUES (1), (2), (3), (4)");
    // Each arm applies its OWN filter to the same CTE `c` -- this only works if the CTE resolves
    // independently per arm (mirrors the `planner.rs` CTE-multi-reference fix's test pattern in
    // tests/cte_and_joins.rs, but across compound-SELECT arms instead of within one FROM clause).
    let rows = run_query_sorted(
        &io,
        &conn,
        "WITH c AS (SELECT id FROM base) \
         SELECT id FROM c WHERE id <= 3 INTERSECT SELECT id FROM c WHERE id >= 2",
    );
    assert_eq!(rows, ints(&[2, 3]));
}

#[test]
fn cte_referenced_by_except_arms_resolves_independently_per_arm() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE base (id INTEGER)");
    exec(&conn, "INSERT INTO base VALUES (1), (2), (3), (4)");
    let rows = run_query_sorted(
        &io,
        &conn,
        "WITH c AS (SELECT id FROM base) \
         SELECT id FROM c EXCEPT SELECT id FROM c WHERE id >= 3",
    );
    assert_eq!(rows, ints(&[1, 2]));
}

#[test]
fn cte_referenced_by_three_compound_arms() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE base (id INTEGER)");
    exec(&conn, "INSERT INTO base VALUES (1), (2), (3)");
    let rows = run_query_sorted(
        &io,
        &conn,
        "WITH c AS (SELECT id FROM base) \
         SELECT id FROM c WHERE id = 1 \
         UNION SELECT id FROM c WHERE id = 2 \
         UNION SELECT id FROM c WHERE id = 3",
    );
    assert_eq!(rows, ints(&[1, 2, 3]));
}

// -----------------------------------------------------------------------
// Miscellaneous regressions: LIMIT 0, deep mixed chains, INSERT INTO ... SELECT
// (a *different* real final destination -- CoroutineYield, not ResultRows).
// -----------------------------------------------------------------------

#[test]
fn intersect_limit_zero_returns_no_rows_without_erroring() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (1), (2)");
    let rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a INTERSECT SELECT x FROM b LIMIT 0",
    );
    assert_eq!(rows, Vec::<Vec<Cell>>::new());
}

#[test]
fn except_limit_zero_returns_no_rows_without_erroring() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (3)");
    let rows = run_query(&io, &conn, "SELECT x FROM a EXCEPT SELECT x FROM b LIMIT 0");
    assert_eq!(rows, Vec::<Vec<Cell>>::new());
}

#[test]
fn four_way_mixed_chain_groups_left_to_right() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE c (x INTEGER)");
    exec(&conn, "CREATE TABLE d (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2)");
    exec(&conn, "INSERT INTO b VALUES (2), (3)");
    exec(&conn, "INSERT INTO c VALUES (1), (3)");
    exec(&conn, "INSERT INTO d VALUES (2), (5)");
    // a={1,2}, b={2,3}, c={1,3}, d={2,5}. Left-to-right: `A UNION B EXCEPT C UNION ALL D` means
    // `((A UNION B) EXCEPT C) UNION ALL D`:
    //   (A UNION B) EXCEPT C = {2} (from the earlier two-way test), UNION ALL D = {2} ++ {2,5}
    //   = [2, 2, 5] as a multiset (UNION ALL does not dedupe).
    let mut rows = run_query(
        &io,
        &conn,
        "SELECT x FROM a UNION SELECT x FROM b EXCEPT SELECT x FROM c UNION ALL SELECT x FROM d",
    );
    rows.sort();
    assert_eq!(rows, ints(&[2, 2, 5]));
}

#[test]
fn insert_into_select_except_uses_the_coroutine_destination_not_result_rows() {
    // INSERT INTO ... SELECT drives the whole SELECT (compound or not) through
    // `QueryDestination::CoroutineYield`, not `ResultRows` -- a genuinely different "real final
    // destination" than every other test in this file exercises.
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE dest (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (1), (2), (3)");
    exec(&conn, "INSERT INTO b VALUES (2)");
    exec(
        &conn,
        "INSERT INTO dest SELECT x FROM a EXCEPT SELECT x FROM b",
    );
    let rows = run_query_sorted(&io, &conn, "SELECT x FROM dest");
    assert_eq!(rows, ints(&[1, 3]));
}

#[test]
fn insert_into_select_union_order_by_uses_the_coroutine_destination() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE a (x INTEGER)");
    exec(&conn, "CREATE TABLE b (x INTEGER)");
    exec(&conn, "CREATE TABLE dest (x INTEGER)");
    exec(&conn, "INSERT INTO a VALUES (3), (1)");
    exec(&conn, "INSERT INTO b VALUES (2), (1)");
    exec(
        &conn,
        "INSERT INTO dest SELECT x FROM a UNION SELECT x FROM b ORDER BY x DESC",
    );
    // Order is not guaranteed to survive a second SELECT without its own ORDER BY, but the
    // dedup + ORDER BY sort *during* the INSERT ... SELECT must still have run (via the
    // CoroutineYield-destination finalize path) for `dest` to end up with exactly {1,2,3}.
    let rows = run_query_sorted(&io, &conn, "SELECT x FROM dest");
    assert_eq!(rows, ints(&[1, 2, 3]));
}

#[test]
fn with_and_order_by_together_on_a_union() {
    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE base (id INTEGER)");
    exec(&conn, "INSERT INTO base VALUES (3), (1), (2)");
    let rows = run_query(
        &io,
        &conn,
        "WITH c AS (SELECT id FROM base) \
         SELECT id FROM c UNION SELECT id FROM c WHERE id > 1 ORDER BY id DESC",
    );
    assert_eq!(rows, ints(&[3, 2, 1]));
}
