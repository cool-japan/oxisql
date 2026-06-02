//! Unit tests for the MySQL → OxiSQL type mapping.
//!
//! All tests use synthetic `mysql_async::Value` variants constructed directly;
//! no live MySQL server is required.

use chrono::NaiveDate;
use mysql_async::consts::ColumnType;
use oxisql_core::Value;
use oxisql_mysql::{mysql_url_parts, mysql_value_to_core, mysql_value_to_core_with_type};

// ── helpers ──────────────────────────────────────────────────────────────────

fn days_since_epoch(year: i32, month: u32, day: u32) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch is valid");
    let d = NaiveDate::from_ymd_opt(year, month, day).expect("valid date in test");
    i32::try_from(d.signed_duration_since(epoch).num_days()).expect("within i32 range in test")
}

fn timestamp_micros(year: i32, month: u32, day: u32, h: u32, m: u32, s: u32, us: u32) -> i64 {
    NaiveDate::from_ymd_opt(year, month, day)
        .and_then(|d| d.and_hms_nano_opt(h, m, s, us * 1_000))
        .expect("valid datetime in test")
        .and_utc()
        .timestamp_micros()
}

// ── scalar type tests ─────────────────────────────────────────────────────────

#[test]
fn null_maps_to_null() {
    let v = mysql_value_to_core(mysql_async::Value::NULL).expect("NULL");
    assert_eq!(v, Value::Null);
}

#[test]
fn int_maps_to_i64() {
    let v = mysql_value_to_core(mysql_async::Value::Int(42)).expect("Int");
    assert_eq!(v, Value::I64(42));
}

#[test]
fn int_negative_maps_to_i64() {
    let v = mysql_value_to_core(mysql_async::Value::Int(-100)).expect("Int negative");
    assert_eq!(v, Value::I64(-100));
}

#[test]
fn uint_small_maps_to_i64() {
    let v = mysql_value_to_core(mysql_async::Value::UInt(1000)).expect("UInt small");
    assert_eq!(v, Value::I64(1000));
}

#[test]
fn uint_max_i64_maps_to_i64() {
    let v = mysql_value_to_core(mysql_async::Value::UInt(i64::MAX as u64)).expect("UInt max i64");
    assert_eq!(v, Value::I64(i64::MAX));
}

#[test]
fn uint_overflow_returns_text_fallback() {
    let n = i64::MAX as u64 + 1;
    let v = mysql_value_to_core(mysql_async::Value::UInt(n)).expect("UInt overflow -> Text");
    assert_eq!(v, Value::Text(n.to_string()));
}

#[test]
fn float_maps_to_f64() {
    let v = mysql_value_to_core(mysql_async::Value::Float(1.5_f32)).expect("Float");
    match v {
        Value::F64(f) => {
            let diff = (f - f64::from(1.5_f32)).abs();
            assert!(diff < 1e-6, "Float widened incorrectly: {f}");
        }
        other => panic!("expected F64, got {other:?}"),
    }
}

#[test]
fn double_maps_to_f64() {
    // Use a non-standard constant to avoid clippy::approx_constant.
    let val: f64 = 1.234_567_89;
    let v = mysql_value_to_core(mysql_async::Value::Double(val)).expect("Double");
    assert_eq!(v, Value::F64(val));
}

#[test]
fn bytes_utf8_maps_to_text() {
    let v = mysql_value_to_core(mysql_async::Value::Bytes(b"hello".to_vec())).expect("Bytes UTF-8");
    assert_eq!(v, Value::Text("hello".to_string()));
}

#[test]
fn bytes_non_utf8_maps_to_blob() {
    let raw = vec![0xFF, 0xFE, 0xFD];
    let v = mysql_value_to_core(mysql_async::Value::Bytes(raw.clone())).expect("Bytes non-UTF-8");
    assert_eq!(v, Value::Blob(raw));
}

// ── Date / Datetime tests ─────────────────────────────────────────────────────

#[test]
fn date_only_maps_to_value_date() {
    // Year, month, day, hour, min, sec, microseconds
    let v =
        mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 0, 0, 0, 0)).expect("Date only");
    assert_eq!(v, Value::Date(days_since_epoch(2024, 3, 15)));
}

#[test]
fn unix_epoch_date_is_day_zero() {
    let v = mysql_value_to_core(mysql_async::Value::Date(1970, 1, 1, 0, 0, 0, 0))
        .expect("Unix epoch date");
    assert_eq!(v, Value::Date(0));
}

