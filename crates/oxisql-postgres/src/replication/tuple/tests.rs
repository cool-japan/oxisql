use super::super::pgoutput::ReplicaIdentity;
use super::*;
use bytes::{Bytes, BytesMut};
use tokio_postgres::types::ToSql;

// ── Fixture helpers ──────────────────────────────────────────────────────

fn col(name: &str, type_oid: u32) -> ColumnSpec {
    ColumnSpec {
        key: false,
        name: name.to_string(),
        type_oid,
        type_modifier: -1,
    }
}

fn text(s: &str) -> TupleColumn {
    TupleColumn::Text(s.to_string())
}

/// Wraps already-encoded raw binary-format bytes into a
/// [`TupleColumn::Binary`] cell — analogous to [`text`], but for the
/// binary wire format.
fn bin(raw: &[u8]) -> TupleColumn {
    TupleColumn::Binary(Bytes::copy_from_slice(raw))
}

/// Encodes `value` via its real `ToSql` binary implementation, for
/// building binary-format test fixtures without hand-rolling every wire
/// format `tokio_postgres` already knows how to write. `.expect(...)` is
/// safe here: every value this helper is called with in this test suite
/// is a well-formed Rust value for `ty`, which `ToSql` impls for these
/// built-in types never reject.
fn to_sql_bytes<T: ToSql>(ty: &Type, value: &T) -> Vec<u8> {
    let mut buf = BytesMut::new();
    value
        .to_sql(ty, &mut buf)
        .expect("encoding a well-formed test fixture value should never fail");
    buf.to_vec()
}

/// Hand-builds a `NUMERIC` binary payload matching the wire format
/// [`decode_binary_numeric`] documents: a 4-field big-endian header
/// (`ndigits` derived from `digits.len()`, `weight`, `sign`, `dscale`)
/// followed by `digits`, each written as a big-endian `u16`.
fn build_numeric(weight: i16, sign: u16, dscale: u16, digits: &[u16]) -> Vec<u8> {
    let mut buf = Vec::new();
    let ndigits = i16::try_from(digits.len()).expect("test fixture digit count should fit in i16");
    buf.extend_from_slice(&ndigits.to_be_bytes());
    buf.extend_from_slice(&weight.to_be_bytes());
    buf.extend_from_slice(&sign.to_be_bytes());
    buf.extend_from_slice(&dscale.to_be_bytes());
    for d in digits {
        buf.extend_from_slice(&d.to_be_bytes());
    }
    buf
}

/// Hand-builds an `INTERVAL` binary payload matching the wire format
/// [`decode_binary_interval`] documents: big-endian `i64` microseconds,
/// `i32` days, `i32` months.
fn build_interval(micros: i64, days: i32, months: i32) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&micros.to_be_bytes());
    buf.extend_from_slice(&days.to_be_bytes());
    buf.extend_from_slice(&months.to_be_bytes());
    buf
}

// ── Bool ─────────────────────────────────────────────────────────────────

