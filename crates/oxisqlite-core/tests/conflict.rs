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
            // Both IO and Busy require an io.run_once() pump before retrying.
            StepResult::IO | StepResult::Busy => io.run_once()?,
            StepResult::Row => {}
            StepResult::Interrupt => return Err(limbo_core::LimboError::Busy),
        }
    }
}

fn count_rows(
    io: &Arc<dyn limbo_core::IO>,
    conn: &Arc<limbo_core::Connection>,
    table: &str,
) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let mut stmt = conn.prepare(&sql).expect("prepare count");
    loop {
        match stmt.step().expect("step count") {
            StepResult::Row => {
                return match stmt.row().expect("row").get_value(0) {
                    Value::Integer(i) => *i,
                    other => panic!("expected integer count, got {other:?}"),
                };
            }
            StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
            StepResult::Done => panic!("count query returned no row"),
            StepResult::Interrupt => panic!("interrupted in count query"),
        }
    }
}

#[test]
fn insert_or_fail_keeps_prior_rows() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed first row");
    // OR FAIL: conflict on id=1 should return an error but keep id=1 row
    let result = exec(&io, &conn, "INSERT OR FAIL INTO t VALUES (1)");
    assert!(result.is_err(), "INSERT OR FAIL must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "prior row must survive OR FAIL"
    );
}

#[test]
fn insert_or_abort_removes_partial_rows() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    // Multi-row INSERT: first value (2) would succeed, second (1) conflicts.
    // OR ABORT must roll back the partial insert (id=2), leaving only seed (id=1).
    let result = exec(&io, &conn, "INSERT OR ABORT INTO t VALUES (2), (1)");
    assert!(result.is_err(), "INSERT OR ABORT must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "ABORT must roll back partial multi-row insert"
    );
}

#[test]
fn insert_or_rollback_errors_on_conflict() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    // OR ROLLBACK: conflict must return an error
    let result = exec(&io, &conn, "INSERT OR ROLLBACK INTO t VALUES (1)");
    assert!(result.is_err(), "INSERT OR ROLLBACK must error on conflict");
}

#[test]
fn insert_or_ignore_skips_conflicting_row() {
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    exec(&io, &conn, "INSERT OR IGNORE INTO t VALUES (1)").expect("IGNORE must not error");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "IGNORE must skip the conflicting row"
    );
}

#[test]
fn insert_default_abort_removes_partial_rows() {
    // Default INSERT (no explicit OR clause) must behave like ABORT:
    // conflict reverts ALL rows inserted by this statement.
    let (io, conn) = new_mem_db();
    exec(&io, &conn, "CREATE TABLE t (id INTEGER PRIMARY KEY)").expect("create table");
    exec(&io, &conn, "INSERT INTO t VALUES (1)").expect("seed row");
    let result = exec(&io, &conn, "INSERT INTO t VALUES (2), (1)");
    assert!(result.is_err(), "default INSERT must error on conflict");
    assert_eq!(
        count_rows(&io, &conn, "t"),
        1,
        "default INSERT ABORT must roll back partial inserts"
    );
}

#[cfg(feature = "index_experimental")]
mod column_unique_on_conflict_tests {
    use super::{count_rows, exec, new_mem_db, Arc, StepResult, Value};

    // ---------------------------------------------------------------------------
    // Per-constraint `ON CONFLICT <action>` on CREATE TABLE UNIQUE constraints.
    //
    // These are distinct from (and independent of) the statement-level
    // `INSERT/UPDATE OR <action>` tests above: here the conflict-resolution
    // action is declared once on the constraint itself, at CREATE TABLE time, and
    // applies whenever no statement-level clause overrides it.
    //
    // All of these tests create a secondary index implicitly, via a
    // column-level `UNIQUE` constraint, which requires `index_experimental`
    // (see the automatic-index gate in translate/schema.rs), matching the
    // rest of this codebase's convention for index-dependent tests (e.g.
    // tests/analyze.rs's `index_tests` module).
    // ---------------------------------------------------------------------------

    /// Read a single integer column back from a query expected to return exactly
    /// one row (e.g. `SELECT id FROM t WHERE x = ...`).
    fn read_one_int(
        io: &Arc<dyn limbo_core::IO>,
        conn: &Arc<limbo_core::Connection>,
        sql: &str,
    ) -> i64 {
        let mut stmt = conn.prepare(sql).expect("prepare");
        loop {
            match stmt.step().expect("step") {
                StepResult::Row => {
                    return match stmt.row().expect("row").get_value(0) {
                        Value::Integer(i) => *i,
                        other => panic!("expected integer, got {other:?}"),
                    };
                }
                StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
                StepResult::Done => panic!("query `{sql}` returned no row"),
                StepResult::Interrupt => panic!("interrupted"),
            }
        }
    }

