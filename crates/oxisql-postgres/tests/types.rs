//! Tests for the `PgError` type and related error handling.

use oxisql_core::Value;
use oxisql_postgres::{OwnedParam, PgError};

/// `PgError::TypeConversion` must produce a non-empty `Display` string.
#[test]
fn pg_error_type_conversion_display() {
    let e = PgError::TypeConversion("unexpected boolean".to_string());
    let msg = e.to_string();
    assert!(!msg.is_empty(), "error message must not be empty");
    assert!(
        msg.contains("type conversion"),
        "message should mention 'type conversion', got: {msg}"
    );
}

/// `PgError::Tls` must produce a non-empty `Display` string.
#[test]
fn pg_error_tls_display() {
    let e = PgError::Tls("certificate validation failed".to_string());
    let msg = e.to_string();
    assert!(!msg.is_empty());
    assert!(
        msg.contains("TLS"),
        "message should mention 'TLS', got: {msg}"
    );
}

/// `PgError` must convert to `oxisql_core::OxiSqlError` without panicking.
#[test]
fn pg_error_converts_to_oxisql_error() {
    use oxisql_core::OxiSqlError;

    let pg_err = PgError::TypeConversion("bad int".to_string());
    let oxisql_err: OxiSqlError = pg_err.into();
    let msg = oxisql_err.to_string();
    assert!(!msg.is_empty());
}

/// `PgError::Tls` converts to `OxiSqlError::Other`.
#[test]
fn pg_tls_error_converts() {
    use oxisql_core::OxiSqlError;

    let pg_err = PgError::Tls("handshake timeout".to_string());
    let oxisql_err: OxiSqlError = pg_err.into();
    match oxisql_err {
        OxiSqlError::Other(msg) => assert!(msg.contains("handshake")),
        other => panic!("expected OxiSqlError::Other, got {other:?}"),
    }
}

// ── Wave 10 tests ─────────────────────────────────────────────────────────────

#[test]
fn pg_error_constraint_violation_display() {
    use oxisql_postgres::PgError;
    let e = PgError::ConstraintViolation {
        constraint: "users_email_key".into(),
        detail: "Key (email)=(test@example.com) already exists.".into(),
    };
    let s = format!("{e}");
    assert!(s.contains("users_email_key"), "got: {s}");
}

#[test]
fn pg_error_constraint_converts_to_oxisql() {
    use oxisql_core::OxiSqlError;
    use oxisql_postgres::PgError;
    let e = PgError::ConstraintViolation {
        constraint: "pk_orders".into(),
        detail: "Duplicate key value violates unique constraint".into(),
    };
    let oe: OxiSqlError = e.into();
    // Should be ConstraintViolation if that variant exists, else Other
    assert!(matches!(
        oe,
        OxiSqlError::ConstraintViolation(_) | OxiSqlError::Other(_)
    ));
}

#[test]
fn pg_timeout_display() {
    use oxisql_postgres::PgError;
    let e = PgError::Timeout("30s".into());
    assert!(format!("{e}").contains("timeout") || format!("{e}").contains("30s"));
}

#[test]
fn pg_connection_builder_constructs() {
    use oxisql_postgres::PgConnectionBuilder;
    let builder = PgConnectionBuilder::new()
        .host("localhost")
        .port(5432)
        .dbname("testdb")
        .user("testuser");
    let _ = builder; // Just verify it constructs without panic
}

// ── value_to_param — pure CPU, no live server required ────────────────────────

/// `Value::Null` encodes to `OwnedParam::Null`.
#[test]
fn value_to_param_null() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Null);
    assert!(matches!(p, OwnedParam::Null));
}

/// `Value::Bool(true)` encodes to `OwnedParam::Bool(true)`.
#[test]
fn value_to_param_bool_true() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Bool(true));
    assert!(matches!(p, OwnedParam::Bool(true)));
}

/// `Value::Bool(false)` encodes to `OwnedParam::Bool(false)`.
#[test]
fn value_to_param_bool_false() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Bool(false));
    assert!(matches!(p, OwnedParam::Bool(false)));
}

/// `Value::I64(42)` encodes to `OwnedParam::I64(42)`.
#[test]
fn value_to_param_i64() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::I64(42));
    assert!(matches!(p, OwnedParam::I64(42)));
}

/// `Value::I64(i64::MIN)` encodes to `OwnedParam::I64(i64::MIN)` (boundary).
#[test]
fn value_to_param_i64_min() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::I64(i64::MIN));
    assert!(matches!(p, OwnedParam::I64(i64::MIN)));
}

