//! Integration tests for typed Value round-trips through the sqlite-compat path.
//!
//! These tests verify that columns declared with DATE, TIMESTAMP, TIME, and UUID
//! types are converted to the corresponding rich `Value` variants on read-back
//! rather than staying as raw `I64` or `Text` values.

use oxisql_core::{Connection, Value};
use oxisql_sqlite_compat::SqliteConnection;

// ── Date round-trip ───────────────────────────────────────────────────────────

/// Verify that a DATE column stored as a text ISO-8601 string comes back as
/// `Value::Date(days)` where days is counted from 1970-01-01.
#[tokio::test]
async fn test_date_text_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_date (d DATE)", &[])
        .await
        .expect("create table");

    // 2024-01-15 is (2024 - 1970) years + days offset.
    // Days since epoch: use the formula days_since_epoch("2024-01-15") = 19737
    conn.execute("INSERT INTO t_date VALUES ('2024-01-15')", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT d FROM t_date", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Date(days)) => {
            // 2024-01-15 = 19737 days since Unix epoch
            assert_eq!(
                *days, 19737,
                "expected 19737 days for 2024-01-15, got {days}"
            );
        }
        other => panic!("expected Value::Date, got {other:?}"),
    }
}

// ── Date integer round-trip ───────────────────────────────────────────────────

/// Verify that a DATE column stored as an integer (days since epoch) comes back
/// as `Value::Date(days)`.
#[tokio::test]
async fn test_date_integer_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_date_int (d DATE)", &[])
        .await
        .expect("create table");

    // 19737 = days since epoch for 2024-01-15
    conn.execute("INSERT INTO t_date_int VALUES (19737)", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT d FROM t_date_int", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Date(days)) => {
            assert_eq!(*days, 19737, "expected 19737 days, got {days}");
        }
        other => panic!("expected Value::Date, got {other:?}"),
    }
}

// ── Timestamp round-trip ──────────────────────────────────────────────────────

/// Verify that a TIMESTAMP column stored as an ISO-8601 datetime text comes
/// back as `Value::Timestamp(µs)`.
#[tokio::test]
async fn test_timestamp_text_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_ts (ts TIMESTAMP)", &[])
        .await
        .expect("create table");

    // Unix epoch itself — 0 µs
    conn.execute("INSERT INTO t_ts VALUES ('1970-01-01 00:00:00')", &[])
        .await
        .expect("insert epoch");

    let rows = conn
        .query("SELECT ts FROM t_ts", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Timestamp(us)) => {
            assert_eq!(*us, 0, "epoch should be 0 µs, got {us}");
        }
        other => panic!("expected Value::Timestamp, got {other:?}"),
    }
}

/// Verify that a DATETIME column behaves identically to TIMESTAMP.
#[tokio::test]
async fn test_datetime_text_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_dt (ts DATETIME)", &[])
        .await
        .expect("create table");

    // One second after epoch = 1_000_000 µs
    conn.execute("INSERT INTO t_dt VALUES ('1970-01-01 00:00:01')", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT ts FROM t_dt", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Timestamp(us)) => {
            assert_eq!(*us, 1_000_000, "1 sec = 1_000_000 µs, got {us}");
        }
        other => panic!("expected Value::Timestamp, got {other:?}"),
    }
}

// ── Time round-trip ───────────────────────────────────────────────────────────

/// Verify that a TIME column stored as "HH:MM:SS" comes back as
/// `Value::Time(µs_since_midnight)`.
#[tokio::test]
async fn test_time_text_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_time (t TIME)", &[])
        .await
        .expect("create table");

    // 01:00:00 = 3600 * 1_000_000 µs since midnight
    conn.execute("INSERT INTO t_time VALUES ('01:00:00')", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT t FROM t_time", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Time(us)) => {
            let expected = 3_600_000_000i64;
            assert_eq!(*us, expected, "01:00:00 = {expected} µs, got {us}");
        }
        other => panic!("expected Value::Time, got {other:?}"),
    }
}

// ── UUID round-trip ───────────────────────────────────────────────────────────

/// Verify that a UUID column stored as a hyphenated hex text string comes back
/// as `Value::Uuid(u128)`.
#[tokio::test]
async fn test_uuid_text_roundtrip() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_uuid (u UUID)", &[])
        .await
        .expect("create table");

    let uuid_str = "550e8400-e29b-41d4-a716-446655440000";
    conn.execute(&format!("INSERT INTO t_uuid VALUES ('{uuid_str}')"), &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT u FROM t_uuid", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Uuid(u)) => {
            let expected: u128 =
                u128::from_str_radix("550e8400e29b41d4a716446655440000", 16).expect("valid hex");
            assert_eq!(*u, expected, "uuid value mismatch: got {u:#034x}");
        }
        other => panic!("expected Value::Uuid, got {other:?}"),
    }
}

// ── Fallback: plain TEXT stays Text ──────────────────────────────────────────

/// Verify that a TEXT column is NOT re-typed even when its content looks like
/// a date string.
#[tokio::test]
async fn test_plain_text_no_false_retyping() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_text (s TEXT)", &[])
        .await
        .expect("create table");

    conn.execute("INSERT INTO t_text VALUES ('2024-01-15')", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT s FROM t_text", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::Text(s)) => {
            assert_eq!(s.as_str(), "2024-01-15", "text value should be unchanged");
        }
        other => panic!("expected Value::Text, got {other:?}"),
    }
}

/// Verify that an INTEGER column is NOT re-typed when declared as INTEGER.
#[tokio::test]
async fn test_plain_integer_no_false_retyping() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute("CREATE TABLE t_int (n INTEGER)", &[])
        .await
        .expect("create table");

    conn.execute("INSERT INTO t_int VALUES (42)", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT n FROM t_int", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    match rows[0].get_by_index(0) {
        Some(Value::I64(n)) => {
            assert_eq!(*n, 42, "integer value should be unchanged");
        }
        other => panic!("expected Value::I64, got {other:?}"),
    }
}

// ── NULL handling ─────────────────────────────────────────────────────────────

/// Verify that a NULL in a typed column comes back as Value::Null.
#[tokio::test]
async fn test_typed_column_null() {
    let conn = SqliteConnection::open_memory()
        .await
        .expect("open_memory failed");

    conn.execute(
        "CREATE TABLE t_null_typed (d DATE, ts TIMESTAMP, u UUID)",
        &[],
    )
    .await
    .expect("create table");

    conn.execute("INSERT INTO t_null_typed VALUES (NULL, NULL, NULL)", &[])
        .await
        .expect("insert");

    let rows = conn
        .query("SELECT d, ts, u FROM t_null_typed", &[])
        .await
        .expect("select");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_index(0), Some(&Value::Null));
    assert_eq!(rows[0].get_by_index(1), Some(&Value::Null));
    assert_eq!(rows[0].get_by_index(2), Some(&Value::Null));
}