#[test]
fn pre_epoch_date_is_negative_days() {
    let v = mysql_value_to_core(mysql_async::Value::Date(1969, 12, 31, 0, 0, 0, 0))
        .expect("pre-epoch date");
    assert_eq!(v, Value::Date(-1));
}

#[test]
fn datetime_maps_to_value_timestamp() {
    let v = mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 10, 30, 55, 0))
        .expect("Datetime");
    let expected_us = timestamp_micros(2024, 3, 15, 10, 30, 55, 0);
    assert_eq!(v, Value::Timestamp(expected_us));
}

#[test]
fn datetime_with_micros_maps_to_timestamp() {
    let v = mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 10, 30, 55, 123_456))
        .expect("Datetime with micros");
    let expected_us = timestamp_micros(2024, 3, 15, 10, 30, 55, 123_456);
    assert_eq!(v, Value::Timestamp(expected_us));
}

// ── Time / interval tests ─────────────────────────────────────────────────────

#[test]
fn time_zero_maps_to_value_time_zero() {
    // neg, days, hours, mins, secs, micros
    let v = mysql_value_to_core(mysql_async::Value::Time(false, 0, 0, 0, 0, 0)).expect("Time zero");
    assert_eq!(v, Value::Time(0));
}

#[test]
fn time_simple_maps_to_microseconds() {
    // 01:02:03 = 3723 seconds = 3_723_000_000 µs
    let v =
        mysql_value_to_core(mysql_async::Value::Time(false, 0, 1, 2, 3, 0)).expect("simple time");
    let expected_us: i64 = (3600 + 2 * 60 + 3) * 1_000_000i64;
    assert_eq!(v, Value::Time(expected_us));
}

#[test]
fn time_with_days_maps_to_value_time() {
    // 1 day + 2 hours + 30 minutes = 26h30m = total microseconds
    let v = mysql_value_to_core(mysql_async::Value::Time(false, 1, 2, 30, 0, 0))
        .expect("Time with days");
    let expected_us: i64 = (86_400 + 2 * 3_600 + 30 * 60) * 1_000_000i64;
    assert_eq!(v, Value::Time(expected_us));
}

#[test]
fn time_negative_maps_to_negative_value_time() {
    let v =
        mysql_value_to_core(mysql_async::Value::Time(true, 0, 1, 0, 0, 0)).expect("Time negative");
    let expected_us: i64 = -3_600_000_000i64;
    assert_eq!(v, Value::Time(expected_us));
}

#[test]
fn time_with_micros_maps_correctly() {
    // 00:00:01.000042
    let v = mysql_value_to_core(mysql_async::Value::Time(false, 0, 0, 0, 1, 42))
        .expect("Time with micros");
    let expected_us: i64 = 1_000_000 + 42;
    assert_eq!(v, Value::Time(expected_us));
}

// ── Extended column type detection tests ────────────────────────────────────

#[test]
fn bytes_with_newdecimal_type_maps_to_decimal() {
    let v = mysql_async::Value::Bytes(b"123.45".to_vec());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_NEWDECIMAL)
        .expect("NEWDECIMAL -> Decimal");
    assert_eq!(result, Value::Decimal("123.45".to_string()));
}

#[test]
fn bytes_with_decimal_type_maps_to_decimal() {
    let v = mysql_async::Value::Bytes(b"9999.999".to_vec());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_DECIMAL)
        .expect("DECIMAL -> Decimal");
    assert_eq!(result, Value::Decimal("9999.999".to_string()));
}

#[test]
fn bytes_with_json_type_maps_to_json() {
    let json = br#"{"a":1,"b":"hello"}"#;
    let v = mysql_async::Value::Bytes(json.to_vec());
    let result =
        mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_JSON).expect("JSON -> Json");
    assert_eq!(result, Value::Json(r#"{"a":1,"b":"hello"}"#.to_string()));
}

#[test]
fn bytes_with_enum_type_maps_to_text() {
    let v = mysql_async::Value::Bytes(b"active".to_vec());
    let result =
        mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_ENUM).expect("ENUM -> Text");
    assert_eq!(result, Value::Text("active".to_string()));
}

#[test]
fn bytes_with_set_type_maps_to_text() {
    let v = mysql_async::Value::Bytes(b"read,write".to_vec());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_SET).expect("SET -> Text");
    assert_eq!(result, Value::Text("read,write".to_string()));
}