/// `Value::F64(1.5)` encodes to `OwnedParam::F64(1.5)`.
#[test]
fn value_to_param_f64() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::F64(1.5_f64));
    match p {
        OwnedParam::F64(v) => assert!((v - 1.5_f64).abs() < f64::EPSILON),
        other => panic!("expected OwnedParam::F64, got {other:?}"),
    }
}

/// `Value::Text("hello")` encodes to `OwnedParam::Text("hello")`.
#[test]
fn value_to_param_text() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Text("hello".to_string()));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, "hello"),
        other => panic!("expected OwnedParam::Text, got {other:?}"),
    }
}

/// `Value::Blob([1,2,3])` encodes to `OwnedParam::Blob([1,2,3])`.
#[test]
fn value_to_param_blob() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Blob(vec![1u8, 2, 3]));
    match p {
        OwnedParam::Blob(b) => assert_eq!(b, vec![1u8, 2, 3]),
        other => panic!("expected OwnedParam::Blob, got {other:?}"),
    }
}

/// `Value::Timestamp` encodes as `OwnedParam::Text` with "secs.microseconds" format.
#[test]
fn value_to_param_timestamp_is_text() {
    use oxisql_postgres::value_to_param;
    // 1_000_001 µs → secs=1, frac=1
    let p = value_to_param(&Value::Timestamp(1_000_001_i64));
    match p {
        OwnedParam::Text(s) => {
            // format is "{secs}.{frac:06}" → "1.000001"
            assert_eq!(s, "1.000001", "unexpected timestamp encoding: {s}");
        }
        other => panic!("expected OwnedParam::Text for Timestamp, got {other:?}"),
    }
}

/// `Value::Date(0)` (Unix epoch 1970-01-01) encodes as `OwnedParam::Text("0")`.
#[test]
fn value_to_param_date_is_text() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Date(0_i32));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, "0"),
        other => panic!("expected OwnedParam::Text for Date, got {other:?}"),
    }
}

/// `Value::Json(...)` encodes as `OwnedParam::Text` with the raw JSON string.
#[test]
fn value_to_param_json_is_text() {
    use oxisql_postgres::value_to_param;
    let json = r#"{"key":"val"}"#.to_string();
    let p = value_to_param(&Value::Json(json.clone()));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, json),
        other => panic!("expected OwnedParam::Text for Json, got {other:?}"),
    }
}

/// `Value::Decimal("123.456")` encodes as `OwnedParam::Text("123.456")`.
#[test]
fn value_to_param_decimal_is_text() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Decimal("123.456".to_string()));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, "123.456"),
        other => panic!("expected OwnedParam::Text for Decimal, got {other:?}"),
    }
}

/// An empty `Value::Array` encodes as `OwnedParam::Text("{}")`.
#[test]
fn value_to_param_empty_array_is_text() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Array(vec![]));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, "{}"),
        other => panic!("expected OwnedParam::Text for Array, got {other:?}"),
    }
}

/// A `Value::Array([I64(1), I64(2)])` encodes as `OwnedParam::Text("{1,2}")`.
#[test]
fn value_to_param_int_array_is_text() {
    use oxisql_postgres::value_to_param;
    let p = value_to_param(&Value::Array(vec![Value::I64(1), Value::I64(2)]));
    match p {
        OwnedParam::Text(s) => assert_eq!(s, "{1,2}"),
        other => panic!("expected OwnedParam::Text for Array, got {other:?}"),
    }
}

// ── PgError — untested variants ───────────────────────────────────────────────

/// `PgError::PoolExhausted` must produce a non-empty `Display` string.
#[test]
fn pg_error_pool_exhausted_display() {
    let e = PgError::PoolExhausted("all 10 connections busy".to_string());
    let s = e.to_string();
    assert!(!s.is_empty(), "PoolExhausted display must be non-empty");
    assert!(
        s.contains("pool exhausted") || s.contains("all 10"),
        "unexpected PoolExhausted message: {s}"
    );
}

/// `PgError::Copy` must produce a non-empty `Display` string.
#[test]
fn pg_error_copy_display() {
    let e = PgError::Copy("unexpected CopyDone message".to_string());
    let s = e.to_string();
    assert!(!s.is_empty(), "Copy display must be non-empty");
    assert!(s.contains("COPY"), "expected 'COPY' in message, got: {s}");
}