#[test]
fn bool_true() {
    assert_eq!(
        text_to_value(Type::BOOL.oid(), "t").unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn bool_false() {
    assert_eq!(
        text_to_value(Type::BOOL.oid(), "f").unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn bool_invalid() {
    let err = text_to_value(Type::BOOL.oid(), "yes").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Integers ─────────────────────────────────────────────────────────────

#[test]
fn int2_positive() {
    assert_eq!(
        text_to_value(Type::INT2.oid(), "1234").unwrap(),
        Value::I64(1234)
    );
}

#[test]
fn int2_negative() {
    assert_eq!(
        text_to_value(Type::INT2.oid(), "-1234").unwrap(),
        Value::I64(-1234)
    );
}

#[test]
fn int2_overflow_rejected() {
    let err = text_to_value(Type::INT2.oid(), "40000").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn int4_positive() {
    assert_eq!(
        text_to_value(Type::INT4.oid(), "70000").unwrap(),
        Value::I64(70000)
    );
}

#[test]
fn int4_negative() {
    assert_eq!(
        text_to_value(Type::INT4.oid(), "-70000").unwrap(),
        Value::I64(-70000)
    );
}

#[test]
fn int4_overflow_rejected() {
    let err = text_to_value(Type::INT4.oid(), "5000000000").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn int8_positive() {
    assert_eq!(
        text_to_value(Type::INT8.oid(), "9223372036854775807").unwrap(),
        Value::I64(i64::MAX)
    );
}

#[test]
fn int8_negative() {
    assert_eq!(
        text_to_value(Type::INT8.oid(), "-42").unwrap(),
        Value::I64(-42)
    );
}

#[test]
fn int8_overflow_rejected() {
    let err = text_to_value(Type::INT8.oid(), "99999999999999999999").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Floats ───────────────────────────────────────────────────────────────

#[test]
fn float4_normal() {
    let Value::F64(f) = text_to_value(Type::FLOAT4.oid(), "3.5").unwrap() else {
        panic!("expected F64");
    };
    assert!((f - 3.5).abs() < f64::EPSILON);
}

#[test]
fn float8_normal() {
    let Value::F64(f) = text_to_value(Type::FLOAT8.oid(), "123.456789").unwrap() else {
        panic!("expected F64");
    };
    assert!((f - 123.456_789).abs() < 1e-9);
}

#[test]
fn float8_infinity() {
    assert!("Infinity".parse::<f64>().is_ok());
    assert_eq!(
        text_to_value(Type::FLOAT8.oid(), "Infinity").unwrap(),
        Value::F64(f64::INFINITY)
    );
}

#[test]
fn float8_negative_infinity() {
    assert!("-Infinity".parse::<f64>().is_ok());
    assert_eq!(
        text_to_value(Type::FLOAT8.oid(), "-Infinity").unwrap(),
        Value::F64(f64::NEG_INFINITY)
    );
}

#[test]
fn float8_nan() {
    assert!("NaN".parse::<f64>().is_ok());
    let Value::F64(f) = text_to_value(Type::FLOAT8.oid(), "NaN").unwrap() else {
        panic!("expected F64");
    };
    assert!(f.is_nan());
}

#[test]
fn float8_malformed() {
    let err = text_to_value(Type::FLOAT8.oid(), "not-a-number").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Text / Varchar / Bpchar / Name ──────────────────────────────────────

#[test]
fn text_passthrough() {
    assert_eq!(
        text_to_value(Type::TEXT.oid(), "hello world").unwrap(),
        Value::Text("hello world".to_string())
    );
}

#[test]
fn text_passthrough_non_ascii() {
    let s = "héllo wörld 日本語";
    assert_eq!(
        text_to_value(Type::TEXT.oid(), s).unwrap(),
        Value::Text(s.to_string())
    );
}

#[test]
fn varchar_passthrough() {
    assert_eq!(
        text_to_value(Type::VARCHAR.oid(), "abc").unwrap(),
        Value::Text("abc".to_string())
    );
}

#[test]
fn bpchar_passthrough() {
    assert_eq!(
        text_to_value(Type::BPCHAR.oid(), "abc  ").unwrap(),
        Value::Text("abc  ".to_string())
    );
}

#[test]
fn name_passthrough() {
    assert_eq!(
        text_to_value(Type::NAME.oid(), "pg_class").unwrap(),
        Value::Text("pg_class".to_string())
    );
}

// ── Bytea ────────────────────────────────────────────────────────────────

#[test]
fn bytea_valid_hex() {
    assert_eq!(
        text_to_value(Type::BYTEA.oid(), "\\xdeadbeef").unwrap(),
        Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])
    );
}

#[test]
fn bytea_empty_payload() {
    assert_eq!(
        text_to_value(Type::BYTEA.oid(), "\\x").unwrap(),
        Value::Blob(vec![])
    );
}

#[test]
fn bytea_missing_prefix() {
    let err = text_to_value(Type::BYTEA.oid(), "deadbeef").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn bytea_odd_length() {
    let err = text_to_value(Type::BYTEA.oid(), "\\xabc").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn bytea_invalid_hex_char() {
    let err = text_to_value(Type::BYTEA.oid(), "\\xzz").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Date ─────────────────────────────────────────────────────────────────

#[test]
fn date_unix_epoch() {
    assert_eq!(
        text_to_value(Type::DATE.oid(), "1970-01-01").unwrap(),
        Value::Date(0)
    );
}

#[test]
fn date_y2k() {
    assert_eq!(
        text_to_value(Type::DATE.oid(), "2000-01-01").unwrap(),
        Value::Date(10_957)
    );
}

#[test]
fn date_ordinary_cross_checked() {
    let expected = Date::from_calendar_date(2026, Month::July, 11)
        .unwrap()
        .to_julian_day()
        - UNIX_EPOCH_JDN;
    assert_eq!(
        text_to_value(Type::DATE.oid(), "2026-07-11").unwrap(),
        Value::Date(expected)
    );
}

#[test]
fn date_malformed() {
    let err = text_to_value(Type::DATE.oid(), "not-a-date").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Timestamp (no tz) ────────────────────────────────────────────────────

#[test]
fn timestamp_whole_seconds() {
    assert_eq!(
        text_to_value(Type::TIMESTAMP.oid(), "1970-01-01 00:00:00").unwrap(),
        Value::Timestamp(0)
    );
}

#[test]
fn timestamp_fractional_seconds() {
    assert_eq!(
        text_to_value(Type::TIMESTAMP.oid(), "1970-01-01 00:00:00.5").unwrap(),
        Value::Timestamp(500_000)
    );
}

#[test]
fn timestamp_full_microsecond_precision() {
    assert_eq!(
        text_to_value(Type::TIMESTAMP.oid(), "1970-01-01 00:00:01.123456").unwrap(),
        Value::Timestamp(1_123_456)
    );
}

#[test]
fn timestamp_malformed_missing_space() {
    let err = text_to_value(Type::TIMESTAMP.oid(), "1970-01-01T00:00:00").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── TimestampTz ──────────────────────────────────────────────────────────
//
// Offset-format coverage: this module handles all three widths
// PostgreSQL's ISO DateStyle can emit for a TIMESTAMPTZ offset suffix —
// bare `±HH`, `±HH:MM`, and `±HH:MM:SS` — each exercised below.

#[test]
fn timestamptz_whole_seconds_utc() {
    assert_eq!(
        text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 00:00:00+00").unwrap(),
        Value::Timestamp(0)
    );
}

#[test]
fn timestamptz_fractional_seconds_utc() {
    assert_eq!(
        text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 00:00:00.25+00").unwrap(),
        Value::Timestamp(250_000)
    );
}

#[test]
fn timestamptz_positive_hh_mm_offset() {
    assert_eq!(
        text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 00:00:00+05:30").unwrap(),
        Value::Timestamp(-19_800_000_000)
    );
}

#[test]
fn timestamptz_negative_hh_offset() {
    assert_eq!(
        text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 00:00:00-05").unwrap(),
        Value::Timestamp(18_000_000_000)
    );
}

#[test]
fn timestamptz_hh_mm_ss_offset() {
    assert_eq!(
        text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 05:30:45+05:30:45").unwrap(),
        Value::Timestamp(0)
    );
}

#[test]
fn timestamptz_missing_offset() {
    let err = text_to_value(Type::TIMESTAMPTZ.oid(), "1970-01-01 00:00:00").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Time (no tz) ─────────────────────────────────────────────────────────

#[test]
fn time_whole_seconds() {
    assert_eq!(
        text_to_value(Type::TIME.oid(), "01:02:03").unwrap(),
        Value::Time(3_723_000_000)
    );
}

#[test]
fn time_fractional_seconds() {
    assert_eq!(
        text_to_value(Type::TIME.oid(), "00:00:00.5").unwrap(),
        Value::Time(500_000)
    );
}

#[test]
fn time_malformed() {
    let err = text_to_value(Type::TIME.oid(), "not-a-time").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Uuid ─────────────────────────────────────────────────────────────────

#[test]
fn uuid_valid() {
    let s = "550e8400-e29b-41d4-a716-446655440000";
    let expected = uuid::Uuid::parse_str(s).unwrap().as_u128();
    assert_eq!(
        text_to_value(Type::UUID.oid(), s).unwrap(),
        Value::Uuid(expected)
    );
}

#[test]
fn uuid_malformed() {
    let err = text_to_value(Type::UUID.oid(), "not-a-uuid").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Json / Jsonb ─────────────────────────────────────────────────────────

#[test]
fn json_passthrough() {
    assert_eq!(
        text_to_value(Type::JSON.oid(), r#"{"a":1}"#).unwrap(),
        Value::Json(r#"{"a":1}"#.to_string())
    );
}

#[test]
fn jsonb_passthrough() {
    assert_eq!(
        text_to_value(Type::JSONB.oid(), r#"{"b":2}"#).unwrap(),
        Value::Json(r#"{"b":2}"#.to_string())
    );
}

// ── Numeric ──────────────────────────────────────────────────────────────

#[test]
fn numeric_passthrough_positive() {
    assert_eq!(
        text_to_value(Type::NUMERIC.oid(), "123.450").unwrap(),
        Value::Decimal("123.450".to_string())
    );
}

#[test]
fn numeric_passthrough_negative() {
    assert_eq!(
        text_to_value(Type::NUMERIC.oid(), "-99.99").unwrap(),
        Value::Decimal("-99.99".to_string())
    );
}

#[test]
fn numeric_passthrough_nan() {
    assert_eq!(
        text_to_value(Type::NUMERIC.oid(), "NaN").unwrap(),
        Value::Decimal("NaN".to_string())
    );
}

// ── Interval ─────────────────────────────────────────────────────────────

#[test]
fn interval_passthrough() {
    let s = "1 year 2 mons 3 days 01:30:00";
    assert_eq!(
        text_to_value(Type::INTERVAL.oid(), s).unwrap(),
        Value::Text(s.to_string())
    );
}

// ── Binary scalars ───────────────────────────────────────────────────────

#[test]
fn binary_bool_true() {
    let raw = to_sql_bytes(&Type::BOOL, &true);
    assert_eq!(
        binary_to_value(Type::BOOL.oid(), &raw).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn binary_bool_false() {
    let raw = to_sql_bytes(&Type::BOOL, &false);
    assert_eq!(
        binary_to_value(Type::BOOL.oid(), &raw).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn binary_int2() {
    let raw = to_sql_bytes(&Type::INT2, &1234i16);
    assert_eq!(
        binary_to_value(Type::INT2.oid(), &raw).unwrap(),
        Value::I64(1234)
    );
}

#[test]
fn binary_int2_negative() {
    let raw = to_sql_bytes(&Type::INT2, &(-1234i16));
    assert_eq!(
        binary_to_value(Type::INT2.oid(), &raw).unwrap(),
        Value::I64(-1234)
    );
}

#[test]
fn binary_int4() {
    let raw = to_sql_bytes(&Type::INT4, &70_000i32);
    assert_eq!(
        binary_to_value(Type::INT4.oid(), &raw).unwrap(),
        Value::I64(70_000)
    );
}

#[test]
fn binary_int8() {
    let raw = to_sql_bytes(&Type::INT8, &i64::MAX);
    assert_eq!(
        binary_to_value(Type::INT8.oid(), &raw).unwrap(),
        Value::I64(i64::MAX)
    );
}

#[test]
fn binary_float4() {
    let raw = to_sql_bytes(&Type::FLOAT4, &3.5f32);
    let Value::F64(f) = binary_to_value(Type::FLOAT4.oid(), &raw).unwrap() else {
        panic!("expected F64");
    };
    assert!((f - 3.5).abs() < f64::EPSILON);
}

#[test]
fn binary_float8() {
    let raw = to_sql_bytes(&Type::FLOAT8, &123.456_789f64);
    let Value::F64(f) = binary_to_value(Type::FLOAT8.oid(), &raw).unwrap() else {
        panic!("expected F64");
    };
    assert!((f - 123.456_789).abs() < 1e-9);
}

#[test]
fn binary_text() {
    let raw = to_sql_bytes(&Type::TEXT, &"hello world".to_string());
    assert_eq!(
        binary_to_value(Type::TEXT.oid(), &raw).unwrap(),
        Value::Text("hello world".to_string())
    );
}

#[test]
fn binary_bytea() {
    let raw = to_sql_bytes(&Type::BYTEA, &vec![0xDEu8, 0xAD, 0xBE, 0xEF]);
    assert_eq!(
        binary_to_value(Type::BYTEA.oid(), &raw).unwrap(),
        Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])
    );
}

#[test]
fn binary_date() {
    let d = Date::from_calendar_date(2000, Month::January, 1).unwrap();
    let raw = to_sql_bytes(&Type::DATE, &d);
    assert_eq!(
        binary_to_value(Type::DATE.oid(), &raw).unwrap(),
        Value::Date(10_957)
    );
}

#[test]
fn binary_timestamp() {
    let date = Date::from_calendar_date(1970, Month::January, 1).unwrap();
    let dt = PrimitiveDateTime::new(date, Time::from_hms_micro(0, 0, 1, 123_456).unwrap());
    let raw = to_sql_bytes(&Type::TIMESTAMP, &dt);
    assert_eq!(
        binary_to_value(Type::TIMESTAMP.oid(), &raw).unwrap(),
        Value::Timestamp(1_123_456)
    );
}

#[test]
fn binary_timestamptz() {
    let date = Date::from_calendar_date(1970, Month::January, 1).unwrap();
    let dt = PrimitiveDateTime::new(date, Time::from_hms(0, 0, 0).unwrap())
        .assume_offset(UtcOffset::from_hms(5, 30, 0).unwrap());
    let raw = to_sql_bytes(&Type::TIMESTAMPTZ, &dt);
    assert_eq!(
        binary_to_value(Type::TIMESTAMPTZ.oid(), &raw).unwrap(),
        Value::Timestamp(-19_800_000_000)
    );
}

#[test]
fn binary_time() {
    let t = Time::from_hms(1, 2, 3).unwrap();
    let raw = to_sql_bytes(&Type::TIME, &t);
    assert_eq!(
        binary_to_value(Type::TIME.oid(), &raw).unwrap(),
        Value::Time(3_723_000_000)
    );
}

#[test]
fn binary_uuid() {
    let u = uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let raw = to_sql_bytes(&Type::UUID, &u);
    assert_eq!(
        binary_to_value(Type::UUID.oid(), &raw).unwrap(),
        Value::Uuid(u.as_u128())
    );
}

#[test]
fn binary_json() {
    let raw = br#"{"a":1}"#.to_vec();
    assert_eq!(
        binary_to_value(Type::JSON.oid(), &raw).unwrap(),
        Value::Json(r#"{"a":1}"#.to_string())
    );
}

#[test]
fn binary_jsonb() {
    let mut raw = vec![1u8];
    raw.extend_from_slice(br#"{"b":2}"#);
    assert_eq!(
        binary_to_value(Type::JSONB.oid(), &raw).unwrap(),
        Value::Json(r#"{"b":2}"#.to_string())
    );
}

#[test]
fn binary_jsonb_missing_version_byte() {
    let err = binary_to_value(Type::JSONB.oid(), &[]).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn binary_jsonb_wrong_version_byte() {
    let mut raw = vec![2u8];
    raw.extend_from_slice(b"{}");
    let err = binary_to_value(Type::JSONB.oid(), &raw).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn binary_numeric_positive_with_fraction() {
    let raw = build_numeric(0, 0x0000, 3, &[123, 4500]);
    assert_eq!(
        binary_to_value(Type::NUMERIC.oid(), &raw).unwrap(),
        Value::Decimal("123.450".to_string())
    );
}

#[test]
fn binary_numeric_negative_with_fraction() {
    let raw = build_numeric(0, 0x4000, 2, &[99, 9900]);
    assert_eq!(
        binary_to_value(Type::NUMERIC.oid(), &raw).unwrap(),
        Value::Decimal("-99.99".to_string())
    );
}

#[test]
fn binary_numeric_nan() {
    let raw = build_numeric(0, 0xC000, 0, &[]);
    assert_eq!(
        binary_to_value(Type::NUMERIC.oid(), &raw).unwrap(),
        Value::Decimal("NaN".to_string())
    );
}

#[test]
fn binary_numeric_multi_group_integer() {
    let raw = build_numeric(1, 0x0000, 0, &[1234, 5678]);
    assert_eq!(
        binary_to_value(Type::NUMERIC.oid(), &raw).unwrap(),
        Value::Decimal("12345678".to_string())
    );
}

#[test]
fn binary_numeric_pure_fraction() {
    let raw = build_numeric(-1, 0x0000, 1, &[5000]);
    assert_eq!(
        binary_to_value(Type::NUMERIC.oid(), &raw).unwrap(),
        Value::Decimal("0.5".to_string())
    );
}

#[test]
fn binary_numeric_out_of_range_digit_rejected() {
    let raw = build_numeric(0, 0x0000, 0, &[10_000]);
    let err = binary_to_value(Type::NUMERIC.oid(), &raw).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn binary_numeric_truncated() {
    let err = binary_to_value(Type::NUMERIC.oid(), &[0, 0]).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn binary_interval_full() {
    let raw = build_interval(90 * 60 * 1_000_000, 3, 14);
    assert_eq!(
        binary_to_value(Type::INTERVAL.oid(), &raw).unwrap(),
        Value::Text("1Y 2M 3D 01:30:00".to_string())
    );
}

#[test]
fn binary_interval_zero() {
    let raw = build_interval(0, 0, 0);
    assert_eq!(
        binary_to_value(Type::INTERVAL.oid(), &raw).unwrap(),
        Value::Text("00:00:00".to_string())
    );
}

#[test]
fn binary_interval_truncated() {
    let err = binary_to_value(Type::INTERVAL.oid(), &[0u8; 10]).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Binary arrays ────────────────────────────────────────────────────────

#[test]
fn binary_array_int4() {
    let raw = to_sql_bytes(&Type::INT4_ARRAY, &vec![1i32, 2, 3]);
    assert_eq!(
        binary_to_value(Type::INT4_ARRAY.oid(), &raw).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::I64(1), Value::I64(2), Value::I64(3)],
        }
    );
}

#[test]
fn binary_array_int4_empty() {
    let raw = to_sql_bytes(&Type::INT4_ARRAY, &Vec::<i32>::new());
    assert_eq!(
        binary_to_value(Type::INT4_ARRAY.oid(), &raw).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![],
        }
    );
}

#[test]
fn binary_array_text_with_null() {
    let raw = to_sql_bytes(
        &Type::TEXT_ARRAY,
        &vec![Some("a".to_string()), None, Some("c".to_string())],
    );
    assert_eq!(
        binary_to_value(Type::TEXT_ARRAY.oid(), &raw).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![
                Value::Text("a".to_string()),
                Value::Null,
                Value::Text("c".to_string()),
            ],
        }
    );
}

#[test]
fn binary_array_truncated() {
    let err = binary_to_value(Type::INT4_ARRAY.oid(), &[0u8; 4]).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Array: text-format decoding ──────────────────────────────────────────

#[test]
fn array_type_decoded_to_typed_array() {
    assert_eq!(
        text_to_value(Type::INT4_ARRAY.oid(), "{1,2,3}").unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::I64(1), Value::I64(2), Value::I64(3)],
        }
    );
}

#[test]
fn text_array_empty() {
    assert_eq!(
        text_to_value(Type::INT4_ARRAY.oid(), "{}").unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![],
        }
    );
}

#[test]
fn text_array_with_null() {
    assert_eq!(
        text_to_value(Type::INT4_ARRAY.oid(), "{NULL,2}").unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::Null, Value::I64(2)],
        }
    );
}

#[test]
fn text_array_null_is_case_insensitive_and_unquoted_only() {
    assert_eq!(
        text_to_value(Type::TEXT_ARRAY.oid(), r#"{null,"NULL",NuLl}"#).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![Value::Null, Value::Text("NULL".to_string()), Value::Null,],
        }
    );
}

#[test]
fn text_array_quoted_with_embedded_comma_and_escaped_quote() {
    let s = r#"{"a,b","c\"d"}"#;
    assert_eq!(
        text_to_value(Type::TEXT_ARRAY.oid(), s).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![
                Value::Text("a,b".to_string()),
                Value::Text("c\"d".to_string()),
            ],
        }
    );
}

#[test]
fn text_array_nested_2x2() {
    assert_eq!(
        text_to_value(Type::INT4_ARRAY.oid(), "{{1,2},{3,4}}").unwrap(),
        Value::Array(vec![
            Value::Array(vec![Value::I64(1), Value::I64(2)]),
            Value::Array(vec![Value::I64(3), Value::I64(4)]),
        ])
    );
}

#[test]
fn text_array_text_with_embedded_commas_and_quotes() {
    let s = r#"{"hello, world","she said \"hi\"","plain"}"#;
    assert_eq!(
        text_to_value(Type::TEXT_ARRAY.oid(), s).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![
                Value::Text("hello, world".to_string()),
                Value::Text("she said \"hi\"".to_string()),
                Value::Text("plain".to_string()),
            ],
        }
    );
}

#[test]
fn text_array_dimension_prefixed() {
    assert_eq!(
        text_to_value(Type::TEXT_ARRAY.oid(), "[2:4]={a,b,c}").unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![
                Value::Text("a".to_string()),
                Value::Text("b".to_string()),
                Value::Text("c".to_string()),
            ],
        }
    );
}

#[test]
fn text_array_multi_dimension_prefix() {
    assert_eq!(
        text_to_value(Type::INT4_ARRAY.oid(), "[1:2][1:2]={{1,2},{3,4}}").unwrap(),
        Value::Array(vec![
            Value::Array(vec![Value::I64(1), Value::I64(2)]),
            Value::Array(vec![Value::I64(3), Value::I64(4)]),
        ])
    );
}

#[test]
fn text_array_uuid() {
    let s1 = "550e8400-e29b-41d4-a716-446655440000";
    let s2 = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    let u1 = uuid::Uuid::parse_str(s1).unwrap().as_u128();
    let u2 = uuid::Uuid::parse_str(s2).unwrap().as_u128();
    assert_eq!(
        text_to_value(Type::UUID_ARRAY.oid(), &format!("{{{s1},{s2}}}")).unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Uuid,
            values: vec![Value::Uuid(u1), Value::Uuid(u2)],
        }
    );
}

#[test]
fn text_array_bool() {
    assert_eq!(
        text_to_value(Type::BOOL_ARRAY.oid(), "{t,f,t}").unwrap(),
        Value::TypedArray {
            element_type: ArrayElementType::Bool,
            values: vec![Value::Bool(true), Value::Bool(false), Value::Bool(true)],
        }
    );
}

// ── Array: malformed text / truncated binary error paths ────────────────

#[test]
fn text_array_unbalanced_brace_missing_close() {
    let err = text_to_value(Type::INT4_ARRAY.oid(), "{1,2,3").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn text_array_empty_input_rejected() {
    let err = text_to_value(Type::INT4_ARRAY.oid(), "").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn text_array_trailing_content_rejected() {
    let err = text_to_value(Type::INT4_ARRAY.oid(), "{1,2,3}garbage").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn text_array_unterminated_quote_rejected() {
    let err = text_to_value(Type::TEXT_ARRAY.oid(), "{\"abc}").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

#[test]
fn text_array_bareword_scalar_without_braces_rejected() {
    let err = text_to_value(Type::INT4_ARRAY.oid(), "1,2,3").unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── Unknown OID ──────────────────────────────────────────────────────────

#[test]
fn unknown_oid_falls_back_to_text() {
    let bogus_oid = 999_999_999;
    assert!(Type::from_oid(bogus_oid).is_none());
    assert_eq!(
        text_to_value(bogus_oid, "raw-value").unwrap(),
        Value::Text("raw-value".to_string())
    );
}

// ── TupleColumn dispatch: Null / UnchangedToast / Binary ────────────────

#[test]
fn cell_null_maps_to_value_null() {
    let c = col("x", Type::INT4.oid());
    assert_eq!(
        decode_cell(&c, &TupleColumn::Null).unwrap(),
        CellValue::Value(Value::Null)
    );
}

#[test]
fn cell_unchanged_toast_is_distinct_from_null() {
    let c = col("x", Type::INT4.oid());
    let null_cell = decode_cell(&c, &TupleColumn::Null).unwrap();
    let toast_cell = decode_cell(&c, &TupleColumn::UnchangedToast).unwrap();
    assert_eq!(toast_cell, CellValue::UnchangedToast);
    assert_ne!(null_cell, toast_cell);
}

#[test]
fn cell_binary_int4_decoded() {
    let c = col("x", Type::INT4.oid());
    let raw = to_sql_bytes(&Type::INT4, &42i32);
    assert_eq!(
        decode_cell(&c, &bin(&raw)).unwrap(),
        CellValue::Value(Value::I64(42))
    );
}

#[test]
fn cell_binary_truncated_is_type_conversion_error() {
    let c = col("x", Type::INT4.oid());
    let err = decode_cell(&c, &bin(&[1, 2, 3])).unwrap_err();
    assert!(matches!(err, PgError::TypeConversion(_)));
}

// ── tuple_to_values: structural behavior ─────────────────────────────────

#[test]
fn tuple_to_values_happy_path() {
    let rel = RelationBody {
        rel_id: 1,
        namespace: "public".to_string(),
        name: "users".to_string(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![
            col("id", Type::INT4.oid()),
            col("name", Type::TEXT.oid()),
            col("active", Type::BOOL.oid()),
        ],
    };
    let tuple = TupleData {
        columns: vec![text("42"), text("alice"), text("t")],
    };
    let values = tuple_to_values(&rel, &tuple).unwrap();
    assert_eq!(
        values,
        vec![
            CellValue::Value(Value::I64(42)),
            CellValue::Value(Value::Text("alice".to_string())),
            CellValue::Value(Value::Bool(true)),
        ]
    );
}

#[test]
fn tuple_to_values_mixed_null_and_toast() {
    let rel = RelationBody {
        rel_id: 1,
        namespace: "public".to_string(),
        name: "t".to_string(),
        replica_identity: ReplicaIdentity::Full,
        columns: vec![col("a", Type::TEXT.oid()), col("b", Type::BYTEA.oid())],
    };
    let tuple = TupleData {
        columns: vec![TupleColumn::Null, TupleColumn::UnchangedToast],
    };
    let values = tuple_to_values(&rel, &tuple).unwrap();
    assert_eq!(
        values,
        vec![CellValue::Value(Value::Null), CellValue::UnchangedToast]
    );
}

#[test]
fn tuple_to_values_column_count_mismatch() {
    let rel = RelationBody {
        rel_id: 7,
        namespace: "public".to_string(),
        name: "mismatch".to_string(),
        replica_identity: ReplicaIdentity::Default,
        columns: vec![col("a", Type::INT4.oid()), col("b", Type::TEXT.oid())],
    };
    let tuple = TupleData {
        columns: vec![text("1")],
    };
    let err = tuple_to_values(&rel, &tuple).unwrap_err();
    assert!(matches!(err, PgError::Protocol(_)));
}