#[test]
fn bytes_with_blob_type_maps_to_blob() {
    let raw = vec![0x00, 0xFF, 0xAB, 0xCD];
    let v = mysql_async::Value::Bytes(raw.clone());
    let result =
        mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_BLOB).expect("BLOB -> Blob");
    assert_eq!(result, Value::Blob(raw));
}

#[test]
fn bytes_with_long_blob_type_maps_to_blob() {
    let raw = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let v = mysql_async::Value::Bytes(raw.clone());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_LONG_BLOB)
        .expect("LONG_BLOB -> Blob");
    assert_eq!(result, Value::Blob(raw));
}

#[test]
fn bytes_with_medium_blob_type_maps_to_blob() {
    let raw = vec![0x01, 0x02, 0x03];
    let v = mysql_async::Value::Bytes(raw.clone());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_MEDIUM_BLOB)
        .expect("MEDIUM_BLOB -> Blob");
    assert_eq!(result, Value::Blob(raw));
}

#[test]
fn bytes_with_tiny_blob_type_maps_to_blob() {
    let raw = vec![0xCA, 0xFE];
    let v = mysql_async::Value::Bytes(raw.clone());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_TINY_BLOB)
        .expect("TINY_BLOB -> Blob");
    assert_eq!(result, Value::Blob(raw));
}

#[test]
fn bytes_with_var_string_type_maps_to_text_when_utf8() {
    let v = mysql_async::Value::Bytes(b"hello world".to_vec());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_VAR_STRING)
        .expect("VAR_STRING UTF-8 -> Text");
    assert_eq!(result, Value::Text("hello world".to_string()));
}

#[test]
fn bytes_with_var_string_type_maps_to_blob_when_non_utf8() {
    let raw = vec![0xFF, 0xFE, 0xFD];
    let v = mysql_async::Value::Bytes(raw.clone());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_VAR_STRING)
        .expect("VAR_STRING non-UTF-8 -> Blob");
    assert_eq!(result, Value::Blob(raw));
}

#[test]
fn int_with_type_delegates_to_mysql_value_to_core() {
    // Non-Bytes variants should not be affected by column type.
    let v = mysql_async::Value::Int(-42);
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_LONG).expect("Int -> I64");
    assert_eq!(result, Value::I64(-42));
}

#[test]
fn null_with_type_maps_to_null() {
    let result =
        mysql_value_to_core_with_type(mysql_async::Value::NULL, ColumnType::MYSQL_TYPE_NEWDECIMAL)
            .expect("NULL -> Null");
    assert_eq!(result, Value::Null);
}

// ── URL parsing tests ────────────────────────────────────────────────────────

#[test]
fn mysql_url_parts_parses_full_url() {
    let parts = mysql_url_parts("mysql://user:pass@localhost:3306/mydb").expect("full URL parse");
    assert_eq!(parts.host, "localhost");
    assert_eq!(parts.port, 3306);
    assert_eq!(parts.dbname, Some("mydb".to_string()));
    assert_eq!(parts.user, Some("user".to_string()));
}

#[test]
fn mysql_url_parts_parses_custom_port() {
    let parts =
        mysql_url_parts("mysql://admin:secret@db.example.com:3307/shop").expect("custom port");
    assert_eq!(parts.host, "db.example.com");
    assert_eq!(parts.port, 3307);
    assert_eq!(parts.dbname, Some("shop".to_string()));
    assert_eq!(parts.user, Some("admin".to_string()));
}

#[test]
fn mysql_url_parts_no_dbname() {
    let parts = mysql_url_parts("mysql://root:@127.0.0.1:3306/").expect("empty dbname");
    assert_eq!(parts.host, "127.0.0.1");
    assert_eq!(parts.port, 3306);
    // Empty path produces None or empty string depending on mysql_async version.
    // Accept either.
    assert!(
        parts.dbname.is_none() || parts.dbname.as_deref() == Some(""),
        "unexpected dbname: {:?}",
        parts.dbname,
    );
}

#[test]
fn mysql_url_parts_invalid_url_returns_err() {
    let result = mysql_url_parts("not_a_url");
    assert!(result.is_err(), "expected Err for invalid URL");
}

// ── UInt boundary value tests via mysql_value_to_core_with_type ─────────────

