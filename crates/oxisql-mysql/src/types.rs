//! Type conversion between `mysql_async` values and `oxisql_core` types.
//!
//! # Supported MySQL -> OxiSQL [`Value`] mapping
//!
//! | MySQL type (mysql_async::Value variant) | OxiSQL variant |
//! |---|---|
//! | NULL | `Value::Null` |
//! | Bytes (TEXT/VARCHAR/BINARY/BLOB) | `Value::Text` (UTF-8) or `Value::Blob` |
//! | Bytes + NEWDECIMAL/DECIMAL column type | `Value::Decimal` |
//! | Bytes + JSON column type | `Value::Json` |
//! | Bytes + ENUM/SET column type | `Value::Text` |
//! | Bytes + BLOB/LONG_BLOB/MEDIUM_BLOB/TINY_BLOB | `Value::Blob` |
//! | Int | `Value::I64` |
//! | UInt (<= i64::MAX) | `Value::I64` |
//! | UInt (> i64::MAX) | `Value::Text` (string fallback) |
//! | Float | `Value::F64` (widened from f32) |
//! | Double | `Value::F64` |
//! | Date (date-only) | `Value::Date` (days since Unix epoch) |
//! | Date (datetime) | `Value::Timestamp` (microseconds since Unix epoch) |
//! | Time | `Value::Time` (microseconds, can be negative for intervals) |

use chrono::NaiveDate;
use mysql_async::consts::ColumnType;
use oxisql_core::{Row, Value};

use crate::error::MysqlError;

/// Unix epoch as a [`NaiveDate`] constant used for computing day offsets.
///
/// Using a named constant avoids the `.expect()` on every call and makes
/// the intent clear.  The value `1970-01-01` is always valid.
fn unix_epoch() -> NaiveDate {
    // Safety: 1970-01-01 is a well-known valid date; this can never fail.
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a valid date")
}

/// Convert a `mysql_async::Value` into an `oxisql_core::Value`, taking the
/// server-reported column type into account for `Bytes` values.
///
/// When the column type is one of the well-known extended types the conversion
/// is more precise than the plain [`mysql_value_to_core`]:
///
/// - `MYSQL_TYPE_NEWDECIMAL` / `MYSQL_TYPE_DECIMAL` → [`Value::Decimal`]
/// - `MYSQL_TYPE_JSON` → [`Value::Json`]
/// - `MYSQL_TYPE_ENUM` / `MYSQL_TYPE_SET` → [`Value::Text`]
/// - `MYSQL_TYPE_BLOB` / `…_LONG_BLOB` / `…_MEDIUM_BLOB` / `…_TINY_BLOB` → [`Value::Blob`]
/// - All other `Bytes` → UTF-8 text if valid, otherwise [`Value::Blob`]
///
/// Non-`Bytes` variants are handled identically to [`mysql_value_to_core`].
pub fn mysql_value_to_core_with_type(
    v: mysql_async::Value,
    col_type: ColumnType,
) -> Result<Value, MysqlError> {
    match v {
        mysql_async::Value::NULL => Ok(Value::Null),
        mysql_async::Value::Bytes(b) => match col_type {
            ColumnType::MYSQL_TYPE_NEWDECIMAL | ColumnType::MYSQL_TYPE_DECIMAL => {
                let s = String::from_utf8(b).map_err(|e| MysqlError::TypeMap(e.to_string()))?;
                Ok(Value::Decimal(s))
            }
            ColumnType::MYSQL_TYPE_JSON => {
                let s = String::from_utf8(b).map_err(|e| MysqlError::TypeMap(e.to_string()))?;
                Ok(Value::Json(s))
            }
            ColumnType::MYSQL_TYPE_ENUM | ColumnType::MYSQL_TYPE_SET => {
                let s = String::from_utf8(b).map_err(|e| MysqlError::TypeMap(e.to_string()))?;
                Ok(Value::Text(s))
            }
            ColumnType::MYSQL_TYPE_BLOB
            | ColumnType::MYSQL_TYPE_LONG_BLOB
            | ColumnType::MYSQL_TYPE_MEDIUM_BLOB
            | ColumnType::MYSQL_TYPE_TINY_BLOB
            | ColumnType::MYSQL_TYPE_GEOMETRY => {
                // GEOMETRY columns contain WKB (Well-Known Binary) data.
                // Map to Blob for raw binary access; callers that need
                // parsed geometry can decode the WKB bytes themselves.
                Ok(Value::Blob(b))
            }
            _ => match String::from_utf8(b.clone()) {
                Ok(s) => Ok(Value::Text(s)),
                Err(_) => Ok(Value::Blob(b)),
            },
        },
        // All numeric and temporal variants are unaffected by the column type.
        other => mysql_value_to_core(other),
    }
}

