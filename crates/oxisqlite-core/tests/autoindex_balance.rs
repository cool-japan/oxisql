//! Regression test for the interior-index divider-overflow balance bug.
//!
//! A join whose inner table has no usable index makes the planner build a
//! transient automatic (ephemeral) index over that table. Building the index
//! over enough rows with widely-varied TEXT key lengths drives the index
//! b-tree deep enough that an interior index page must balance and push a large
//! divider cell up into its parent. When that divider does not fit it is
//! deferred to the parent's `overflow_cells`; a smaller following divider that
//! still fits in the leftover free space then used to take the in-place insert
//! path at a logical position past the parent's physical cell count, underflowing
//! `page.cell_count() - cell_idx` in `insert_into_cell` and panicking (in release
//! it silently corrupted the page and the join returned wrong/zero rows).
//!
//! This test recreates that shape synthetically (no external database file) and
//! asserts the join returns exactly the expected rows.
//!
//! The ephemeral-index planner path is only compiled under `index_experimental`,
//! so the test is gated on that feature — without it the join is a plain scan
//! and never exercises the index-balance path this guards.
#![cfg(feature = "index_experimental")]

use std::sync::Arc;

use limbo_core::{Connection, Database, MemoryIO, StepResult, Value, IO};

fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory: database");
    let conn = db.connect().expect("connect to :memory: database");
    (io, conn)
}

fn exec(conn: &Arc<Connection>, sql: &str) {
    conn.execute(sql)
        .unwrap_or_else(|e| panic!("execute failed for {sql}: {e:?}"));
}

fn run_query(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) -> Vec<(String, String)> {
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
                let values: Vec<Value> = row.get_values().cloned().collect();
                let text = |v: &Value| match v {
                    Value::Text(t) => t.as_str().to_string(),
                    other => panic!("expected TEXT, got {other:?}"),
                };
                out.push((text(&values[0]), text(&values[1])));
            }
            StepResult::IO => io.run_once().unwrap(),
            StepResult::Done => break,
            other => panic!("unexpected step result for {sql}: {other:?}"),
        }
    }
    out
}

/// Deterministic key with widely-varied byte length so index cells span sizes
/// and interior index pages must balance with large divider cells.
fn key_for(i: usize) -> String {
    let base = format!("K{i:06}");
    let extra = (i * 37) % 480;
    let mut s = String::with_capacity(base.len() + extra);
    s.push_str(&base);
    for j in 0..extra {
        s.push((b'a' + ((i + j) % 26) as u8) as char);
    }
    s
}

fn payload_for(i: usize) -> String {
    format!("p{}{}", i, "z".repeat((i * 13) % 300))
}

#[test]
fn join_autoindex_interior_divider_overflow() {
    // Sized well above the observed reproduction threshold so the index b-tree
    // is deep enough for an interior page to balance a divider that overflows
    // its parent. Keep deterministic for a stable, self-contained repro.
    const N: usize = 3000;
    // Every OUTER_STEP-th inner key is also probed via the outer table.
    const OUTER_STEP: usize = 371;

    let (io, conn) = new_conn();
    exec(&conn, "CREATE TABLE inner_t (k TEXT, payload TEXT)");
    exec(&conn, "CREATE TABLE outer_t (k TEXT)");

    exec(&conn, "BEGIN");
    for i in 0..N {
        // Keys/payloads contain only [A-Za-z0-9]; safe to inline as SQL literals.
        exec(
            &conn,
            &format!(
                "INSERT INTO inner_t (k, payload) VALUES ('{}', '{}')",
                key_for(i),
                payload_for(i)
            ),
        );
    }
    let mut expected: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < N {
        exec(
            &conn,
            &format!("INSERT INTO outer_t (k) VALUES ('{}')", key_for(i)),
        );
        expected.push((key_for(i), payload_for(i)));
        i += OUTER_STEP;
    }
    exec(&conn, "COMMIT");

    expected.sort();

    // The inner table has no index on `k`, and it is not the leftmost table, so
    // the planner builds a transient automatic index over it — the code path
    // that balanced the index b-tree and hit the divider-overflow bug.
    let sql = "SELECT o.k, i.payload FROM outer_t o JOIN inner_t i ON i.k = o.k ORDER BY o.k";
    let mut rows = run_query(&io, &conn, sql);
    rows.sort();

    assert_eq!(
        rows, expected,
        "auto-index join returned wrong rows (index b-tree balance corruption): got {} rows, expected {}",
        rows.len(),
        expected.len()
    );
}
