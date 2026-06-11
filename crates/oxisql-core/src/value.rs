//! [`Value`] enum and its implementations.

use std::cmp::Ordering;
use std::fmt;

/// The nominal SQL element type of a [`Value::TypedArray`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ArrayElementType {
    /// `bool` element type.
    Bool,
    /// `int2` element type.
    Int2,
    /// `int4` element type.
    Int4,
    /// `int8` element type.
    Int8,
    /// `float4` element type.
    Float4,
    /// `float8` element type.
    Float8,
    /// `text` element type.
    Text,
    /// `bytea` element type.
    Bytea,
    /// `date` element type.
    Date,
    /// `time` element type.
    Time,
    /// `timestamp` element type.
    Timestamp,
    /// `timestamptz` element type.
    TimestampTz,
    /// `uuid` element type.
    Uuid,
    /// `json` element type.
    Json,
    /// `jsonb` element type.
    Jsonb,
    /// `decimal` element type.
    Decimal,
}

impl fmt::Display for ArrayElementType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Bool => "bool",
            Self::Int2 => "int2",
            Self::Int4 => "int4",
            Self::Int8 => "int8",
            Self::Float4 => "float4",
            Self::Float8 => "float8",
            Self::Text => "text",
            Self::Bytea => "bytea",
            Self::Date => "date",
            Self::Time => "time",
            Self::Timestamp => "timestamp",
            Self::TimestampTz => "timestamptz",
            Self::Uuid => "uuid",
            Self::Json => "json",
            Self::Jsonb => "jsonb",
            Self::Decimal => "decimal",
        };
        f.write_str(s)
    }
}

/// A single SQL value returned from or passed to a query.
///
/// Covers both the basic scalar types common to all databases and extended
/// types for dates, timestamps, UUIDs, JSON, exact decimals, and arrays.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// Boolean value.
    Bool(bool),
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit floating-point number.
    F64(f64),
    /// UTF-8 text string.
    Text(String),
    /// Raw binary data.
    Blob(Vec<u8>),

    // ── Extended types ──────────────────────────────────────────────────
    /// Unix timestamp with microsecond precision (microseconds since epoch,
    /// UTC).  Maps to SQL `TIMESTAMP` / `TIMESTAMPTZ`.
    Timestamp(i64),
    /// Date-only value as days since Unix epoch (1970-01-01).
    /// Maps to SQL `DATE`.
    Date(i32),
    /// Time-of-day value as microseconds since midnight.
    /// Maps to SQL `TIME`.
    Time(i64),
    /// UUID stored as a 128-bit unsigned integer.
    /// Maps to SQL `UUID`.
    Uuid(u128),
    /// JSON or JSONB data stored as a UTF-8 string.
    /// Maps to SQL `JSON` / `JSONB`.
    Json(String),
    /// Exact decimal stored as a string representation (e.g. `"123.456"`).
    /// Using a string avoids introducing a big-decimal dependency at the
    /// core level while preserving exact precision.
    /// Maps to SQL `NUMERIC` / `DECIMAL`.
    Decimal(String),
    /// Ordered array of values (e.g. Postgres `INTEGER[]`, `TEXT[]`).
    Array(Vec<Value>),
    /// Like [`Value::Array`] but also carries the nominal element type returned
    /// by the backend (e.g. PostgreSQL `int4[]` → `TypedArray { element_type: Int4, .. }`).
    TypedArray {
        /// The nominal SQL element type of this array.
        element_type: ArrayElementType,
        /// The array elements.
        values: Vec<Value>,
    },
}