/// Verify that i64::MAX (as a UInt) delegates correctly through
/// `mysql_value_to_core_with_type` and produces `Value::I64`.
///
/// `mysql_value_to_core_with_type` delegates non-Bytes variants to
/// `mysql_value_to_core`, so this exercises the delegation path.
#[test]
fn test_uint_boundary_i64_max_with_type() {
    let v = mysql_async::Value::UInt(i64::MAX as u64);
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_LONGLONG)
        .expect("UInt(i64::MAX) with type should not fail");
    match result {
        Value::I64(n) => assert_eq!(
            n,
            i64::MAX,
            "UInt(i64::MAX) must map to Value::I64(i64::MAX)"
        ),
        other => panic!("expected Value::I64, got {other:?}"),
    }
}

/// Verify that u64::MAX (> i64::MAX) falls back to `Value::Text` through
/// the `mysql_value_to_core_with_type` delegation path.
#[test]
fn test_uint_boundary_u64_max_with_type() {
    let v = mysql_async::Value::UInt(u64::MAX);
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_LONGLONG)
        .expect("UInt(u64::MAX) with type should not fail");
    match result {
        Value::Text(s) => assert_eq!(
            s,
            u64::MAX.to_string(),
            "u64::MAX should fall back to Value::Text containing the decimal string"
        ),
        other => panic!("expected Value::Text fallback, got {other:?}"),
    }
}

/// Verify that i64::MAX + 1 (the first value that overflows i64) also falls
/// back to `Value::Text`.
#[test]
fn test_uint_boundary_i64_max_plus_one_with_type() {
    let overflow = i64::MAX as u64 + 1;
    let v = mysql_async::Value::UInt(overflow);
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_LONGLONG)
        .expect("UInt(i64::MAX+1) with type should not fail");
    match result {
        Value::Text(s) => assert_eq!(
            s,
            overflow.to_string(),
            "i64::MAX+1 should fall back to Value::Text"
        ),
        other => panic!("expected Value::Text fallback, got {other:?}"),
    }
}

// ── GEOMETRY type mapping tests ──────────────────────────────────────────────

/// GEOMETRY columns carry WKB binary data.  They must map to `Value::Blob`
/// rather than `Value::Text` (since WKB is arbitrary binary, not UTF-8).
#[test]
fn bytes_with_geometry_type_maps_to_blob() {
    // Minimal WKB point: byte order (1) + type (1=point, LE u32) + x + y (f64s)
    let wkb = vec![
        0x01u8, // little-endian
        0x01, 0x00, 0x00, 0x00, // WKB type: Point
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x3E, 0x40, // x = 30.0
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x24, 0x40, // y = 10.0
    ];
    let v = mysql_async::Value::Bytes(wkb.clone());
    let result = mysql_value_to_core_with_type(v, ColumnType::MYSQL_TYPE_GEOMETRY)
        .expect("GEOMETRY -> Blob");
    assert_eq!(result, Value::Blob(wkb));
}

/// NULL GEOMETRY values should produce `Value::Null` (handled by the outer
/// NULL arm, not the Bytes arm).
#[test]
fn null_with_geometry_type_maps_to_null() {
    let result =
        mysql_value_to_core_with_type(mysql_async::Value::NULL, ColumnType::MYSQL_TYPE_GEOMETRY)
            .expect("NULL GEOMETRY -> Null");
    assert_eq!(result, Value::Null);
}

// ── Value round-trip tests ───────────────────────────────────────────────────

