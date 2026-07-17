use limbo_core::{Database, MemoryIO, StepResult, Value};
/// Integration tests for UPSERT (ON CONFLICT … DO NOTHING / DO UPDATE).
///
/// Slice 1 covers:
///   - ON CONFLICT(<pk-col>) DO NOTHING
///   - ON CONFLICT DO NOTHING  (no target / catch-all)
///   - Multi-row INSERT with mixed conflict/new rows
///   - Regression: plain INSERT, OR IGNORE, OR REPLACE still behave correctly
///   - Error paths: target-less DO UPDATE, unmatched constraint target
///
/// Slice 2 covers:
///   - DO UPDATE SET v = excluded.v
///   - DO UPDATE SET n = n + 1   (old-value reference)
///   - DO UPDATE SET n = n + excluded.n  (combined old + excluded)
///   - DO UPDATE … WHERE guard (true / false)
///   - Multi-row with mixed conflict/insert
///   - NOT NULL violation in DO UPDATE
///   - excluded.col invalid outside DO UPDATE
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_mem_db() -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
    let io: Arc<dyn limbo_core::IO> = Arc::new(MemoryIO::new());
    let db = Database::open_file(io.clone(), ":memory:", false).expect("open :memory:");
    let conn = db.connect().expect("connect");
    (io, conn)
}

fn exec(io: &Arc<dyn limbo_core::IO>, conn: &Arc<limbo_core::Connection>, sql: &str) {
    let mut stmt = conn
        .prepare(sql)
        .unwrap_or_else(|e| panic!("prepare {:?}: {:?}", sql, e));
    loop {
        match stmt
            .step()
            .unwrap_or_else(|e| panic!("step {:?}: {:?}", sql, e))
        {
            StepResult::Done => return,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Row => {}
            StepResult::Interrupt => panic!("interrupted in exec"),
        }
    }
}

fn query_one_int(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Option<i64> {
    let mut stmt = conn.prepare(sql).expect("prepare");
    loop {
        match stmt.step().expect("step") {
            StepResult::Row => {
                return match stmt.row().expect("row").get_value(0) {
                    Value::Integer(n) => Some(*n),
                    Value::Null => None,
                    other => panic!("unexpected {:?}", other),
                };
            }
            StepResult::Done => return None,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
}

fn query_one_text(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    sql: &str,
) -> Option<String> {
    let mut stmt = conn.prepare(sql).expect("prepare");
    loop {
        match stmt.step().expect("step") {
            StepResult::Row => {
                return match stmt.row().expect("row").get_value(0) {
                    Value::Text(t) => Some(String::from_utf8_lossy(&t.value).into_owned()),
                    Value::Null => None,
                    other => panic!("unexpected {:?}", other),
                };
            }
            StepResult::Done => return None,
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Interrupt => panic!("interrupted"),
        }
    }
}

fn count_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    table: &str,
) -> i64 {
    query_one_int(io, conn, &format!("SELECT COUNT(*) FROM {}", table)).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Default-build tests (rowid / INTEGER PRIMARY KEY targets, no extra index)
// ---------------------------------------------------------------------------

#[test]
fn do_nothing_skips_rowid_conflict() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'conflict') ON CONFLICT(id) DO NOTHING",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("original")
    );
}

#[test]
fn do_nothing_target_omitted_skips() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'conflict') ON CONFLICT DO NOTHING",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("original")
    );
}

#[test]
fn do_nothing_multirow_continues() {
    // Mixed: row 1 conflicts (skip), row 2 is new (insert), row 3 conflicts (skip)
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a'), (3, 'c')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'skip1'), (2, 'new'), (3, 'skip3') ON CONFLICT(id) DO NOTHING",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 3);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("a")
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=2").as_deref(),
        Some("new")
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=3").as_deref(),
        Some("c")
    );
}

#[test]
fn plain_insert_and_or_ignore_replace_unaffected() {
    // Regression: plain INSERT still errors on conflict
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    let result = conn
        .prepare("INSERT INTO t VALUES (1, 'conflict')")
        .and_then(|mut s| s.step());
    assert!(result.is_err(), "plain INSERT should error on PK conflict");

    // INSERT OR IGNORE still works
    exec(&io, &conn, "INSERT OR IGNORE INTO t VALUES (1, 'ignored')");
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("original")
    );

    // INSERT OR REPLACE still works
    exec(
        &io,
        &conn,
        "INSERT OR REPLACE INTO t VALUES (1, 'replaced')",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("replaced")
    );
}

