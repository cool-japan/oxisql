//! Type conversion between `tokio-postgres` values and `oxisql_core` types.
//!
//! Supported Postgres types -> OxiSQL [`Value`] mapping:
//!
//! | Postgres OID group | OxiSQL variant |
//! |---|---|
//! | BOOL | `Value::Bool` |
//! | INT2, INT4 | `Value::I64` (widened) |
//! | INT8 | `Value::I64` |
//! | FLOAT4 | `Value::F64` (widened) |
//! | FLOAT8 | `Value::F64` |
//! | TEXT, VARCHAR, BPCHAR, NAME | `Value::Text` |
//! | BYTEA | `Value::Blob` |
//! | DATE | `Value::Date` (days since Unix epoch 1970-01-01) |
//! | TIMESTAMP | `Value::Timestamp` (microseconds since Unix epoch, no tz) |
//! | TIMESTAMPTZ | `Value::Timestamp` (microseconds since Unix epoch, UTC) |
//! | TIME | `Value::Time` (microseconds since midnight) |
//! | UUID | `Value::Uuid` (u128 big-endian) |
//! | JSON, JSONB | `Value::Json` |
//! | NUMERIC | `Value::Decimal` (exact decimal string) |
//! | BOOL[] / INT2[] / INT4[] / INT8[] / FLOAT4[] / FLOAT8[] / TEXT[] / VARCHAR[] | `Value::Array(Vec<Value>)` |
//! | INTERVAL | `Value::Text` (HH:MM:SS or "N months N days HH:MM:SS") |
//! | NULL (any) | `Value::Null` |
//! | everything else | `Value::Text` via type-name marker |

use bytes::{Buf, BytesMut};
use tokio_postgres::types::{FromSql, IsNull, ToSql, Type};
use tokio_postgres::Row as PgRow;

use oxisql_core::{Row, Value};

use crate::error::PgError;

// ── NUMERIC binary decoder ────────────────────────────────────────────────────

/// Newtype that captures raw Postgres NUMERIC bytes via a custom [`FromSql`] impl.
///
/// Postgres NUMERIC binary format:
/// - 2 bytes (big-endian u16): `ndigits` — number of base-10000 digit groups
/// - 2 bytes (big-endian i16): `weight` — index of the most-significant digit group
///   (weight=0 means the group represents 1..9999)
/// - 2 bytes (big-endian u16): `sign`   — 0x0000 positive, 0x4000 negative, 0xC000 NaN
/// - 2 bytes (big-endian u16): `dscale` — number of decimal digits after the point
/// - `ndigits` × 2 bytes (big-endian u16): digit groups, each 0..9999
struct RawNumeric(Vec<u8>);

impl<'a> FromSql<'a> for RawNumeric {
    fn from_sql(
        _ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(RawNumeric(raw.to_vec()))
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::NUMERIC
    }
}

