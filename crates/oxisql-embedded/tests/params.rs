//! Tests for SQL-injection-safe parameter binding.
//!
//! Exercises [`escape_sql_value`] and [`bind_params`] in isolation, then
//! validates that the binding works end-to-end through [`EmbeddedConnection`].

use oxisql_core::{Connection, OxiSqlError, Value};
use oxisql_embedded::{bind_params, escape_sql_value, EmbeddedConnection};

// ── escape_sql_value unit tests ───────────────────────────────────────────────

#[test]
fn test_escape_null() {
    assert_eq!(escape_sql_value(&Value::Null), "NULL");
}

#[test]
fn test_escape_bool_true() {
    assert_eq!(escape_sql_value(&Value::Bool(true)), "TRUE");
}

#[test]
fn test_escape_bool_false() {
    assert_eq!(escape_sql_value(&Value::Bool(false)), "FALSE");
}

#[test]
fn test_escape_i64_positive() {
    assert_eq!(escape_sql_value(&Value::I64(42)), "42");
}

#[test]
fn test_escape_i64_negative() {
    assert_eq!(escape_sql_value(&Value::I64(-7)), "-7");
}

#[test]
fn test_escape_f64_finite() {
    assert_eq!(escape_sql_value(&Value::F64(1.5)), "1.5");
    assert_eq!(escape_sql_value(&Value::F64(100.0)), "100");
}

#[test]
fn test_escape_f64_nan_to_null() {
    assert_eq!(escape_sql_value(&Value::F64(f64::NAN)), "NULL");
}

#[test]
fn test_escape_f64_inf_to_null() {
    assert_eq!(escape_sql_value(&Value::F64(f64::INFINITY)), "NULL");
    assert_eq!(escape_sql_value(&Value::F64(f64::NEG_INFINITY)), "NULL");
}

#[test]
fn test_escape_text_no_quotes() {
    assert_eq!(escape_sql_value(&Value::Text("hello".into())), "'hello'");
}

#[test]
fn test_escape_text_with_single_quote() {
    assert_eq!(
        escape_sql_value(&Value::Text("it's a test".into())),
        "'it''s a test'"
    );
}

#[test]
fn test_escape_text_multiple_quotes() {
    assert_eq!(
        escape_sql_value(&Value::Text("it's O'Brien".into())),
        "'it''s O''Brien'"
    );
}

#[test]
fn test_escape_blob_empty() {
    assert_eq!(escape_sql_value(&Value::Blob(vec![])), "X''");
}

#[test]
fn test_escape_blob_bytes() {
    assert_eq!(
        escape_sql_value(&Value::Blob(vec![0xca, 0xfe, 0xba, 0xbe])),
        "X'cafebabe'"
    );
}

#[test]
fn test_escape_decimal() {
    assert_eq!(
        escape_sql_value(&Value::Decimal("9999.99".into())),
        "9999.99"
    );
}