/// Convert a `mysql_async::Value` into an `oxisql_core::Value`.
///
/// `Bytes` data is interpreted as UTF-8 text when valid; otherwise it is
/// stored as a `Value::Blob`.  Date-only values produce `Value::Date` (days
/// since Unix epoch).  Datetime values produce `Value::Timestamp` (microseconds
/// since Unix epoch, UTC).  Time/interval values produce `Value::Time`
/// (total microseconds; negative for negative intervals).  UInt values
/// exceeding i64::MAX are converted to text strings rather than returning
/// an error.
///
/// For extended column types (DECIMAL, JSON, ENUM, SET) prefer
/// [`mysql_value_to_core_with_type`], which uses the server-reported column
/// type to produce the most precise [`Value`] variant.
pub fn mysql_value_to_core(v: mysql_async::Value) -> Result<Value, MysqlError> {
    match v {
        mysql_async::Value::NULL => Ok(Value::Null),
        mysql_async::Value::Bytes(b) => match String::from_utf8(b.clone()) {
            Ok(s) => Ok(Value::Text(s)),
            Err(_) => Ok(Value::Blob(b)),
        },
        mysql_async::Value::Int(n) => Ok(Value::I64(n)),
        mysql_async::Value::UInt(n) => {
            // Graceful fallback: values exceeding i64::MAX become Text
            // instead of returning an error.
            match i64::try_from(n) {
                Ok(signed) => Ok(Value::I64(signed)),
                Err(_) => Ok(Value::Text(n.to_string())),
            }
        }
        mysql_async::Value::Float(f) => Ok(Value::F64(f64::from(f))),
        mysql_async::Value::Double(f) => Ok(Value::F64(f)),
        mysql_async::Value::Date(year, month, day, hour, min, sec, micro) => {
            if hour == 0 && min == 0 && sec == 0 && micro == 0 {
                // Date-only value → Value::Date(days since Unix epoch)
                let d = NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                    .ok_or_else(|| {
                        MysqlError::TypeMap(format!("invalid date {year:04}-{month:02}-{day:02}"))
                    })?;
                let days = d.signed_duration_since(unix_epoch()).num_days();
                let days_i32 = i32::try_from(days).map_err(|_| {
                    MysqlError::TypeMap(format!(
                        "date {year:04}-{month:02}-{day:02} is out of i32 day range"
                    ))
                })?;
                Ok(Value::Date(days_i32))
            } else {
                // Datetime value → Value::Timestamp(microseconds since Unix epoch)
                // MySQL microseconds field is already in microseconds, but
                // NaiveDate::and_hms_nano_opt expects nanoseconds.
                let nanos = micro * 1_000;
                let dt = NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))
                    .and_then(|d| {
                        d.and_hms_nano_opt(u32::from(hour), u32::from(min), u32::from(sec), nanos)
                    })
                    .ok_or_else(|| {
                        MysqlError::TypeMap(format!(
                            "invalid datetime {year:04}-{month:02}-{day:02} \
                         {hour:02}:{min:02}:{sec:02}"
                        ))
                    })?;
                Ok(Value::Timestamp(dt.and_utc().timestamp_micros()))
            }
        }
        mysql_async::Value::Time(neg, days, hours, mins, secs, micros) => {
            // MySQL TIME can be negative and can span multiple days (used for
            // time intervals).  Convert to total microseconds; negative
            // intervals produce a negative result.
            let total_us: i64 = i64::from(days) * 86_400_000_000
                + i64::from(hours) * 3_600_000_000
                + i64::from(mins) * 60_000_000
                + i64::from(secs) * 1_000_000
                + i64::from(micros);
            let total_us = if neg { -total_us } else { total_us };
            Ok(Value::Time(total_us))
        }
    }
}