/// Decode a Postgres NUMERIC binary payload into a decimal string.
///
/// Returns `"NaN"` for Postgres NaN values.  Returns an error if the byte
/// buffer is too short or otherwise malformed.
fn decode_pg_numeric(raw: &[u8]) -> Result<String, PgError> {
    if raw.len() < 8 {
        return Err(PgError::TypeConversion(format!(
            "NUMERIC binary too short: {} bytes",
            raw.len()
        )));
    }

    let mut buf = raw;

    let ndigits = buf.get_u16() as usize;
    let weight = buf.get_i16(); // signed: negative means pure fraction
    let sign = buf.get_u16();
    let dscale = buf.get_u16() as usize;

    const SIGN_NAN: u16 = 0xC000;
    const SIGN_NEG: u16 = 0x4000;

    if sign == SIGN_NAN {
        return Ok("NaN".to_string());
    }

    // Collect digit groups (each is a base-10000 value).
    let mut digits: Vec<u16> = Vec::with_capacity(ndigits);
    for _ in 0..ndigits {
        if buf.remaining() < 2 {
            return Err(PgError::TypeConversion(
                "NUMERIC binary truncated while reading digit groups".to_string(),
            ));
        }
        digits.push(buf.get_u16());
    }

    // Build the integer part: digit groups at positions weight, weight-1, …, 0
    // (each group contributes 4 decimal digits; the highest group may have fewer).
    //
    // A group at weight position `w` represents the range:
    //   10000^w  …  10000^(w+1) - 1
    //
    // We synthesise from the most-significant group down to weight=0, then
    // continue with fractional groups.

    let mut integer_part = String::new();
    let mut fraction_part = String::new();

    // Number of groups that lie at weight >= 0 (integer part).
    let int_groups = (weight + 1).max(0) as usize;

    // Fill integer groups, padding with zeros for missing leading groups.
    // We zip a range with padded digits so clippy doesn't see needless indexing.
    let int_digit_iter = (0..int_groups).map(|i| if i < ndigits { digits[i] } else { 0 });
    for (i, digit) in int_digit_iter.enumerate() {
        if i == 0 {
            // First group: no leading zeros.
            integer_part.push_str(&digit.to_string());
        } else {
            // Subsequent groups: always 4 digits with leading zeros.
            integer_part.push_str(&format!("{digit:04}"));
        }
    }

    if integer_part.is_empty() {
        integer_part.push('0');
    }

    // Fractional groups start at index `int_groups` in the digits array.
    // We need `dscale` decimal digits total in the fraction.
    if dscale > 0 {
        let mut remaining_scale = dscale;
        let frac_groups_needed = dscale.div_ceil(4);

        for i in 0..frac_groups_needed {
            let digit_idx = int_groups + i;
            let digit = if digit_idx < ndigits {
                digits[digit_idx]
            } else {
                0
            };
            let group_str = format!("{digit:04}");
            // Only append as many digits as dscale demands.
            let take = remaining_scale.min(4);
            fraction_part.push_str(&group_str[..take]);
            remaining_scale = remaining_scale.saturating_sub(4);
        }
    }

    let mut result = if sign == SIGN_NEG {
        format!("-{integer_part}")
    } else {
        integer_part
    };

    if !fraction_part.is_empty() {
        result.push('.');
        result.push_str(&fraction_part);
    }

    Ok(result)
}

// ── INTERVAL binary decoder ───────────────────────────────────────────────────

/// Newtype that captures raw Postgres INTERVAL bytes via a custom [`FromSql`] impl.
///
/// Postgres INTERVAL binary format (16 bytes total):
/// - 8 bytes (big-endian i64): microseconds part
/// - 4 bytes (big-endian i32): days part
/// - 4 bytes (big-endian i32): months part
struct RawInterval(Vec<u8>);

impl<'a> FromSql<'a> for RawInterval {
    fn from_sql(
        _ty: &Type,
        raw: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(RawInterval(raw.to_vec()))
    }

    fn accepts(ty: &Type) -> bool {
        *ty == Type::INTERVAL
    }
}

/// Decode a Postgres INTERVAL binary payload into a human-readable string.
///
/// Produces a representation like `"1Y 2M 3D 01:30:00"` or just `"01:30:00"`.
fn decode_pg_interval(raw: &[u8]) -> Result<String, PgError> {
    if raw.len() < 16 {
        return Err(PgError::TypeConversion(format!(
            "INTERVAL binary too short: {} bytes",
            raw.len()
        )));
    }
    let mut buf = raw;
    let micros = buf.get_i64();
    let days = buf.get_i32();
    let months = buf.get_i32();

    let years = months / 12;
    let rem_months = months % 12;
    let hours = micros / 3_600_000_000;
    let rem_micros = micros % 3_600_000_000;
    let minutes = rem_micros / 60_000_000;
    let rem_micros2 = rem_micros % 60_000_000;
    let secs = rem_micros2 / 1_000_000;
    let frac_micros = (rem_micros2 % 1_000_000).unsigned_abs();

    let mut parts: Vec<String> = Vec::new();
    if years != 0 {
        parts.push(format!("{years}Y"));
    }
    if rem_months != 0 {
        parts.push(format!("{rem_months}M"));
    }
    if days != 0 {
        parts.push(format!("{days}D"));
    }

    let has_time = hours != 0 || minutes != 0 || secs != 0 || frac_micros != 0;
    if has_time {
        if frac_micros > 0 {
            parts.push(format!(
                "{hours:02}:{minutes:02}:{secs:02}.{frac_micros:06}"
            ));
        } else {
            parts.push(format!("{hours:02}:{minutes:02}:{secs:02}"));
        }
    }

    if parts.is_empty() {
        Ok("00:00:00".to_string())
    } else {
        Ok(parts.join(" "))
    }
}