/// Verify that scalar `Value` variants survive a round-trip through
/// `core_value_to_mysql` → `mysql_value_to_core`.
///
/// The round-trip is lossy for extended types (Decimal, Json, Timestamp, …)
/// which are serialised as `Bytes` and come back as `Value::Text`.  The tests
/// below document the expected lossiness explicitly and assert non-null output.
#[test]
fn test_all_scalar_value_round_trips() {
    use oxisql_mysql::core_value_to_mysql;

    // (original, predicate description)
    struct Case {
        original: Value,
        check: fn(&Value, &Value),
    }

    fn check_i64(orig: &Value, rt: &Value) {
        if let (Value::I64(a), Value::I64(b)) = (orig, rt) {
            assert_eq!(a, b, "I64 round-trip mismatch");
        } else {
            panic!("expected I64/I64, got {orig:?} / {rt:?}");
        }
    }

    fn check_f64(orig: &Value, rt: &Value) {
        if let (Value::F64(a), Value::F64(b)) = (orig, rt) {
            assert!((a - b).abs() < 1e-10, "F64 round-trip mismatch: {a} vs {b}");
        } else {
            panic!("expected F64/F64, got {orig:?} / {rt:?}");
        }
    }

    fn check_bool(orig: &Value, rt: &Value) {
        // Bool is sent as Int(0/1) and comes back as I64.
        if let Value::Bool(b) = orig {
            if let Value::I64(n) = rt {
                assert_eq!(*b as i64, *n, "Bool -> I64 round-trip mismatch");
            } else {
                panic!("Bool should round-trip via I64, got {rt:?}");
            }
        }
    }

    fn check_text(orig: &Value, rt: &Value) {
        if let (Value::Text(a), Value::Text(b)) = (orig, rt) {
            assert_eq!(a, b, "Text round-trip mismatch");
        } else {
            panic!("expected Text/Text, got {orig:?} / {rt:?}");
        }
    }

    fn check_null(_orig: &Value, rt: &Value) {
        assert_eq!(rt, &Value::Null, "Null should round-trip as Null");
    }

    fn check_blob(orig: &Value, rt: &Value) {
        if let (Value::Blob(a), Value::Blob(b)) = (orig, rt) {
            assert_eq!(a, b, "Blob round-trip mismatch");
        } else {
            panic!("expected Blob/Blob, got {orig:?} / {rt:?}");
        }
    }

    fn check_extended_non_null(_orig: &Value, rt: &Value) {
        // Extended types (Decimal, Json, Timestamp, Date, Time, Uuid, Array)
        // are serialised as Bytes and come back as Text (or Blob for binary).
        // We just assert non-null here; the typed path tests cover fidelity.
        assert!(
            !matches!(rt, Value::Null),
            "Extended type should not round-trip to Null; got {rt:?}"
        );
    }

    let cases: Vec<Case> = vec![
        Case {
            original: Value::I64(42),
            check: check_i64,
        },
        Case {
            original: Value::I64(-1),
            check: check_i64,
        },
        Case {
            original: Value::I64(i64::MAX),
            check: check_i64,
        },
        Case {
            original: Value::F64(std::f64::consts::PI),
            check: check_f64,
        },
        Case {
            original: Value::F64(-0.0),
            check: check_f64,
        },
        Case {
            original: Value::Bool(true),
            check: check_bool,
        },
        Case {
            original: Value::Bool(false),
            check: check_bool,
        },
        Case {
            original: Value::Text("hello".into()),
            check: check_text,
        },
        Case {
            original: Value::Text(String::new()),
            check: check_text,
        },
        Case {
            original: Value::Text("with 'quotes'".into()),
            check: check_text,
        },
        Case {
            original: Value::Null,
            check: check_null,
        },
        Case {
            original: Value::Blob(vec![0, 1, 2, 255]),
            check: check_blob,
        },
        // Extended types — lossily become Text on the plain mysql_value_to_core path.
        Case {
            original: Value::Decimal("123.456".into()),
            check: check_extended_non_null,
        },
        Case {
            original: Value::Json("{\"key\": 1}".into()),
            check: check_extended_non_null,
        },
    ];

    for case in &cases {
        let mysql_val = core_value_to_mysql(&case.original);
        let round_tripped = mysql_value_to_core(mysql_val).expect("round-trip should not fail");
        (case.check)(&case.original, &round_tripped);
    }
}

/// Verify that the *typed* round-trip (`core_value_to_mysql` →
/// `mysql_value_to_core_with_type(..., NEWDECIMAL)`) restores
/// `Value::Decimal` faithfully.
#[test]
fn test_decimal_typed_round_trip() {
    use oxisql_mysql::core_value_to_mysql;

    let original = Value::Decimal("9876.54321".into());
    let mysql_val = core_value_to_mysql(&original);
    let result = mysql_value_to_core_with_type(mysql_val, ColumnType::MYSQL_TYPE_NEWDECIMAL)
        .expect("DECIMAL typed round-trip");
    assert_eq!(result, Value::Decimal("9876.54321".into()));
}