/// Convert a `mysql_async` [`Row`][mysql_async::Row] into an `oxisql_core` [`Row`].
///
/// Column names are taken from the row's [`Column`][mysql_async::Column] descriptors.
/// Each cell is mapped through [`mysql_value_to_core_with_type`], which uses the
/// server-reported column type to select the correct [`Value`] variant for
/// extended types such as DECIMAL, JSON, ENUM, and SET.
pub fn mysql_row_to_core(mut row: mysql_async::Row) -> Result<Row, MysqlError> {
    let cols: Vec<String> = row
        .columns_ref()
        .iter()
        .map(|c| c.name_str().into_owned())
        .collect();

    let mut values = Vec::with_capacity(cols.len());
    for i in 0..cols.len() {
        let col_type = row.columns_ref()[i].column_type();
        let raw: Option<mysql_async::Value> = row.take(i);
        let v = match raw {
            Some(val) => mysql_value_to_core_with_type(val, col_type)?,
            None => Value::Null,
        };
        values.push(v);
    }

    Ok(Row::new(cols, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2024-03-15 is 19,797 days after 1970-01-01.
    /// Calculation: (2024-1970)*365 + leap-year adjustments.
    fn days_since_epoch(year: i32, month: u32, day: u32) -> i32 {
        let d = NaiveDate::from_ymd_opt(year, month, day).expect("valid date in test");
        let days = d.signed_duration_since(unix_epoch()).num_days();
        i32::try_from(days).expect("within i32 range in test")
    }

    #[test]
    fn date_only_maps_to_value_date() {
        let v = mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 0, 0, 0, 0))
            .expect("date-only");
        let expected_days = days_since_epoch(2024, 3, 15);
        assert_eq!(v, Value::Date(expected_days));
    }

    #[test]
    fn unix_epoch_date_maps_to_zero() {
        let v = mysql_value_to_core(mysql_async::Value::Date(1970, 1, 1, 0, 0, 0, 0))
            .expect("unix epoch date");
        assert_eq!(v, Value::Date(0));
    }

    #[test]
    fn datetime_maps_to_value_timestamp() {
        // 2024-03-15 10:30:55 UTC
        let v = mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 10, 30, 55, 0))
            .expect("datetime");
        let expected_dt = NaiveDate::from_ymd_opt(2024, 3, 15)
            .and_then(|d| d.and_hms_nano_opt(10, 30, 55, 0))
            .expect("valid datetime in test");
        let expected_us = expected_dt.and_utc().timestamp_micros();
        assert_eq!(v, Value::Timestamp(expected_us));
    }

    #[test]
    fn datetime_with_micros_maps_to_timestamp() {
        // 2024-03-15 10:30:55.123456 UTC
        let v = mysql_value_to_core(mysql_async::Value::Date(2024, 3, 15, 10, 30, 55, 123_456))
            .expect("datetime with micros");
        let expected_dt = NaiveDate::from_ymd_opt(2024, 3, 15)
            .and_then(|d| d.and_hms_nano_opt(10, 30, 55, 123_456 * 1_000))
            .expect("valid datetime in test");
        let expected_us = expected_dt.and_utc().timestamp_micros();
        assert_eq!(v, Value::Timestamp(expected_us));
    }

    #[test]
    fn time_zero_maps_to_value_time_zero() {
        let v =
            mysql_value_to_core(mysql_async::Value::Time(false, 0, 0, 0, 0, 0)).expect("time zero");
        assert_eq!(v, Value::Time(0));
    }

    #[test]
    fn time_with_days_maps_to_value_time() {
        // 1 day + 2 hours + 30 minutes = 26:30:00
        let v = mysql_value_to_core(mysql_async::Value::Time(false, 1, 2, 30, 0, 0))
            .expect("time with days");
        // 26*3600 + 30*60 = 93600 seconds = 93_600_000_000 microseconds
        let expected_us: i64 = (86_400 + 2 * 3_600 + 30 * 60) * 1_000_000i64;
        assert_eq!(v, Value::Time(expected_us));
    }

    #[test]
    fn time_negative_maps_to_negative_value_time() {
        // -01:00:00
        let v = mysql_value_to_core(mysql_async::Value::Time(true, 0, 1, 0, 0, 0))
            .expect("negative time");
        let expected_us: i64 = -3_600_000_000i64;
        assert_eq!(v, Value::Time(expected_us));
    }
}