// ── Parameter encoding ────────────────────────────────────────────────────────

/// Owned SQL parameter that implements `ToSql`.
///
/// `tokio-postgres` expects `&[&(dyn ToSql + Sync)]` parameters.  Since our
/// source values live in a `Vec<Value>`, we first convert them to this owned
/// enum, collect references, then pass a slice.
#[derive(Debug)]
pub enum OwnedParam {
    /// SQL NULL.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit float.
    F64(f64),
    /// UTF-8 string.
    Text(String),
    /// Binary blob.
    Blob(Vec<u8>),
}

impl ToSql for OwnedParam {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut BytesMut,
    ) -> Result<IsNull, Box<dyn std::error::Error + Sync + Send>> {
        match self {
            OwnedParam::Null => Ok(IsNull::Yes),
            OwnedParam::Bool(v) => v.to_sql(ty, out),
            OwnedParam::I64(v) => {
                if *ty == Type::INT4 {
                    let v4 = i32::try_from(*v)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Sync + Send>)?;
                    v4.to_sql(ty, out)
                } else if *ty == Type::INT2 {
                    let v2 = i16::try_from(*v)
                        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Sync + Send>)?;
                    v2.to_sql(ty, out)
                } else {
                    v.to_sql(ty, out)
                }
            }
            OwnedParam::F64(v) => {
                if *ty == Type::FLOAT4 {
                    #[allow(clippy::cast_possible_truncation)]
                    let v4 = *v as f32;
                    v4.to_sql(ty, out)
                } else {
                    v.to_sql(ty, out)
                }
            }
            OwnedParam::Text(v) => v.to_sql(ty, out),
            OwnedParam::Blob(v) => v.to_sql(ty, out),
        }
    }

    fn accepts(_ty: &Type) -> bool
    where
        Self: Sized,
    {
        // We accept any type at this stage; incompatibility will produce an
        // error from the underlying concrete `to_sql` call.
        true
    }

    tokio_postgres::types::to_sql_checked!();
}

/// Convert an [`oxisql_core::Value`] to an [`OwnedParam`].
///
/// Extended types (Timestamp, Date, Time, Uuid, Json, Decimal, Array) are
/// converted to their string representation and sent as Text parameters.
/// The Postgres server handles the implicit cast for typed columns.
pub fn value_to_param(v: &Value) -> OwnedParam {
    match v {
        Value::Null => OwnedParam::Null,
        Value::Bool(b) => OwnedParam::Bool(*b),
        Value::I64(n) => OwnedParam::I64(*n),
        Value::F64(f) => OwnedParam::F64(*f),
        Value::Text(s) => OwnedParam::Text(s.clone()),
        Value::Blob(b) => OwnedParam::Blob(b.clone()),
        // Extended types: send as text, Postgres handles the cast
        Value::Timestamp(us) => {
            let secs = us / 1_000_000;
            let frac = (us % 1_000_000).unsigned_abs();
            OwnedParam::Text(format!("{secs}.{frac:06}"))
        }
        Value::Date(days) => OwnedParam::Text(format!("{days}")),
        Value::Time(us) => {
            let total_secs = us / 1_000_000;
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            let secs = total_secs % 60;
            let frac = (us % 1_000_000).unsigned_abs();
            if frac == 0 {
                OwnedParam::Text(format!("{hours:02}:{mins:02}:{secs:02}"))
            } else {
                OwnedParam::Text(format!("{hours:02}:{mins:02}:{secs:02}.{frac:06}"))
            }
        }
        Value::Uuid(u) => OwnedParam::Text(format!("{}", Value::Uuid(*u))),
        Value::Json(s) => OwnedParam::Text(s.clone()),
        Value::Decimal(s) => OwnedParam::Text(s.clone()),
        Value::Array(vals) => {
            // Render as Postgres array literal: {val1,val2,...}
            let items: Vec<String> = vals.iter().map(|v| format!("{v}")).collect();
            OwnedParam::Text(format!("{{{}}}", items.join(",")))
        }
    }
}