// ---------------------------------------------------------------------------
// Negative tests
// ---------------------------------------------------------------------------

#[test]
fn do_update_target_omitted_errors() {
    let (_io, conn) = new_mem_db();
    // Target-less DO UPDATE must error at prepare time (no conflict target given)
    let result = conn.prepare("INSERT INTO t VALUES (1,'x') ON CONFLICT DO UPDATE SET v='y'");
    assert!(result.is_err(), "target-less DO UPDATE should error");
}

#[test]
fn on_conflict_no_matching_constraint_errors() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    // Column 'v' has no UNIQUE constraint — this target should error
    let result = conn.prepare("INSERT INTO t VALUES (1,'x') ON CONFLICT(v) DO NOTHING");
    assert!(
        result.is_err(),
        "no-matching-constraint target should error"
    );
}

// ---------------------------------------------------------------------------
// index_experimental tests (unique-index target)
// ---------------------------------------------------------------------------

#[cfg(feature = "index_experimental")]
#[test]
fn do_nothing_unique_index_target() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT)",
    );
    exec(&io, &conn, "CREATE UNIQUE INDEX t_email ON t(email)");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a@example.com')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (2, 'a@example.com') ON CONFLICT(email) DO NOTHING",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_int(&io, &conn, "SELECT id FROM t WHERE email='a@example.com'"),
        Some(1)
    );
}

// ---------------------------------------------------------------------------
// Slice 2 tests: DO UPDATE
// ---------------------------------------------------------------------------

#[test]
fn do_update_set_excluded_value() {
    // ON CONFLICT DO UPDATE SET v = excluded.v  (basic excluded.col)
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'updated') ON CONFLICT(id) DO UPDATE SET v = excluded.v",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("updated")
    );
}

#[test]
fn do_update_old_plus_one() {
    // ON CONFLICT DO UPDATE SET n = n + 1  (increment the existing value)
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 10)");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 99) ON CONFLICT(id) DO UPDATE SET n = n + 1",
    );
    assert_eq!(
        query_one_int(&io, &conn, "SELECT n FROM t WHERE id=1"),
        Some(11)
    );
}

#[test]
fn do_update_old_plus_excluded() {
    // ON CONFLICT DO UPDATE SET n = n + excluded.n
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 3)");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 7) ON CONFLICT(id) DO UPDATE SET n = n + excluded.n",
    );
    assert_eq!(
        query_one_int(&io, &conn, "SELECT n FROM t WHERE id=1"),
        Some(10)
    );
}

#[test]
fn do_update_where_false_unchanged() {
    // WHERE 0=1 must leave row untouched
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'keep')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'ignored') ON CONFLICT(id) DO UPDATE SET v = excluded.v WHERE 0=1",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("keep")
    );
}

#[test]
fn do_update_where_true_applies() {
    // WHERE 1=1 must apply the update
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'old')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'new') ON CONFLICT(id) DO UPDATE SET v = excluded.v WHERE 1=1",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("new")
    );
}

#[test]
fn do_update_multirow() {
    // Multiple rows: row 1 conflicts (update), row 2 is new (insert)
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'updated'), (2, 'fresh') ON CONFLICT(id) DO UPDATE SET v = excluded.v",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 2);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("updated")
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=2").as_deref(),
        Some("fresh")
    );
}

#[test]
fn do_update_notnull_violation_errors() {
    // Setting a NOT NULL column to NULL via DO UPDATE SET must error
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT NOT NULL)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    // DO UPDATE SET v = NULL should fail (NOT NULL constraint)
    let result =
        conn.prepare("INSERT INTO t VALUES (1, 'x') ON CONFLICT(id) DO UPDATE SET v = NULL");
    match result {
        Err(_) => {
            // parse-time rejection is fine
        }
        Ok(mut stmt) => {
            let mut errored = false;
            loop {
                match stmt.step() {
                    Ok(StepResult::Done) => break,
                    Ok(StepResult::IO) | Ok(StepResult::Busy) => io.run_once().expect("io"),
                    Ok(StepResult::Row) => {}
                    Ok(StepResult::Interrupt) => break,
                    Err(_) => {
                        errored = true;
                        break;
                    }
                }
            }
            assert!(errored, "NOT NULL violation in DO UPDATE must error");
        }
    }
}