/// Verify that the *typed* round-trip restores `Value::Json` faithfully.
#[test]
fn test_json_typed_round_trip() {
    use oxisql_mysql::core_value_to_mysql;

    let original = Value::Json(r#"{"answer":42}"#.into());
    let mysql_val = core_value_to_mysql(&original);
    let result = mysql_value_to_core_with_type(mysql_val, ColumnType::MYSQL_TYPE_JSON)
        .expect("JSON typed round-trip");
    assert_eq!(result, Value::Json(r#"{"answer":42}"#.into()));
}

// ── Error mapping tests ───────────────────────────────────────────────────────

/// Verify that `MysqlError::ConstraintViolation` (struct variant) displays
/// non-empty output containing identifiable content.
#[test]
fn test_error_display_constraint_violation() {
    use oxisql_mysql::MysqlError;
    let err = MysqlError::ConstraintViolation {
        constraint: "23000".to_string(),
        message: "Duplicate entry 'x' for key 'PRIMARY'".to_string(),
    };
    let s = err.to_string();
    assert!(!s.is_empty(), "Display output must not be empty");
    assert!(
        s.contains("23000") || s.contains("Duplicate") || s.contains("constraint"),
        "Display output should contain constraint code or message: {s}"
    );
}

/// Verify that `MysqlError::ConnectionTimeout` and `MysqlError::PoolExhausted`
/// both produce non-empty `Display` output.
#[test]
fn test_error_display_timeout_and_pool() {
    use oxisql_mysql::MysqlError;
    let timeout_err = MysqlError::ConnectionTimeout("timed out after 5s".to_string());
    let pool_err = MysqlError::PoolExhausted("all 10 connections in use".to_string());

    let timeout_s = timeout_err.to_string();
    let pool_s = pool_err.to_string();

    assert!(
        !timeout_s.is_empty(),
        "ConnectionTimeout Display must not be empty"
    );
    assert!(
        !pool_s.is_empty(),
        "PoolExhausted Display must not be empty"
    );

    // Content checks
    assert!(
        timeout_s.contains("timeout") || timeout_s.contains("timed out"),
        "ConnectionTimeout display should reference timeout: {timeout_s}"
    );
    assert!(
        pool_s.contains("pool") || pool_s.contains("exhausted") || pool_s.contains("in use"),
        "PoolExhausted display should reference pool: {pool_s}"
    );
}

/// Verify that `classify_mysql_error` maps constraint-violation codes
/// (1062, 1216, 1217, 1451, 1452) to `MysqlError::ConstraintViolation`.
#[test]
fn test_classify_mysql_error_constraint_codes() {
    use oxisql_mysql::error::classify_mysql_error;
    use oxisql_mysql::MysqlError;

    let constraint_codes = [1062u16, 1216, 1217, 1451, 1452];
    for code in constraint_codes {
        let raw = mysql_async::Error::Server(mysql_async::ServerError {
            code,
            message: format!("constraint error code {code}"),
            state: "23000".to_string(),
        });
        let mapped = classify_mysql_error(raw);
        match mapped {
            MysqlError::ConstraintViolation { .. } => {}
            other => panic!("code {code} should map to ConstraintViolation, got {other:?}"),
        }
    }
}

/// Verify that `classify_mysql_error` maps non-constraint server errors
/// (e.g. 1064 syntax error) to `MysqlError::Query`.
#[test]
fn test_classify_mysql_error_non_constraint_maps_to_query() {
    use oxisql_mysql::error::classify_mysql_error;
    use oxisql_mysql::MysqlError;

    let raw = mysql_async::Error::Server(mysql_async::ServerError {
        code: 1064,
        message: "You have an error in your SQL syntax".to_string(),
        state: "42000".to_string(),
    });
    let mapped = classify_mysql_error(raw);
    match mapped {
        MysqlError::Query(_) => {}
        other => panic!("code 1064 should map to MysqlError::Query, got {other:?}"),
    }
}

// ── mysql_url_parts edge case tests ──────────────────────────────────────────

/// Verify that `mysql_url_parts` handles a URL with a URL-encoded `@` in
/// the password without panicking or returning garbage host data.
#[test]
fn test_mysql_url_parts_encoded_at_in_password() {
    // %40 = '@', which could confuse naive URL splitters.
    let result = mysql_url_parts("mysql://user:p%40ss@localhost:3306/mydb");
    // mysql_async may or may not decode this precisely; we just verify no panic
    // and that if Ok, the host is correct.
    if let Ok(parts) = result {
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, 3306);
    }
    // Err is also acceptable — mysql_async may reject the encoded URL.
}

/// Verify that `mysql_url_parts` handles a minimal URL (no user/pass/db).
#[test]
fn test_mysql_url_parts_minimal_url() {
    // mysql_async requires at least a hostname; bare host-only may fail.
    // We test the expected shape of the result.
    let result = mysql_url_parts("mysql://localhost/");
    if let Ok(parts) = result {
        assert_eq!(parts.host, "localhost");
        assert_eq!(parts.port, 3306, "default port must be 3306");
    }
    // Err is acceptable for minimal URLs depending on mysql_async version.
}