// ── Row decoding ──────────────────────────────────────────────────────────────

/// Convert a `tokio_postgres` [`PgRow`] into an `oxisql_core` [`Row`].
///
/// Column types are read from the row's `Column` descriptors so each value
/// is extracted using the most specific supported type.  Unsupported column
/// types fall back to a placeholder [`Value::Text`].
pub fn pg_row_to_row(pg_row: PgRow) -> Result<Row, PgError> {
    let cols = pg_row.columns();
    let mut names = Vec::with_capacity(cols.len());
    let mut values = Vec::with_capacity(cols.len());

    for (i, col) in cols.iter().enumerate() {
        names.push(col.name().to_string());
        let value = extract_value(&pg_row, i, col.type_())?;
        values.push(value);
    }

    Ok(Row::new(names, values))
}

fn extract_value(row: &PgRow, idx: usize, ty: &Type) -> Result<Value, PgError> {
    match *ty {
        Type::BOOL => {
            let v: Option<bool> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(Value::Bool).unwrap_or(Value::Null))
        }
        Type::INT2 => {
            let v: Option<i16> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|n| Value::I64(i64::from(n))).unwrap_or(Value::Null))
        }
        Type::INT4 => {
            let v: Option<i32> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|n| Value::I64(i64::from(n))).unwrap_or(Value::Null))
        }
        Type::INT8 => {
            let v: Option<i64> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(Value::I64).unwrap_or(Value::Null))
        }
        Type::FLOAT4 => {
            let v: Option<f32> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|f| Value::F64(f64::from(f))).unwrap_or(Value::Null))
        }
        Type::FLOAT8 => {
            let v: Option<f64> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(Value::F64).unwrap_or(Value::Null))
        }
        Type::BYTEA => {
            let v: Option<Vec<u8>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(Value::Blob).unwrap_or(Value::Null))
        }
        // JSON and JSONB: extract as String and wrap in Value::Json
        Type::JSON | Type::JSONB => {
            let v: Option<String> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(Value::Json).unwrap_or(Value::Null))
        }
        // DATE: extract as time::Date, convert to days since Unix epoch (1970-01-01).
        //
        // `time::Date::to_julian_day()` returns the proleptic Gregorian Julian Day Number:
        //   - 1970-01-01 → 2_440_588
        //   - 2000-01-01 → 2_451_545
        //
        // `Value::Date` stores days since Unix epoch, so we subtract the JDN of the epoch.
        Type::DATE => {
            let v: Option<time::Date> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|d| {
                const UNIX_EPOCH_JDN: i32 = 2_440_588; // Julian Day Number of 1970-01-01
                Value::Date(d.to_julian_day() - UNIX_EPOCH_JDN)
            })
            .unwrap_or(Value::Null))
        }
        // TIMESTAMP (no time zone): extract as PrimitiveDateTime, treat as UTC.
        Type::TIMESTAMP => {
            let v: Option<time::PrimitiveDateTime> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|dt| {
                // assume_utc() converts PrimitiveDateTime to OffsetDateTime at UTC offset.
                let us = dt.assume_utc().unix_timestamp_nanos() / 1_000;
                #[allow(clippy::cast_possible_truncation)]
                Value::Timestamp(us as i64)
            })
            .unwrap_or(Value::Null))
        }
        // TIMESTAMPTZ: extract as OffsetDateTime (already carries its UTC offset).
        Type::TIMESTAMPTZ => {
            let v: Option<time::OffsetDateTime> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|dt| {
                let us = dt.unix_timestamp_nanos() / 1_000;
                #[allow(clippy::cast_possible_truncation)]
                Value::Timestamp(us as i64)
            })
            .unwrap_or(Value::Null))
        }
        // TIME (no time zone): microseconds since midnight.
        Type::TIME => {
            let v: Option<time::Time> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|t| {
                let (h, m, s, ns) = t.as_hms_nano();
                let us = i64::from(h) * 3_600_000_000
                    + i64::from(m) * 60_000_000
                    + i64::from(s) * 1_000_000
                    + i64::from(ns) / 1_000;
                Value::Time(us)
            })
            .unwrap_or(Value::Null))
        }
        // UUID: extract as uuid::Uuid, store as big-endian u128.
        Type::UUID => {
            let v: Option<uuid::Uuid> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|u| Value::Uuid(u.as_u128())).unwrap_or(Value::Null))
        }
        // NUMERIC: decode binary payload manually into an exact decimal string.
        Type::NUMERIC => {
            let v: Option<RawNumeric> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            match v {
                None => Ok(Value::Null),
                Some(raw) => {
                    let s = decode_pg_numeric(&raw.0)?;
                    Ok(Value::Decimal(s))
                }
            }
        }
        // INTERVAL: decode binary payload manually into a human-readable string.
        Type::INTERVAL => {
            let v: Option<RawInterval> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            match v {
                None => Ok(Value::Null),
                Some(raw) => {
                    let s = decode_pg_interval(&raw.0)?;
                    Ok(Value::Text(s))
                }
            }
        }
        // ARRAY types: each maps to Value::Array(Vec<Value>).
        Type::BOOL_ARRAY => {
            let v: Option<Vec<Option<bool>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(Value::Bool).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::INT2_ARRAY => {
            let v: Option<Vec<Option<i16>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(|n| Value::I64(i64::from(n))).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::INT4_ARRAY => {
            let v: Option<Vec<Option<i32>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(|n| Value::I64(i64::from(n))).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::INT8_ARRAY => {
            let v: Option<Vec<Option<i64>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(Value::I64).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::FLOAT4_ARRAY => {
            let v: Option<Vec<Option<f32>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(|f| Value::F64(f64::from(f))).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::FLOAT8_ARRAY => {
            let v: Option<Vec<Option<f64>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(Value::F64).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        Type::TEXT_ARRAY | Type::VARCHAR_ARRAY => {
            let v: Option<Vec<Option<String>>> = row
                .try_get(idx)
                .map_err(|e| PgError::TypeConversion(e.to_string()))?;
            Ok(v.map(|items| {
                Value::Array(
                    items
                        .into_iter()
                        .map(|opt| opt.map(Value::Text).unwrap_or(Value::Null))
                        .collect(),
                )
            })
            .unwrap_or(Value::Null))
        }
        // TEXT, VARCHAR, BPCHAR, NAME, UNKNOWN and anything String-compatible.
        ref other => {
            if <String as FromSql>::accepts(other) {
                let v: Option<String> = row
                    .try_get(idx)
                    .map_err(|e| PgError::TypeConversion(e.to_string()))?;
                Ok(v.map(Value::Text).unwrap_or(Value::Null))
            } else {
                // Unknown type: return an opaque placeholder rather than failing.
                Ok(Value::Text(format!("<opaque:{}>", other.name())))
            }
        }
    }
}