impl Value {
    /// Returns the human-readable type name of this value variant.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Bool(_) => "Bool",
            Value::I64(_) => "I64",
            Value::F64(_) => "F64",
            Value::Text(_) => "Text",
            Value::Blob(_) => "Blob",
            Value::Timestamp(_) => "Timestamp",
            Value::Date(_) => "Date",
            Value::Time(_) => "Time",
            Value::Uuid(_) => "Uuid",
            Value::Json(_) => "Json",
            Value::Decimal(_) => "Decimal",
            Value::Array(_) => "Array",
            Value::TypedArray { .. } => "TypedArray",
        }
    }

    /// Returns `true` if this value is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::I64(n) => write!(f, "{n}"),
            Value::F64(n) => write!(f, "{n}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Blob(b) => write!(f, "<blob:{} bytes>", b.len()),
            Value::Timestamp(us) => {
                // Format as seconds.microseconds from epoch
                let secs = us / 1_000_000;
                let frac = (us % 1_000_000).unsigned_abs();
                write!(f, "{secs}.{frac:06}")
            }
            Value::Date(days) => write!(f, "{days}d"),
            Value::Time(us) => {
                let total_secs = us / 1_000_000;
                let hours = total_secs / 3600;
                let mins = (total_secs % 3600) / 60;
                let secs = total_secs % 60;
                let frac = (us % 1_000_000).unsigned_abs();
                if frac == 0 {
                    write!(f, "{hours:02}:{mins:02}:{secs:02}")
                } else {
                    write!(f, "{hours:02}:{mins:02}:{secs:02}.{frac:06}")
                }
            }
            Value::Uuid(u) => {
                // Format as standard UUID: 8-4-4-4-12
                let bytes = u.to_be_bytes();
                write!(
                    f,
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    bytes[0], bytes[1], bytes[2], bytes[3],
                    bytes[4], bytes[5],
                    bytes[6], bytes[7],
                    bytes[8], bytes[9],
                    bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
                )
            }
            Value::Json(s) => write!(f, "{s}"),
            Value::Decimal(s) => write!(f, "{s}"),
            Value::Array(vals) => {
                write!(f, "[")?;
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
            Value::TypedArray {
                element_type,
                values,
            } => {
                write!(f, "{element_type}[")?;
                for (i, v) in values.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::Null, _) => Some(Ordering::Less),
            (_, Value::Null) => Some(Ordering::Greater),
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            (Value::I64(a), Value::I64(b)) => a.partial_cmp(b),
            (Value::F64(a), Value::F64(b)) => a.partial_cmp(b),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Blob(a), Value::Blob(b)) => a.partial_cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.partial_cmp(b),
            (Value::Date(a), Value::Date(b)) => a.partial_cmp(b),
            (Value::Time(a), Value::Time(b)) => a.partial_cmp(b),
            (Value::Uuid(a), Value::Uuid(b)) => a.partial_cmp(b),
            (Value::Json(a), Value::Json(b)) => a.partial_cmp(b),
            (Value::Decimal(a), Value::Decimal(b)) => a.partial_cmp(b),
            (Value::TypedArray { values: a, .. }, Value::TypedArray { values: b, .. }) => {
                a.partial_cmp(b)
            }
            _ => None,
        }
    }
}

// ── From impls for ergonomic Value construction ─────────────────────────────

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::I64(i64::from(v))
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::I64(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}

// ── FromValue for chrono types (feature = "chrono") ─────────────────────────

#[cfg(feature = "chrono")]
mod chrono_impls {
    use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

    use crate::row::FromValue;
    use crate::{OxiSqlError, Value};

    // Days-since-epoch constant helpers
    // Value::Date(i32) → days since 1970-01-01 (Unix epoch)
    // Value::Time(i64) → microseconds since midnight
    // Value::Timestamp(i64) → microseconds since Unix epoch