/// `PgError::Notify` must produce a non-empty `Display` string.
#[test]
fn pg_error_notify_display() {
    let e = PgError::Notify("invalid channel name".to_string());
    let s = e.to_string();
    assert!(!s.is_empty(), "Notify display must be non-empty");
    assert!(
        s.contains("NOTIFY"),
        "expected 'NOTIFY' in message, got: {s}"
    );
}

/// `PgError::PoolExhausted` converts to `OxiSqlError::ConnectionPool`.
#[test]
fn pg_error_pool_exhausted_converts() {
    use oxisql_core::OxiSqlError;
    let e = PgError::PoolExhausted("pool at limit".to_string());
    let oe: OxiSqlError = e.into();
    assert!(
        matches!(oe, OxiSqlError::ConnectionPool(_)),
        "expected ConnectionPool variant, got {oe:?}"
    );
}

/// `TlsMode::skip_verify()` constructs successfully and yields the `Rustls` arm.
#[test]
fn tls_mode_skip_verify_constructs() {
    use oxisql_postgres::TlsMode;
    let result = TlsMode::skip_verify();
    assert!(result.is_ok(), "skip_verify() should not fail");
    match result.unwrap() {
        TlsMode::Rustls(_) => {} // expected
        TlsMode::Disabled => panic!("expected TlsMode::Rustls from skip_verify"),
    }
}

// ── Integration tests: ARRAY and INTERVAL extraction ─────────────────────────

#[cfg(feature = "integration-postgres")]
mod pg_array_interval_integration {
    use oxisql_core::{Connection, Value};
    use oxisql_postgres::{PgConnection, TlsMode};

    const CONN_STR: &str = "host=localhost port=5432 user=postgres password=test dbname=postgres";

    async fn connect() -> PgConnection {
        PgConnection::connect(CONN_STR, TlsMode::Disabled)
            .await
            .expect("connect")
    }

    /// `SELECT ARRAY[1, 2, 3]::int[]` → `Value::Array([I64(1), I64(2), I64(3)])`
    #[tokio::test]
    #[ignore]
    async fn test_int_array() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT ARRAY[1, 2, 3]::int[] AS arr", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("arr").expect("column arr");
        assert_eq!(
            *v,
            Value::Array(vec![Value::I64(1), Value::I64(2), Value::I64(3)])
        );
    }

    /// `SELECT ARRAY['a', 'b']::text[]` → `Value::Array([Text("a"), Text("b")])`
    #[tokio::test]
    #[ignore]
    async fn test_text_array() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT ARRAY['a', 'b']::text[] AS arr", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("arr").expect("column arr");
        assert_eq!(
            *v,
            Value::Array(vec![
                Value::Text("a".to_string()),
                Value::Text("b".to_string())
            ])
        );
    }

    /// `SELECT ARRAY[1.5, 2.5]::float8[]` → `Value::Array([F64(1.5), F64(2.5)])`
    #[tokio::test]
    #[ignore]
    async fn test_float_array() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT ARRAY[1.5, 2.5]::float8[] AS arr", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("arr").expect("column arr");
        assert_eq!(*v, Value::Array(vec![Value::F64(1.5), Value::F64(2.5)]));
    }

    /// `SELECT INTERVAL '1 hour 30 minutes'` → some `Value::Text` containing "01:30:00"
    #[tokio::test]
    #[ignore]
    async fn test_interval() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT INTERVAL '1 hour 30 minutes' AS iv", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("iv").expect("column iv");
        match v {
            Value::Text(s) => {
                assert!(
                    s.contains("01:30:00"),
                    "expected '01:30:00' in interval string, got: {s}"
                );
            }
            other => panic!("expected Value::Text for INTERVAL, got {other:?}"),
        }
    }

    /// `SELECT NULL::int[]` → `Value::Null`
    #[tokio::test]
    #[ignore]
    async fn test_null_array() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT NULL::int[] AS arr", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("arr").expect("column arr");
        assert_eq!(*v, Value::Null);
    }

    /// `SELECT ARRAY[1, NULL, 3]::int[]` → `Value::Array([I64(1), Null, I64(3)])`
    #[tokio::test]
    #[ignore]
    async fn test_array_with_null_element() {
        let conn = connect().await;
        let rows = conn
            .query("SELECT ARRAY[1, NULL, 3]::int[] AS arr", &[])
            .await
            .expect("query");
        assert_eq!(rows.len(), 1);
        let v = rows[0].get("arr").expect("column arr");
        assert_eq!(
            *v,
            Value::Array(vec![Value::I64(1), Value::Null, Value::I64(3)])
        );
    }
}