#[test]
fn excluded_invalid_outside_do_update() {
    // excluded.col is only valid inside a DO UPDATE clause.
    // Referencing excluded in a plain SELECT should fail (no such table).
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    let result = conn.prepare("SELECT excluded.v FROM t");
    assert!(
        result.is_err(),
        "excluded.v outside DO UPDATE should fail at prepare time"
    );
}

// ---------------------------------------------------------------------------
// Slice 3 tests: coexistence hardening
// ---------------------------------------------------------------------------

/// `INSERT OR IGNORE … ON CONFLICT(id) DO UPDATE`:
/// The DO UPDATE fires on the id conflict (targeted), taking precedence over
/// the OR IGNORE fall-through.  Without the targeted clause, OR IGNORE would
/// have skipped the row; here the update must win.
#[test]
fn or_ignore_with_on_conflict_coexist() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");
    exec(
        &io,
        &conn,
        "INSERT OR IGNORE INTO t VALUES (1, 'proposed') ON CONFLICT(id) DO UPDATE SET v = excluded.v",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("proposed")
    );
}

/// Verify chain-of-one termination (`next = None`) is handled correctly for
/// both DO NOTHING and DO UPDATE clauses issued on the same table sequentially.
#[test]
fn chained_do_nothing_then_update() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'original')");

    // Single targeted DO NOTHING: chain terminates immediately (next=None).
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'conflict') ON CONFLICT(id) DO NOTHING",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("original")
    );

    // Now a single-clause DO UPDATE: verify the chain still works after the
    // previous DO NOTHING left the row unchanged.
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'v2') ON CONFLICT(id) DO UPDATE SET v = excluded.v",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("v2")
    );
}

#[cfg(feature = "index_experimental")]
#[test]
fn do_update_unique_index_target() {
    // ON CONFLICT(email) DO UPDATE SET name = excluded.name
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT, name TEXT)",
    );
    exec(
        &io,
        &conn,
        "CREATE UNIQUE INDEX users_email ON users(email)",
    );
    exec(
        &io,
        &conn,
        "INSERT INTO users VALUES (1, 'a@b.com', 'Alice')",
    );
    exec(
        &io,
        &conn,
        "INSERT INTO users VALUES (2, 'a@b.com', 'AliceNew') ON CONFLICT(email) DO UPDATE SET name = excluded.name",
    );
    // The original row (id=1) should have updated name; no new row inserted.
    assert_eq!(count_rows(&io, &conn, "users"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT name FROM users WHERE email='a@b.com'").as_deref(),
        Some("AliceNew")
    );
}

/// Chained ON CONFLICT with two distinct targets:
/// - ON CONFLICT(id) DO NOTHING — fires when id already exists (skip)
/// - ON CONFLICT(email) DO UPDATE SET name = excluded.name — fires when email already exists (update)
///
/// Two separate INSERTs each exercise one branch of the chain independently.
#[cfg(feature = "index_experimental")]
#[test]
fn chained_conflict_targets() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT, name TEXT)",
    );
    exec(&io, &conn, "CREATE UNIQUE INDEX t_email ON t(email)");
    exec(&io, &conn, "INSERT INTO t VALUES (1, 'a@x.com', 'Alice')");
    exec(&io, &conn, "INSERT INTO t VALUES (2, 'b@x.com', 'Bob')");

    // id=1 conflicts → DO NOTHING (first clause).  email is new so no index conflict.
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'new@x.com', 'X') ON CONFLICT(id) DO NOTHING ON CONFLICT(email) DO UPDATE SET name = excluded.name",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT name FROM t WHERE id=1").as_deref(),
        Some("Alice") // unchanged — DO NOTHING fired
    );

    // id=3 is new, email='a@x.com' conflicts → DO UPDATE (second clause).
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (3, 'a@x.com', 'NewAlice') ON CONFLICT(id) DO NOTHING ON CONFLICT(email) DO UPDATE SET name = excluded.name",
    );
    assert_eq!(
        query_one_text(&io, &conn, "SELECT name FROM t WHERE email='a@x.com'").as_deref(),
        Some("NewAlice") // updated — DO UPDATE fired
    );
    assert_eq!(count_rows(&io, &conn, "t"), 2); // still 2 rows (no new row was inserted)
}