    impl FromValue for NaiveDate {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Date(days) => {
                    NaiveDate::from_num_days_from_ce_opt(
                        // chrono epoch is 1 CE; Unix epoch (1970-01-01) is day 719_163 in CE days.
                        *days + 719_163,
                    )
                    .ok_or(OxiSqlError::TypeMismatch {
                        expected: "NaiveDate (valid days-since-epoch)",
                        got: "Date (out of range)",
                    })
                }
                Value::Text(s) => s
                    .parse::<NaiveDate>()
                    .map_err(|_| OxiSqlError::TypeMismatch {
                        expected: "NaiveDate (ISO 8601 text)",
                        got: "Text (not a valid date)",
                    }),
                Value::I64(n) => {
                    // Treat as days since Unix epoch
                    let days = i32::try_from(*n).map_err(|_| OxiSqlError::TypeMismatch {
                        expected: "NaiveDate (i64 days-since-epoch in i32 range)",
                        got: "I64 (out of i32 range)",
                    })?;
                    NaiveDate::from_num_days_from_ce_opt(days + 719_163).ok_or(
                        OxiSqlError::TypeMismatch {
                            expected: "NaiveDate (valid days-since-epoch)",
                            got: "I64 (out of range)",
                        },
                    )
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Date",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for NaiveTime {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Time(us) => {
                    // microseconds since midnight; split into secs + nano remainder
                    let total_secs = (*us / 1_000_000) as u32;
                    let nano = ((*us % 1_000_000) * 1_000) as u32;
                    let h = total_secs / 3600;
                    let m = (total_secs % 3600) / 60;
                    let s = total_secs % 60;
                    NaiveTime::from_hms_nano_opt(h, m, s, nano).ok_or(OxiSqlError::TypeMismatch {
                        expected: "NaiveTime (valid time-of-day)",
                        got: "Time (out of range)",
                    })
                }
                Value::Text(s) => s
                    .parse::<NaiveTime>()
                    .map_err(|_| OxiSqlError::TypeMismatch {
                        expected: "NaiveTime (ISO 8601 text)",
                        got: "Text (not a valid time)",
                    }),
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Time",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for NaiveDateTime {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Timestamp(us) => {
                    // microseconds since Unix epoch
                    let secs = us.div_euclid(1_000_000);
                    let nsecs = (us.rem_euclid(1_000_000) * 1_000) as u32;
                    DateTime::from_timestamp(secs, nsecs)
                        .map(|dt| dt.naive_utc())
                        .ok_or(OxiSqlError::TypeMismatch {
                            expected: "NaiveDateTime (valid timestamp)",
                            got: "Timestamp (out of range)",
                        })
                }
                Value::Text(s) => {
                    s.parse::<NaiveDateTime>()
                        .map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "NaiveDateTime (ISO 8601 text)",
                            got: "Text (not a valid datetime)",
                        })
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Timestamp",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for DateTime<Utc> {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Timestamp(us) => {
                    let secs = us.div_euclid(1_000_000);
                    let nsecs = (us.rem_euclid(1_000_000) * 1_000) as u32;
                    Utc.timestamp_opt(secs, nsecs)
                        .single()
                        .ok_or(OxiSqlError::TypeMismatch {
                            expected: "DateTime<Utc> (valid timestamp)",
                            got: "Timestamp (out of range or ambiguous)",
                        })
                }
                Value::Text(s) => {
                    s.parse::<DateTime<Utc>>()
                        .map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "DateTime<Utc> (RFC3339 text)",
                            got: "Text (not a valid datetime)",
                        })
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Timestamp",
                    got: other.type_name(),
                }),
            }
        }
    }

    #[cfg(test)]
    mod chrono_tests {
        use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};

        use crate::row::FromValue;
        use crate::{OxiSqlError, Value};

        #[test]
        fn roundtrip_naive_date() {
            // 1970-01-01 is day 0 (Unix epoch)
            let v = Value::Date(0);
            let d = NaiveDate::from_value(&v).expect("epoch date");
            assert_eq!(d, NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());

            // 1970-01-02 is day 1
            let v = Value::Date(1);
            let d = NaiveDate::from_value(&v).expect("day 1");
            assert_eq!(d, NaiveDate::from_ymd_opt(1970, 1, 2).unwrap());

            // 2024-03-15 — validate round-trip through i32
            let expected = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap();
            let days_from_ce = expected.num_days_from_ce();
            let days_since_epoch = days_from_ce - 719_163;
            let v = Value::Date(days_since_epoch);
            let d = NaiveDate::from_value(&v).expect("2024-03-15");
            assert_eq!(d, expected);
        }

        #[test]
        fn roundtrip_naive_time() {
            // 01:02:03.000042 → microseconds
            let us = (3600 + 2 * 60 + 3) * 1_000_000i64 + 42;
            let v = Value::Time(us);
            let t = NaiveTime::from_value(&v).expect("time");
            assert_eq!(t.hour(), 1);
            assert_eq!(t.minute(), 2);
            assert_eq!(t.second(), 3);
            // sub-second: 42 microseconds = 42_000 nanoseconds
            assert_eq!(t.nanosecond(), 42_000);
        }

        #[test]
        fn roundtrip_naive_datetime() {
            // Unix epoch timestamp: 0 microseconds → 1970-01-01T00:00:00
            let v = Value::Timestamp(0);
            let dt = NaiveDateTime::from_value(&v).expect("epoch datetime");
            let expected_epoch = DateTime::from_timestamp(0, 0).unwrap().naive_utc();
            assert_eq!(dt, expected_epoch);

            // 1_000_000 microseconds = 1 second after epoch
            let v = Value::Timestamp(1_000_000);
            let dt = NaiveDateTime::from_value(&v).expect("1s after epoch");
            let expected_1s = DateTime::from_timestamp(1, 0).unwrap().naive_utc();
            assert_eq!(dt, expected_1s);
        }

        #[test]
        fn roundtrip_datetime_utc() {
            let v = Value::Timestamp(0);
            let dt = DateTime::<Utc>::from_value(&v).expect("utc epoch");
            assert_eq!(dt, DateTime::from_timestamp(0, 0).unwrap());
        }

        #[test]
        fn fromvalue_error_on_mismatch() {
            // Non-date value → error
            assert!(matches!(
                NaiveDate::from_value(&Value::Bool(true)),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
            // Non-time value → error
            assert!(matches!(
                NaiveTime::from_value(&Value::I64(42)),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
            // Non-timestamp value → error
            assert!(matches!(
                NaiveDateTime::from_value(&Value::Text("not-a-dt".into())),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
            assert!(matches!(
                DateTime::<Utc>::from_value(&Value::Bool(false)),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
        }

        #[test]
        fn naive_date_from_text() {
            let v = Value::Text("2024-06-10".into());
            let d = NaiveDate::from_value(&v).expect("from text");
            assert_eq!(d, NaiveDate::from_ymd_opt(2024, 6, 10).unwrap());
        }

        #[test]
        fn naive_date_from_i64() {
            // day 0 from i64
            let v = Value::I64(0);
            let d = NaiveDate::from_value(&v).expect("epoch from i64");
            assert_eq!(d, NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
        }
    }
}

// ── FromValue for time types (feature = "time") ──────────────────────────────

#[cfg(feature = "time")]
mod time_impls {
    use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

    use crate::row::FromValue;
    use crate::{OxiSqlError, Value};

    impl FromValue for Date {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Date(days) => {
                    // Value::Date is days since Unix epoch (1970-01-01).
                    // `time` uses Julian day internally; we reconstruct via year/month/day
                    // by converting to an OffsetDateTime from a Unix timestamp.
                    let unix_secs = i64::from(*days) * 86_400;
                    OffsetDateTime::from_unix_timestamp(unix_secs)
                        .map(|odt| odt.date())
                        .map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "time::Date (valid days-since-epoch)",
                            got: "Date (out of range)",
                        })
                }
                Value::Text(s) => {
                    // Attempt ISO 8601 YYYY-MM-DD parse
                    let parts: Vec<&str> = s.splitn(3, '-').collect();
                    if parts.len() == 3 {
                        let y: i32 = parts[0].parse().map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "time::Date (YYYY-MM-DD text)",
                            got: "Text (non-numeric year)",
                        })?;
                        let mo: u8 = parts[1].parse().map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "time::Date (YYYY-MM-DD text)",
                            got: "Text (non-numeric month)",
                        })?;
                        let d: u8 = parts[2].parse().map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "time::Date (YYYY-MM-DD text)",
                            got: "Text (non-numeric day)",
                        })?;
                        let month = Month::try_from(mo).map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "time::Date (month 1-12)",
                            got: "Text (month out of range)",
                        })?;
                        Date::from_calendar_date(y, month, d).map_err(|_| {
                            OxiSqlError::TypeMismatch {
                                expected: "time::Date (valid calendar date)",
                                got: "Text (invalid date)",
                            }
                        })
                    } else {
                        Err(OxiSqlError::TypeMismatch {
                            expected: "time::Date (YYYY-MM-DD text)",
                            got: "Text (not a valid date)",
                        })
                    }
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Date",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for Time {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Time(us) => {
                    let total_secs = us / 1_000_000;
                    let h = (total_secs / 3600) as u8;
                    let m = ((total_secs % 3600) / 60) as u8;
                    let s = (total_secs % 60) as u8;
                    let nano = ((*us % 1_000_000) * 1_000) as u32;
                    Time::from_hms_nano(h, m, s, nano).map_err(|_| OxiSqlError::TypeMismatch {
                        expected: "time::Time (valid time-of-day)",
                        got: "Time (out of range)",
                    })
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Time",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for PrimitiveDateTime {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Timestamp(us) => {
                    let secs = us.div_euclid(1_000_000);
                    let nano = (us.rem_euclid(1_000_000) * 1_000) as u32;
                    let odt = OffsetDateTime::from_unix_timestamp(secs).map_err(|_| {
                        OxiSqlError::TypeMismatch {
                            expected: "PrimitiveDateTime (valid timestamp)",
                            got: "Timestamp (out of range)",
                        }
                    })?;
                    // Replace sub-second with exact nanoseconds from the microsecond field
                    let odt_with_nanos =
                        odt.replace_nanosecond(nano)
                            .map_err(|_| OxiSqlError::TypeMismatch {
                                expected: "PrimitiveDateTime (valid nanoseconds)",
                                got: "Timestamp (nanosecond out of range)",
                            })?;
                    Ok(PrimitiveDateTime::new(
                        odt_with_nanos.date(),
                        odt_with_nanos.time(),
                    ))
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Timestamp",
                    got: other.type_name(),
                }),
            }
        }
    }

    impl FromValue for OffsetDateTime {
        fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
            match v {
                Value::Timestamp(us) => {
                    let secs = us.div_euclid(1_000_000);
                    let nano = (us.rem_euclid(1_000_000) * 1_000) as u32;
                    let odt = OffsetDateTime::from_unix_timestamp(secs).map_err(|_| {
                        OxiSqlError::TypeMismatch {
                            expected: "OffsetDateTime (valid timestamp)",
                            got: "Timestamp (out of range)",
                        }
                    })?;
                    odt.replace_nanosecond(nano)
                        .map_err(|_| OxiSqlError::TypeMismatch {
                            expected: "OffsetDateTime (valid nanoseconds)",
                            got: "Timestamp (nanosecond out of range)",
                        })
                }
                other => Err(OxiSqlError::TypeMismatch {
                    expected: "Timestamp",
                    got: other.type_name(),
                }),
            }
        }
    }

    #[cfg(test)]
    mod time_tests {
        use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

        use crate::row::FromValue;
        use crate::{OxiSqlError, Value};

        #[test]
        fn roundtrip_time_date() {
            // Unix epoch = 1970-01-01
            let v = Value::Date(0);
            let d = Date::from_value(&v).expect("epoch date");
            let expected = Date::from_calendar_date(1970, Month::January, 1).unwrap();
            assert_eq!(d, expected);

            // 1970-01-02 = day 1
            let v = Value::Date(1);
            let d = Date::from_value(&v).expect("day 1");
            let expected = Date::from_calendar_date(1970, Month::January, 2).unwrap();
            assert_eq!(d, expected);
        }

        #[test]
        fn roundtrip_time_time() {
            // 01:02:03 = (3600 + 120 + 3) * 1_000_000 µs
            let us = (3600 + 2 * 60 + 3) * 1_000_000i64;
            let v = Value::Time(us);
            let t = Time::from_value(&v).expect("time");
            assert_eq!(t.hour(), 1);
            assert_eq!(t.minute(), 2);
            assert_eq!(t.second(), 3);
        }

        #[test]
        fn roundtrip_primitive_datetime() {
            let v = Value::Timestamp(0);
            let dt = PrimitiveDateTime::from_value(&v).expect("epoch");
            let epoch = Date::from_calendar_date(1970, Month::January, 1).unwrap();
            let midnight = Time::from_hms(0, 0, 0).unwrap();
            assert_eq!(dt, PrimitiveDateTime::new(epoch, midnight));
        }

        #[test]
        fn roundtrip_offset_datetime() {
            let v = Value::Timestamp(0);
            let dt = OffsetDateTime::from_value(&v).expect("utc epoch");
            assert_eq!(dt.unix_timestamp(), 0);
        }

        #[test]
        fn time_date_from_text() {
            let v = Value::Text("2024-06-10".into());
            let d = Date::from_value(&v).expect("from text");
            let expected = Date::from_calendar_date(2024, Month::June, 10).unwrap();
            assert_eq!(d, expected);
        }

        #[test]
        fn time_fromvalue_error_on_mismatch() {
            assert!(matches!(
                Date::from_value(&Value::Bool(true)),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
            assert!(matches!(
                Time::from_value(&Value::I64(42)),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
            assert!(matches!(
                PrimitiveDateTime::from_value(&Value::Text("bad".into())),
                Err(OxiSqlError::TypeMismatch { .. })
            ));
        }
    }
}

#[cfg(test)]
mod typed_array_tests {
    use super::*;

    #[test]
    fn typed_array_display() {
        let v = Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::I64(1), Value::I64(2), Value::Null],
        };
        let s = format!("{v}");
        assert!(s.starts_with("int4["), "got: {s}");
        assert!(s.contains("1"), "got: {s}");
        assert!(s.contains("NULL"), "got: {s}");
    }

    #[test]
    fn typed_array_type_name() {
        let v = Value::TypedArray {
            element_type: ArrayElementType::Text,
            values: vec![],
        };
        assert_eq!(v.type_name(), "TypedArray");
    }

    #[test]
    fn typed_array_partial_ord() {
        let a = Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::I64(1)],
        };
        let b = Value::TypedArray {
            element_type: ArrayElementType::Int4,
            values: vec![Value::I64(2)],
        };
        assert!(a < b);
    }

    #[test]
    fn typed_array_is_not_null() {
        let v = Value::TypedArray {
            element_type: ArrayElementType::Bool,
            values: vec![],
        };
        assert!(!v.is_null());
    }

    #[test]
    fn array_element_type_display() {
        assert_eq!(ArrayElementType::Int4.to_string(), "int4");
        assert_eq!(ArrayElementType::TimestampTz.to_string(), "timestamptz");
    }
}
