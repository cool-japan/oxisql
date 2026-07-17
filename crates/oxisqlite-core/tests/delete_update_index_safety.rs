//! Correctness tests for index-based DELETE/UPDATE table-access selection.
//!
//! `translate/optimizer/mod.rs` re-enables `optimize_table_access` for DELETE
//! and UPDATE plans, but only keeps its answer when
//! `translate/optimizer/dml_safety.rs` proves the resulting access method safe;
//! anything else keeps falling back to the pre-existing full table scan. This
//! file exercises the actual DML *results* end-to-end (row counts and exact
//! per-row values) for both the newly-enabled cases and the cases that must
//! keep falling back, with enough rows that any skipped or double-processed
//! row would visibly change the count/values. See
//! `translate/optimizer/dml_safety.rs`'s `plan_tests` module (an in-crate test,
//! since `translate` is a private module) for direct assertions that the
//! chosen access method is actually what we expect, rather than just checking
//! results here.
//!
//! All indexed-table DELETE/UPDATE support requires `index_experimental`
//! (see translate/delete.rs, translate/update.rs, translate/index.rs).

#![cfg(feature = "index_experimental")]

use std::sync::Arc;

use limbo_core::{Connection, Database, MemoryIO, StepResult, Value, IO};

fn new_mem_db() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
    let conn = db.connect().expect("connect");
    (io, conn)
}

fn exec(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {sql:?}: {e:?}"));
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {sql:?}: {e:?}"))
        {
            StepResult::Done => return,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {}
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
}

fn as_int(value: &Value) -> i64 {
    match value {
        Value::Integer(i) => *i,
        other => panic!("expected an integer column, got {other:?}"),
    }
}

/// Runs a query expected to yield rows of `(col0, col1)` integers, e.g.
/// `SELECT id, x FROM t ORDER BY id`.
fn query_pairs(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) -> Vec<(i64, i64)> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {sql:?}: {e:?}"));
    let mut rows = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {sql:?}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                rows.push((as_int(row.get_value(0)), as_int(row.get_value(1))));
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

/// Runs a query expected to yield rows of a single integer column.
fn query_ints(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) -> Vec<i64> {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {sql:?}: {e:?}"));
    let mut rows = Vec::new();
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {sql:?}: {e:?}"))
        {
            StepResult::Row => {
                let row = stmt.row().expect("row available after StepResult::Row");
                rows.push(as_int(row.get_value(0)));
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => break,
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
    rows
}

/// A deterministic pseudo-shuffle so that the indexed column's sort order
/// differs from insertion/rowid order -- if a re-traversal bug skipped or
/// revisited rows relative to the LIVE index's key order (rather than rowid
/// order), a value sequence that happens to already be sorted by rowid could
/// mask it.
fn shuffled_value(i: i64, modulus: i64, offset: i64) -> i64 {
    ((i * 73) % modulus) - offset
}

const ROW_COUNT: i64 = 150;

#[test]
fn delete_secondary_index_mass_delete_no_skip_or_double_delete() {
    // This mirrors the shape of the upstream repro (tursodatabase/limbo#1714):
    // a DELETE whose WHERE clause is selective on a secondarily-indexed column,
    // over enough rows that a single skipped or double-deleted row would show
    // up in the final count/id-set. Our optimizer keeps this on a full table
    // scan (see dml_safety::delete_access_method_is_safe), so this test is a
    // correctness regression-lock: if that safety gate is ever loosened
    // incorrectly, this is the kind of test that should catch it.
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
    );
    exec(&io, &conn, "CREATE INDEX idx_x ON t(x)");

    let mut expected_survivors = Vec::new();
    for i in 1..=ROW_COUNT {
        let x = shuffled_value(i, ROW_COUNT, 50);
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, {x})"));
        if x <= 20 {
            expected_survivors.push(i);
        }
    }

    exec(&io, &conn, "DELETE FROM t WHERE x > 20");

    let remaining_ids = query_ints(&io, &conn, "SELECT id FROM t ORDER BY id");
    assert_eq!(
        remaining_ids, expected_survivors,
        "DELETE via a secondary index must remove exactly the matching rows, no more, no less"
    );
}

#[test]
fn update_set_x_plus_5_where_x_gt_10_indexed_on_x_exact_repro() {
    // The exact scenario the original FIXME comment described:
    // `UPDATE t SET x=x+5 WHERE x>10` where `x` is indexed. Traversing an index
    // while mutating the very column it is keyed on is the classic hazard, so
    // this must keep falling back to a full table scan (locked in by
    // dml_safety::plan_tests::update_falls_back_to_full_scan_when_set_touches_indexed_column).
    // This test verifies the actual DML result: every row must be updated
    // EXACTLY once (no skips, no double-application of `+5`), with values
    // straddling the WHERE boundary so a traversal bug would show up either as
    // a wrong final value or a wrong survivor count.
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
    );
    exec(&io, &conn, "CREATE INDEX idx_x ON t(x)");

    let mut expected = Vec::new();
    for i in 1..=ROW_COUNT {
        let x = shuffled_value(i, ROW_COUNT, 60);
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, {x})"));
        let expected_x = if x > 10 { x + 5 } else { x };
        expected.push((i, expected_x));
    }

    exec(&io, &conn, "UPDATE t SET x = x + 5 WHERE x > 10");

    let actual = query_pairs(&io, &conn, "SELECT id, x FROM t ORDER BY id");
    assert_eq!(
        actual.len() as i64,
        ROW_COUNT,
        "no row should be gained or lost by an UPDATE"
    );
    assert_eq!(
        actual, expected,
        "every row with original x>10 must become exactly x+5 exactly once; \
         rows with original x<=10 must be untouched"
    );
}