#[test]
fn test_escape_json_plain() {
    assert_eq!(
        escape_sql_value(&Value::Json(r#"{"a":1}"#.into())),
        r#"'{"a":1}'"#
    );
}

#[test]
fn test_escape_json_with_single_quote() {
    assert_eq!(
        escape_sql_value(&Value::Json(r#"{"k":"it's"}"#.into())),
        r#"'{"k":"it''s"}'"#
    );
}

#[test]
fn test_escape_array_falls_back_to_null() {
    assert_eq!(
        escape_sql_value(&Value::Array(vec![Value::I64(1), Value::I64(2)])),
        "NULL"
    );
}

#[test]
fn test_escape_uuid_zero() {
    assert_eq!(
        escape_sql_value(&Value::Uuid(0)),
        "'00000000-0000-0000-0000-000000000000'"
    );
}

#[test]
fn test_escape_timestamp_epoch() {
    // 0 µs → 1970-01-01 00:00:00 UTC
    assert_eq!(
        escape_sql_value(&Value::Timestamp(0)),
        "TIMESTAMP '1970-01-01 00:00:00'"
    );
}

#[test]
fn test_escape_date_epoch() {
    // 0 days → 1970-01-01
    assert_eq!(escape_sql_value(&Value::Date(0)), "DATE '1970-01-01'");
}

#[test]
fn test_escape_date_known() {
    // 2024-01-15 is 19737 days after Unix epoch
    assert_eq!(escape_sql_value(&Value::Date(19737)), "DATE '2024-01-15'");
}

#[test]
fn test_escape_time_midnight() {
    assert_eq!(escape_sql_value(&Value::Time(0)), "TIME '00:00:00'");
}

#[test]
fn test_escape_time_known() {
    // 13:30:00 = 48_600_000_000 µs since midnight
    assert_eq!(
        escape_sql_value(&Value::Time(48_600_000_000_i64)),
        "TIME '13:30:00'"
    );
}

// ── bind_params unit tests ────────────────────────────────────────────────────

#[test]
fn test_bind_null() {
    assert_eq!(
        bind_params("SELECT $1", &[Value::Null]).unwrap(),
        "SELECT NULL"
    );
}

#[test]
fn test_bind_text_escaping() {
    assert_eq!(
        bind_params("SELECT $1", &[Value::Text("it's a test".into())]).unwrap(),
        "SELECT 'it''s a test'"
    );
}

#[test]
fn test_bind_multiple_params() {
    assert_eq!(
        bind_params("WHERE a=$1 AND b=$2", &[Value::I64(42), Value::Bool(true)]).unwrap(),
        "WHERE a=42 AND b=TRUE"
    );
}

#[test]
fn test_bind_out_of_bounds() {
    // $2 references params[1], but only one param given.
    let err = bind_params("$2", &[Value::I64(1)]).unwrap_err();
    match err {
        OxiSqlError::Parse(msg) => {
            assert!(
                msg.contains("$2") || msg.contains("out of range"),
                "unexpected error message: {msg}"
            );
        }
        other => panic!("expected OxiSqlError::Parse, got {other:?}"),
    }
}

#[test]
fn test_bind_dollar_dollar_escape() {
    assert_eq!(bind_params("price = $$1", &[]).unwrap(), "price = $1");
}

#[test]
fn test_bind_high_index_not_partial_matched() {
    // $10 must not be read as $1 + "0".
    let params: Vec<Value> = (1..=10).map(Value::I64).collect();
    assert_eq!(bind_params("col=$10", &params).unwrap(), "col=10");
}

#[test]
fn test_bind_no_placeholders() {
    assert_eq!(bind_params("SELECT 1 + 1", &[]).unwrap(), "SELECT 1 + 1");
}

#[test]
fn test_bind_same_placeholder_twice() {
    // Each $1 occurrence is replaced independently.
    assert_eq!(bind_params("$1 = $1", &[Value::I64(7)]).unwrap(), "7 = 7");
}

// ── integration test through EmbeddedConnection ───────────────────────────────

/// Verify that `bind_params` output is accepted by GlueSQL when submitted
/// through an `EmbeddedConnection`.
#[tokio::test]
async fn test_bind_used_in_query() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    // Create a table with several typed columns.
    conn.execute(
        "CREATE TABLE params_test (id INTEGER, label TEXT, flag BOOLEAN)",
        &[],
    )
    .await
    .expect("CREATE TABLE");

    // Build the INSERT SQL using bind_params directly and execute it.
    let insert_sql = bind_params(
        "INSERT INTO params_test VALUES ($1, $2, $3)",
        &[
            Value::I64(1),
            Value::Text("hello world".into()),
            Value::Bool(true),
        ],
    )
    .expect("bind_params should succeed");

    conn.execute(&insert_sql, &[])
        .await
        .expect("INSERT via bind_params should succeed");

    // Build the SELECT SQL with a WHERE clause via bind_params.
    let select_sql = bind_params(
        "SELECT id, label, flag FROM params_test WHERE id = $1",
        &[Value::I64(1)],
    )
    .expect("bind_params for SELECT should succeed");

    let rows = conn
        .query(&select_sql, &[])
        .await
        .expect("SELECT via bind_params should succeed");

    assert_eq!(rows.len(), 1, "expected exactly 1 row");
    let row = &rows[0];
    let id: i64 = row.try_get("id").expect("column 'id'");
    let label: String = row.try_get("label").expect("column 'label'");
    let flag: bool = row.try_get("flag").expect("column 'flag'");

    assert_eq!(id, 1);
    assert_eq!(label, "hello world");
    assert!(flag);
}

/// Verify that text values with single quotes are safely escaped
/// and do not cause SQL parse errors.
#[tokio::test]
async fn test_bind_text_with_quote_in_query() {
    let conn = EmbeddedConnection::open_memory().expect("open_memory should not fail");

    conn.execute("CREATE TABLE quoted_test (s TEXT)", &[])
        .await
        .expect("CREATE TABLE");

    let insert_sql = bind_params(
        "INSERT INTO quoted_test VALUES ($1)",
        &[Value::Text("O'Brien".into())],
    )
    .expect("bind_params");

    conn.execute(&insert_sql, &[])
        .await
        .expect("INSERT O'Brien");

    let rows = conn
        .query("SELECT s FROM quoted_test", &[])
        .await
        .expect("SELECT");

    assert_eq!(rows.len(), 1);
    let s: String = rows[0].try_get("s").expect("column 's'");
    assert_eq!(s, "O'Brien");
}