    #[test]
    fn column_unique_on_conflict_replace_applies_without_statement_level_clause() {
        let (io, conn) = new_mem_db();
        exec(
            &io,
            &conn,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER UNIQUE ON CONFLICT REPLACE)",
        )
        .expect("create table");
        exec(&io, &conn, "INSERT INTO t VALUES (1, 100)").expect("seed row");
        // Plain INSERT (no OR clause): the constraint's own ON CONFLICT REPLACE
        // must apply instead of the ABORT default, deleting the id=1 victim and
        // inserting the new id=2 row in its place.
        exec(&io, &conn, "INSERT INTO t VALUES (2, 100)")
            .expect("constraint-level ON CONFLICT REPLACE must not error");
        assert_eq!(
            count_rows(&io, &conn, "t"),
            1,
            "REPLACE must delete the conflicting victim row"
        );
        assert_eq!(
            read_one_int(&io, &conn, "SELECT id FROM t WHERE x = 100"),
            2,
            "surviving row must be the newly inserted one (id=2), proving REPLACE \
         actually happened rather than the insert being silently ignored"
        );
    }

    #[test]
    fn column_unique_on_conflict_ignore_applies_without_statement_level_clause() {
        let (io, conn) = new_mem_db();
        exec(
            &io,
            &conn,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER UNIQUE ON CONFLICT IGNORE)",
        )
        .expect("create table");
        exec(&io, &conn, "INSERT INTO t VALUES (1, 100)").expect("seed row");
        // Plain INSERT (no OR clause): the constraint's own ON CONFLICT IGNORE
        // must apply instead of the ABORT default, silently skipping the new row.
        exec(&io, &conn, "INSERT INTO t VALUES (2, 100)")
            .expect("constraint-level ON CONFLICT IGNORE must not error");
        assert_eq!(
            count_rows(&io, &conn, "t"),
            1,
            "IGNORE must skip the conflicting row, leaving exactly the seed row"
        );
        assert_eq!(
            read_one_int(&io, &conn, "SELECT id FROM t WHERE x = 100"),
            1,
            "surviving row must be the original seed row (id=1), proving the new \
         row was skipped rather than replacing it"
        );
    }

    #[test]
    fn statement_level_rollback_overrides_column_constraint_replace() {
        // Precedence: an explicit statement-level `INSERT OR <action>` must win
        // over the constraint's own `ON CONFLICT <action>`.
        let (io, conn) = new_mem_db();
        exec(
            &io,
            &conn,
            "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER UNIQUE ON CONFLICT REPLACE)",
        )
        .expect("create table");
        exec(&io, &conn, "INSERT INTO t VALUES (1, 100)").expect("seed row");
        let result = exec(&io, &conn, "INSERT OR ROLLBACK INTO t VALUES (2, 100)");
        assert!(
            result.is_err(),
            "statement-level OR ROLLBACK must override the constraint's own \
         ON CONFLICT REPLACE and error instead of silently replacing"
        );
        assert_eq!(
            count_rows(&io, &conn, "t"),
            1,
            "conflicting row must not have been inserted"
        );
        assert_eq!(
            read_one_int(&io, &conn, "SELECT id FROM t WHERE x = 100"),
            1,
            "original seed row (id=1) must be untouched — REPLACE must NOT have run"
        );
    }

    /// Open (or create) a file-backed database at `path` using a fresh `SyscallIO`.
    fn open_file_db(
        path: &std::path::Path,
    ) -> (Arc<dyn limbo_core::IO>, Arc<limbo_core::Connection>) {
        let io: Arc<dyn limbo_core::IO> = Arc::new(limbo_core::SyscallIO::new().expect("new io"));
        let db =
            limbo_core::Database::open_file(io.clone(), path.to_str().expect("utf8 path"), false)
                .expect("open file db");
        let conn = db.connect().expect("connect");
        (io, conn)
    }

    fn temp_db_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "oxisqlite_conflict_test_{}_{}_{}.db",
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
    fn column_unique_on_conflict_replace_survives_reopen() {
        // This is the test that proves `schema::table::create_table` (the
        // reload-from-persisted-SQL path, exercised here by closing and reopening
        // the connection) stores the constraint's own `ON CONFLICT REPLACE`
        // identically to `translate::schema` (the fresh-CREATE-TABLE path already
        // covered by `column_unique_on_conflict_replace_applies_without_statement_level_clause`
        // above). If only one of the two code paths captured the resolution, this
        // test would silently fall back to the ABORT default and fail.
        let path = temp_db_path("on_conflict_replace_reopen");
        cleanup_db_files(&path);

        {
            let (io, conn) = open_file_db(&path);
            exec(
                &io,
                &conn,
                "CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER UNIQUE ON CONFLICT REPLACE)",
            )
            .expect("create table");
            conn.close().expect("clean close");
        }

        // Reopen: the schema for `t` is now rebuilt entirely from the persisted
        // `sqlite_schema.sql` text via `schema::table::create_table`.
        let (io, conn) = open_file_db(&path);
        exec(&io, &conn, "INSERT INTO t VALUES (1, 100)").expect("seed row after reopen");
        exec(&io, &conn, "INSERT INTO t VALUES (2, 100)")
            .expect("constraint-level ON CONFLICT REPLACE must survive reopen");
        assert_eq!(
            count_rows(&io, &conn, "t"),
            1,
            "REPLACE must delete the conflicting victim row after reopen"
        );
        assert_eq!(
            read_one_int(&io, &conn, "SELECT id FROM t WHERE x = 100"),
            2,
            "surviving row must be the newly inserted one (id=2) after reopen"
        );

        cleanup_db_files(&path);
    }
}