#[test]
fn update_disjoint_index_now_uses_index_with_correct_results() {
    // `x` is indexed and drives the scan; the SET clause only touches `y`, so
    // this is the newly-enabled safe case
    // (dml_safety::plan_tests::update_uses_index_when_disjoint_from_set_clause
    // confirms the plan actually uses idx_x). Verify the actual values are
    // correct over enough rows that a skipped/double-updated row would show up.
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER, y INTEGER)",
    );
    exec(&io, &conn, "CREATE INDEX idx_x ON t(x)");

    let mut expected = Vec::new();
    for i in 1..=ROW_COUNT {
        let x = shuffled_value(i, ROW_COUNT, 60);
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, {x}, 0)"));
        let expected_y = if x > 10 { 1 } else { 0 };
        expected.push((i, expected_y));
    }

    exec(&io, &conn, "UPDATE t SET y = y + 1 WHERE x > 10");

    let actual = query_pairs(&io, &conn, "SELECT id, y FROM t ORDER BY id");
    assert_eq!(actual.len() as i64, ROW_COUNT);
    assert_eq!(
        actual, expected,
        "every row with x>10 must get y=1 exactly once; others must stay y=0"
    );

    // x itself must be completely untouched.
    let x_values = query_pairs(&io, &conn, "SELECT id, x FROM t ORDER BY id");
    let expected_x: Vec<(i64, i64)> = (1..=ROW_COUNT)
        .map(|i| (i, shuffled_value(i, ROW_COUNT, 60)))
        .collect();
    assert_eq!(x_values, expected_x, "x must be unchanged by this UPDATE");
}

#[test]
fn delete_rowid_range_now_uses_rowid_seek_with_correct_results() {
    // No secondary index at all: WHERE is purely on the rowid (`id`), which is
    // the newly-enabled Search::Seek{index: None} path
    // (dml_safety::plan_tests::delete_uses_rowid_range_seek_for_pk_range
    // confirms the plan shape). This uses the table's own cursor for both
    // traversal and deletion -- the same self-mutation mechanism the mandatory
    // full-table-scan fallback already relies on for every DELETE today, just
    // over a narrower range.
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
    );
    for i in 1..=ROW_COUNT {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, {i})"));
    }

    exec(&io, &conn, "DELETE FROM t WHERE id > 30 AND id < 120");

    let remaining_ids = query_ints(&io, &conn, "SELECT id FROM t ORDER BY id");
    let expected: Vec<i64> = (1..=ROW_COUNT).filter(|&i| !(i > 30 && i < 120)).collect();
    assert_eq!(
        remaining_ids, expected,
        "DELETE via a rowid range seek must remove exactly the matching rows"
    );
}

#[test]
fn delete_rowid_eq_removes_exactly_one_row() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
    );
    for i in 1..=ROW_COUNT {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, {i})"));
    }

    exec(&io, &conn, "DELETE FROM t WHERE id = 75");

    let remaining_ids = query_ints(&io, &conn, "SELECT id FROM t ORDER BY id");
    let expected: Vec<i64> = (1..=ROW_COUNT).filter(|&i| i != 75).collect();
    assert_eq!(remaining_ids, expected);
}

#[test]
fn update_rowid_eq_updates_exactly_one_row() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, x INTEGER)",
    );
    for i in 1..=ROW_COUNT {
        exec(&io, &conn, &format!("INSERT INTO t VALUES ({i}, 0)"));
    }

    exec(&io, &conn, "UPDATE t SET x = 999 WHERE id = 75");

    let actual = query_pairs(&io, &conn, "SELECT id, x FROM t ORDER BY id");
    let expected: Vec<(i64, i64)> = (1..=ROW_COUNT)
        .map(|i| (i, if i == 75 { 999 } else { 0 }))
        .collect();
    assert_eq!(actual, expected);
}