/// `INSERT OR REPLACE … ON CONFLICT(email) DO UPDATE`:
/// When the email conflict fires, the targeted DO UPDATE takes precedence over
/// the OR REPLACE fall-through that would otherwise delete the victim row.
/// The existing row is updated in place rather than replaced.
#[cfg(feature = "index_experimental")]
#[test]
fn or_replace_with_on_conflict_coexist() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT, v TEXT)",
    );
    exec(&io, &conn, "CREATE UNIQUE INDEX t_email ON t(email)");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 'a@x.com', 'original')",
    );

    // email='a@x.com' conflicts → DO UPDATE fires (not OR REPLACE delete).
    exec(
        &io,
        &conn,
        "INSERT OR REPLACE INTO t VALUES (2, 'a@x.com', 'updated') ON CONFLICT(email) DO UPDATE SET v = excluded.v",
    );
    // Row 1 should have v updated; row 2 should NOT have been inserted.
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE id=1").as_deref(),
        Some("updated")
    );
}

/// Composite primary key ON CONFLICT target → DO UPDATE.
/// A table with `PRIMARY KEY(a, b)` has an automatic unique index for (a, b).
/// Targeting that composite key with `ON CONFLICT(a, b)` must match it and
/// trigger the DO UPDATE when both columns conflict.
#[cfg(feature = "index_experimental")]
#[test]
fn composite_pk_target_do_update() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (a INTEGER, b INTEGER, v TEXT, PRIMARY KEY(a, b))",
    );
    exec(&io, &conn, "INSERT INTO t VALUES (1, 2, 'original')");
    exec(
        &io,
        &conn,
        "INSERT INTO t VALUES (1, 2, 'updated') ON CONFLICT(a, b) DO UPDATE SET v = excluded.v",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT v FROM t WHERE a=1 AND b=2").as_deref(),
        Some("updated")
    );
}

// ---------------------------------------------------------------------------
// Generated column rejection (ON CONFLICT DO UPDATE SET)
// ---------------------------------------------------------------------------

/// `DO UPDATE SET` targeting a `GENERATED ALWAYS AS (...)` column must be
/// rejected with a clear parse error naming the column -- not silently
/// accepted (which would either be a no-op or leave the generated column out
/// of sync with the expression it's supposed to always reflect).
///
/// `x` is declared `INTEGER PRIMARY KEY` (a rowid alias) so `ON CONFLICT(x)`
/// is a valid, matching conflict target reachable under default features:
/// without a real PK/UNIQUE constraint on some column, the conflict-target
/// resolver rejects the statement before ever reaching the SET-clause check
/// this test exercises (see `on_conflict_no_matching_constraint_errors`
/// above), which would make the test pass for the wrong reason.
#[test]
fn set_generated_column_errors() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (x INTEGER PRIMARY KEY, y INT GENERATED ALWAYS AS (x + 1))",
    );
    let result = conn.prepare("INSERT INTO t (x) VALUES (1) ON CONFLICT(x) DO UPDATE SET y = 5");
    let err = match result {
        Ok(_) => panic!("SET on a generated column must be rejected, not accepted"),
        Err(e) => e,
    };
    let message = err.to_string();
    assert!(
        message.contains("generated column"),
        "expected a clear generated-column error, got: {message}"
    );
    assert!(
        message.contains('y'),
        "expected the error to name the offending column, got: {message}"
    );
}

/// Companion regression: a non-generated column in the very same table must
/// still work normally in the SET clause -- the rejection above is specific
/// to the generated column, not an overly broad guard on the whole table.
#[test]
fn set_non_generated_column_in_same_table_still_works() {
    let (io, conn) = new_mem_db();
    exec(
        &io,
        &conn,
        "CREATE TABLE t (x INTEGER PRIMARY KEY, label TEXT, y INT GENERATED ALWAYS AS (x + 1))",
    );
    exec(&io, &conn, "INSERT INTO t (x, label) VALUES (1, 'a')");
    exec(
        &io,
        &conn,
        "INSERT INTO t (x, label) VALUES (1, 'b') ON CONFLICT(x) DO UPDATE SET label = excluded.label",
    );
    assert_eq!(count_rows(&io, &conn, "t"), 1);
    assert_eq!(
        query_one_text(&io, &conn, "SELECT label FROM t WHERE x = 1").as_deref(),
        Some("b")
    );
}
