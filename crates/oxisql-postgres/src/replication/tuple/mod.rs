//! Maps decoded `pgoutput` tuple **text- and binary-format** cell values to
//! [`oxisql_core::Value`]s, using the column schema carried by a cached
//! [`RelationBody`].
//!
//! # Relationship to [`crate::types`]
//!
//! [`crate::types::pg_row_to_row`] (the ordinary query path) and
//! [`tuple_to_values`] (the replication path, here) solve the same problem —
//! "given a PostgreSQL value and its type, produce the matching
//! [`oxisql_core::Value`]" — but by fundamentally different mechanisms:
//!
//! - [`crate::types::pg_row_to_row`] always decodes `tokio-postgres`'s
//!   **binary** wire format via `Row::try_get::<T: FromSql>`, pulled from a
//!   live `Row` object tied to an active query.
//! - [`tuple_to_values`] has no live `Row` (or even a live connection) to
//!   call `try_get` on — a `pgoutput` tuple cell is just a column-schema
//!   entry (a [`ColumnSpec`], carrying a raw `type_oid`) plus a
//!   [`TupleColumn`] payload, decoded well after the fact from a background
//!   task. It handles both wire formats `pgoutput` can send:
//!     - [`TupleColumn::Text`] — a PostgreSQL **text-output-format string**
//!       (e.g. `"123"`, `"t"`, `"2026-07-11 12:00:00+00"`), the format this
//!       MVP always negotiates (see [`super::pgoutput`]'s module doc
//!       comment: it never asks for `binary 'true'`) — parsed by hand in
//!       [`text_to_value`] (see [Why manual date/time
//!       parsing](self#why-manual-datetime-parsing-not-times-text-parser)).
//!     - [`TupleColumn::Binary`] — `pgoutput`'s binary wire format, decoded
//!       in [`binary_to_value`] via the same `T: FromSql` machinery
//!       `crate::types` uses, just called directly against raw bytes
//!       (`<T as FromSql>::from_sql(&ty, raw)`) instead of through a live
//!       `Row`.
//!
//! Binary deserialization and text parsing are different enough decode
//! strategies that literally sharing code between them would mostly add
//! indirection rather than remove duplication — and `NUMERIC`/`INTERVAL`
//! binary decoding is re-derived from scratch here rather than reusing
//! `crate::types`'s private `decode_pg_numeric`/`decode_pg_interval` (see
//! [`decode_binary_numeric`]/[`decode_binary_interval`]). What IS shared,
//! deliberately, is the *mapping*: this module supports the same set of
//! PostgreSQL OIDs and picks the same [`Value`] variant per OID as the
//! table at the top of [`crate::types`]'s module doc comment (including its
//! array-type table), so a replication consumer and a query consumer
//! observe the same [`Value`] shape for the same PostgreSQL column type.
//! Where the target representation involves nontrivial arithmetic (dates as
//! days-since-Unix-epoch, timestamps/times as microseconds), this module
//! reproduces [`crate::types`]'s exact numeric conventions and formulas,
//! even on the text path, where it arrives at them via the [`time`] crate's
//! calendar/clock constructors rather than binary `FromSql`.
//!
//! # Scope: binary and text formats, including arrays
//!
//! Both scalar and array (`Kind::Array`) column types are decoded, for both
//! [`TupleColumn::Text`] and [`TupleColumn::Binary`] cells — see
//! [`text_to_value`] / [`decode_text_array`] for the text-format entry
//! points and [`binary_to_value`] / [`binary_array_to_value`] for the
//! binary-format ones. This MVP never negotiates `binary 'true'` with the
//! server (see [`super::pgoutput`]'s module doc comment), so
//! [`TupleColumn::Binary`] is not expected on real traffic today; it is
//! decoded anyway so support is ready the moment that negotiation is added,
//! and so its correctness can be tested directly (see this module's tests)
//! without needing a server that actually speaks binary `pgoutput`.
//!
//! # Why manual date/time parsing, not `time`'s text parser
//!
//! This module parses `DATE`/`TIME`/`TIMESTAMP`/`TIMESTAMPTZ` text by
//! hand-splitting on the fixed delimiters PostgreSQL's default `DateStyle`
//! (`ISO`) always uses (`-`, `:`, `.`, `+`/`-` for the timestamptz offset),
//! then building `time::Date`/`time::Time`/`time::PrimitiveDateTime`/
//! `time::UtcOffset` values via their core (always-available) constructors.
//! It deliberately does not use `time`'s own runtime text parser
//! (`Date::parse`/`PrimitiveDateTime::parse`/`OffsetDateTime::parse`,
//! `time::macros::format_description!`), even though this crate's `time`
//! dependency happens to end up with the `parsing` feature enabled in the
//! full workspace build (transitively, via an unrelated dependency chain —
//! `oxitls` -> `x509-parser` -> `asn1-rs` -> `time/parsing`, confirmed via
//! `cargo tree -e features -i time`). Relying on that would make this
//! module's compilability hostage to a TLS-stack dependency having nothing
//! to do with replication; hand-rolled splitting only needs `time`'s `std`
//! feature (this crate's dependency already enables it) and is a better fit
//! anyway for PostgreSQL's variable-width `TIMESTAMPTZ` offset suffix (`+00`
//! / `+05:30` / `+05:30:00`), which `time`'s format-description mini
//! language cannot express as a single optional-width pattern.

use oxisql_core::{ArrayElementType, Value};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};
use tokio_postgres::types::{FromSql, Kind, Type};

use super::pgoutput::{ColumnSpec, RelationBody, TupleColumn, TupleData};
use crate::error::PgError;

/// Julian Day Number of the Unix epoch (1970-01-01), used to convert a
/// parsed [`time::Date`] to the days-since-Unix-epoch convention
/// [`Value::Date`] uses. Mirrors the identical private constant in
/// `crate::types`'s `DATE` decode arm; kept as an independent local copy
/// rather than shared, per this module's [relationship to
/// `crate::types`](self#relationship-to-cratetypes).
const UNIX_EPOCH_JDN: i32 = 2_440_588;

// ── Public API ────────────────────────────────────────────────────────────────

/// A decoded tuple cell: either a real value, or a marker that the source
/// column was an unchanged out-of-line `TOASTed` value.
///
/// PostgreSQL omits the actual bytes for a `TOASTed` column that is
/// unchanged from the previous row version (wire tag `'u'`, see
/// [`TupleColumn::UnchangedToast`]) — the server never sent them, so this
/// module has nothing to decode. A caller that needs the value for such a
/// cell must carry it forward from a prior row image of the same logical
/// row (tracked by primary key / replica identity); this module cannot
/// recover it.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// A successfully decoded value, including SQL `NULL` (represented as
    /// `Value(Value::Null)` — a distinct case from
    /// [`CellValue::UnchangedToast`], even though both mean "no data was
    /// carried in this cell").
    Value(Value),
    /// The column's value is unchanged from a prior row image and was not
    /// retransmitted; see the [type-level documentation](CellValue) for how
    /// callers should handle this.
    UnchangedToast,
}

/// Maps a decoded `pgoutput` [`TupleData`] to OxiSQL [`Value`]s, using the
/// column schema from its cached [`RelationBody`] (as previously announced
/// by an `'R'` Relation message with a matching `rel_id` — see the
/// `pgoutput` module's doc comment for why that caching is the caller's
/// responsibility, not this function's).
///
/// `rel.columns` and `tuple.columns` are matched up positionally:
/// PostgreSQL guarantees a `pgoutput` tuple lists columns in the same order
/// as the `Relation` message that describes them.
///
/// # Errors
///
/// Returns [`PgError::Protocol`] if `rel.columns.len() != tuple.columns.len()`
/// — a mismatch means either the cached `Relation` schema is stale or the
/// two messages were paired incorrectly by the caller, not a value that can
/// be decoded.
///
/// Returns [`PgError::TypeConversion`] if any column's [`TupleColumn::Text`]
/// or [`TupleColumn::Binary`] value fails to parse per its resolved
/// PostgreSQL type's text-output or binary-wire convention (respectively) —
/// see this module's top-of-file "Scope" doc section.
pub fn tuple_to_values(rel: &RelationBody, tuple: &TupleData) -> Result<Vec<CellValue>, PgError> {
    if rel.columns.len() != tuple.columns.len() {
        return Err(PgError::Protocol(format!(
            "relation '{}.{}' (rel_id {}) has {} column(s) but its tuple has {} column(s)",
            rel.namespace,
            rel.name,
            rel.rel_id,
            rel.columns.len(),
            tuple.columns.len(),
        )));
    }
    rel.columns
        .iter()
        .zip(tuple.columns.iter())
        .map(|(col, cell)| decode_cell(col, cell))
        .collect()
}

// ── Per-cell dispatch ────────────────────────────────────────────────────────

/// Decodes one tuple cell against its column's schema.
fn decode_cell(col: &ColumnSpec, cell: &TupleColumn) -> Result<CellValue, PgError> {
    match cell {
        TupleColumn::Null => Ok(CellValue::Value(Value::Null)),
        TupleColumn::UnchangedToast => Ok(CellValue::UnchangedToast),
        TupleColumn::Binary(raw) => binary_to_value(col.type_oid, raw).map(CellValue::Value),
        TupleColumn::Text(s) => text_to_value(col.type_oid, s).map(CellValue::Value),
    }
}

/// Parses one text-format cell value per its resolved PostgreSQL type.
///
/// An OID that [`Type::from_oid`] does not recognize falls back to
/// [`Value::Text`], matching `crate::types`'s "unknown type -> opaque text"
/// philosophy (without the `<opaque:name>` marker `crate::types` uses, since
/// this module has no live connection to look up a type name for an OID it
/// doesn't already know).
fn text_to_value(type_oid: u32, s: &str) -> Result<Value, PgError> {
    let Some(ty) = Type::from_oid(type_oid) else {
        return Ok(Value::Text(s.to_string()));
    };
    if let Kind::Array(elem_ty) = ty.kind() {
        return decode_text_array(elem_ty, s);
    }
    match ty {
        Type::BOOL => parse_bool(s).map(Value::Bool),
        Type::INT2 => parse_int::<i16>(s, "int2").map(Value::I64),
        Type::INT4 => parse_int::<i32>(s, "int4").map(Value::I64),
        Type::INT8 => parse_int::<i64>(s, "int8").map(Value::I64),
        Type::FLOAT4 => parse_float4(s).map(Value::F64),
        Type::FLOAT8 => parse_float8(s).map(Value::F64),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => Ok(Value::Text(s.to_string())),
        Type::BYTEA => parse_bytea(s).map(Value::Blob),
        Type::DATE => parse_date(s).map(Value::Date),
        Type::TIMESTAMP => parse_timestamp(s).map(Value::Timestamp),
        Type::TIMESTAMPTZ => parse_timestamptz(s).map(Value::Timestamp),
        Type::TIME => parse_time(s).map(Value::Time),
        Type::UUID => parse_uuid(s).map(Value::Uuid),
        Type::JSON | Type::JSONB => Ok(Value::Json(s.to_string())),
        Type::NUMERIC => Ok(Value::Decimal(s.to_string())),
        Type::INTERVAL => Ok(Value::Text(s.to_string())),
        _ => Ok(Value::Text(s.to_string())),
    }
}

// ── Scalar text parsers ──────────────────────────────────────────────────────

/// Parses PostgreSQL's `BOOL` text output: exactly `"t"` or `"f"`.
fn parse_bool(s: &str) -> Result<bool, PgError> {
    match s {
        "t" => Ok(true),
        "f" => Ok(false),
        other => Err(PgError::TypeConversion(format!(
            "invalid bool text: {other:?} (expected 't' or 'f')"
        ))),
    }
}

/// Parses `s` as `T`, widening the result to `i64`. Used for `INT2`/`INT4`/
/// `INT8` (`T` = `i16`/`i32`/`i64` respectively). Parsing at the narrow
/// width first — rather than always parsing as `i64` and range-checking —
/// ensures a value that overflows the column's *declared* width is rejected
/// even though `Value::I64` itself could represent it.
fn parse_int<T>(s: &str, pg_type_name: &str) -> Result<i64, PgError>
where
    T: std::str::FromStr,
    i64: From<T>,
    T::Err: std::fmt::Display,
{
    s.parse::<T>()
        .map(i64::from)
        .map_err(|e| PgError::TypeConversion(format!("invalid {pg_type_name} text {s:?}: {e}")))
}

/// Parses a PostgreSQL `FLOAT4` text value, widening to `f64`.
fn parse_float4(s: &str) -> Result<f64, PgError> {
    match s.parse::<f32>() {
        Ok(f) => Ok(f64::from(f)),
        Err(_) => parse_pg_special_float(s, "float4"),
    }
}

/// Parses a PostgreSQL `FLOAT8` text value.
fn parse_float8(s: &str) -> Result<f64, PgError> {
    match s.parse::<f64>() {
        Ok(f) => Ok(f),
        Err(_) => parse_pg_special_float(s, "float8"),
    }
}

/// Matches PostgreSQL's exact text output for the three IEEE-754 special
/// float values — `"Infinity"`, `"-Infinity"`, `"NaN"` (this exact
/// capitalization, unconditionally; PostgreSQL does not vary it by locale)
/// — only reached once the primary `f32`/`f64` parse has already failed.
///
/// This module's tests empirically confirm that Rust's own `f32`/`f64`
/// `FromStr` already accepts these three spellings on the *primary* parse
/// path (its float grammar recognizes `"inf"`/`"infinity"`/`"nan"`
/// case-insensitively, which also matches PostgreSQL's exact-case
/// spelling), so this fallback is not actually exercised by current Rust
/// `std`. It is kept anyway as an explicit, independently tested guarantee
/// that does not silently depend on `std`'s float grammar continuing to
/// accept exactly these spellings.
fn parse_pg_special_float(s: &str, pg_type_name: &str) -> Result<f64, PgError> {
    match s {
        "Infinity" => Ok(f64::INFINITY),
        "-Infinity" => Ok(f64::NEG_INFINITY),
        "NaN" => Ok(f64::NAN),
        _ => Err(PgError::TypeConversion(format!(
            "invalid {pg_type_name} text: {s:?}"
        ))),
    }
}

/// Parses a PostgreSQL `BYTEA` text value in `hex` format (`bytea_output =
/// hex`, the PostgreSQL default since 9.0): a `\x` prefix followed by an
/// even number of hex digit pairs, e.g. `\xdeadbeef` (8 hex digits = 4
/// bytes).
fn parse_bytea(s: &str) -> Result<Vec<u8>, PgError> {
    let Some(hex) = s.strip_prefix("\\x") else {
        return Err(PgError::TypeConversion(format!(
            "bytea text {s:?} is missing the '\\x' prefix (requires bytea_output=hex, \
             the PostgreSQL default since 9.0)"
        )));
    };
    if hex.len() % 2 != 0 {
        return Err(PgError::TypeConversion(format!(
            "bytea text {s:?} has an odd-length hex payload ({} hex character(s))",
            hex.len()
        )));
    }
    let mut bytes = Vec::with_capacity(hex.len() / 2);
    for pair in hex.as_bytes().chunks_exact(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        let (Some(hi), Some(lo)) = (hi, lo) else {
            return Err(PgError::TypeConversion(format!(
                "bytea text {s:?} contains a non-hex-digit character"
            )));
        };
        let byte = u8::try_from((hi << 4) | lo).map_err(|_| {
            PgError::TypeConversion(format!(
                "bytea text {s:?} contains a non-hex-digit character"
            ))
        })?;
        bytes.push(byte);
    }
    Ok(bytes)
}

// ── Date / time parsers ──────────────────────────────────────────────────────

/// Parses a PostgreSQL `DATE` text value (`YYYY-MM-DD`, PostgreSQL's default
/// `DateStyle=ISO` format) into days since the Unix epoch, matching
/// `crate::types`'s `Value::Date` convention and its exact JDN-offset
/// arithmetic.
fn parse_date(s: &str) -> Result<i32, PgError> {
    let date = parse_ymd(s)?;
    Ok(date.to_julian_day() - UNIX_EPOCH_JDN)
}

/// Parses a bare `YYYY-MM-DD` date, with no time-of-day or offset.
fn parse_ymd(s: &str) -> Result<Date, PgError> {
    let parts: Vec<&str> = s.split('-').collect();
    let (year_str, month_str, day_str) = match parts.as_slice() {
        [y, m, d] => (*y, *m, *d),
        _ => {
            return Err(PgError::TypeConversion(format!(
                "invalid date text {s:?}: expected 'YYYY-MM-DD'"
            )));
        }
    };
    let year: i32 = year_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid date year in {s:?}: {e}")))?;
    let month_num: u8 = month_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid date month in {s:?}: {e}")))?;
    let day: u8 = day_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid date day in {s:?}: {e}")))?;
    let month = Month::try_from(month_num)
        .map_err(|e| PgError::TypeConversion(format!("invalid date month in {s:?}: {e}")))?;
    Date::from_calendar_date(year, month, day)
        .map_err(|e| PgError::TypeConversion(format!("invalid date {s:?}: {e}")))
}

/// Parses a PostgreSQL `TIME` (no time zone) text value (`HH:MM:SS` or
/// `HH:MM:SS.ffffff`) into microseconds since midnight, matching
/// `crate::types`'s `Value::Time` convention and its exact `(h, m, s, ns)`
/// arithmetic.
fn parse_time(s: &str) -> Result<i64, PgError> {
    let t = parse_hms(s)?;
    let (h, m, sec, ns) = t.as_hms_nano();
    let us = i64::from(h) * 3_600_000_000
        + i64::from(m) * 60_000_000
        + i64::from(sec) * 1_000_000
        + i64::from(ns) / 1_000;
    Ok(us)
}

/// Parses a bare `HH:MM:SS[.ffffff]` time-of-day, with no date or offset.
fn parse_hms(s: &str) -> Result<Time, PgError> {
    let parts: Vec<&str> = s.split(':').collect();
    let (hour_str, minute_str, sec_field) = match parts.as_slice() {
        [h, m, sec] => (*h, *m, *sec),
        _ => {
            return Err(PgError::TypeConversion(format!(
                "invalid time text {s:?}: expected 'HH:MM:SS[.ffffff]'"
            )));
        }
    };
    let hour: u8 = hour_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid time hour in {s:?}: {e}")))?;
    let minute: u8 = minute_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid time minute in {s:?}: {e}")))?;
    let (sec_str, micros) = split_fractional_seconds(sec_field)?;
    let second: u8 = sec_str
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid time second in {s:?}: {e}")))?;
    Time::from_hms_micro(hour, minute, second, micros)
        .map_err(|e| PgError::TypeConversion(format!("invalid time {s:?}: {e}")))
}

/// Splits a `"SS"` or `"SS.ffffff"` seconds field into its whole-seconds
/// text and a microsecond count. PostgreSQL trims trailing zero digits from
/// the fractional part (1 to 6 digits may appear), so a shorter fraction is
/// right-padded with zeros to full microsecond precision.
fn split_fractional_seconds(s: &str) -> Result<(&str, u32), PgError> {
    let Some((sec, frac)) = s.split_once('.') else {
        return Ok((s, 0));
    };
    if frac.is_empty() || frac.len() > 6 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(PgError::TypeConversion(format!(
            "invalid fractional seconds {s:?}: expected 1 to 6 digits after '.'"
        )));
    }
    let mut padded = frac.to_string();
    padded.push_str(&"0".repeat(6 - frac.len()));
    let micros: u32 = padded
        .parse()
        .map_err(|e| PgError::TypeConversion(format!("invalid fractional seconds {s:?}: {e}")))?;
    Ok((sec, micros))
}

/// Parses a PostgreSQL `TIMESTAMP` (no time zone) text value
/// (`YYYY-MM-DD HH:MM:SS[.ffffff]`) into microseconds since the Unix epoch,
/// treating the value as UTC — matching `crate::types`'s `TIMESTAMP` arm
/// exactly, including its `unix_timestamp_nanos() / 1_000` truncating cast.
fn parse_timestamp(s: &str) -> Result<i64, PgError> {
    let (date_str, time_str) = split_date_and_rest(s)?;
    let date = parse_ymd(date_str)?;
    let time = parse_hms(time_str)?;
    let dt = PrimitiveDateTime::new(date, time);
    let us = dt.assume_utc().unix_timestamp_nanos() / 1_000;
    #[allow(clippy::cast_possible_truncation)]
    Ok(us as i64)
}

/// Parses a PostgreSQL `TIMESTAMPTZ` text value
/// (`YYYY-MM-DD HH:MM:SS[.ffffff]±HH[:MM[:SS]]`) into microseconds since the
/// Unix epoch — matching `crate::types`'s `TIMESTAMPTZ` arm exactly,
/// including its `unix_timestamp_nanos() / 1_000` truncating cast. Handles
/// all three offset widths PostgreSQL's ISO `DateStyle` can emit: `±HH`,
/// `±HH:MM`, and `±HH:MM:SS` (see [`parse_offset`]).
fn parse_timestamptz(s: &str) -> Result<i64, PgError> {
    let (date_str, rest) = split_date_and_rest(s)?;
    let date = parse_ymd(date_str)?;
    let (time_str, offset) = split_time_and_offset(rest)?;
    let time = parse_hms(time_str)?;
    let dt = PrimitiveDateTime::new(date, time).assume_offset(offset);
    let us = dt.unix_timestamp_nanos() / 1_000;
    #[allow(clippy::cast_possible_truncation)]
    Ok(us as i64)
}

/// Splits `"YYYY-MM-DD <rest>"` on its single mandatory space separator.
fn split_date_and_rest(s: &str) -> Result<(&str, &str), PgError> {
    s.split_once(' ').ok_or_else(|| {
        PgError::TypeConversion(format!(
            "invalid timestamp text {s:?}: expected a space between date and time"
        ))
    })
}

/// Splits a `"HH:MM:SS[.ffffff]±HH[:MM[:SS]]"` timestamptz time-with-offset
/// field into the bare time-of-day text and the parsed [`UtcOffset`].
///
/// PostgreSQL can emit the offset in three widths depending on the zone
/// (`+00`, `-05`, `+05:30`, `+05:30:00`, ...); rather than attempting one
/// fixed format and falling back to another, this locates the offset's sign
/// character (unambiguous: nothing earlier in the string can contain
/// `+`/`-`) and then branches on how many `:`-separated components follow
/// it, in [`parse_offset`], so all three widths are handled by one routine.
fn split_time_and_offset(s: &str) -> Result<(&str, UtcOffset), PgError> {
    let sign_pos = s.find(['+', '-']).ok_or_else(|| {
        PgError::TypeConversion(format!(
            "invalid timestamptz text {s:?}: missing '+'/'-' UTC offset"
        ))
    })?;
    let (time_str, signed_offset) = s.split_at(sign_pos);
    let negative = signed_offset.starts_with('-');
    let offset = parse_offset(&signed_offset[1..], negative, s)?;
    Ok((time_str, offset))
}

/// Parses the digits of a UTC offset (sign already stripped by the caller)
/// in `HH`, `HH:MM`, or `HH:MM:SS` form. `original` is the full source text,
/// used only for error messages.
fn parse_offset(digits: &str, negative: bool, original: &str) -> Result<UtcOffset, PgError> {
    let component = |p: &str| -> Result<i8, PgError> {
        p.parse::<i8>().map_err(|e| {
            PgError::TypeConversion(format!(
                "invalid UTC offset in timestamptz text {original:?}: {e}"
            ))
        })
    };
    let parts: Vec<&str> = digits.split(':').collect();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [h] => (component(h)?, 0, 0),
        [h, m] => (component(h)?, component(m)?, 0),
        [h, m, sec] => (component(h)?, component(m)?, component(sec)?),
        _ => {
            return Err(PgError::TypeConversion(format!(
                "invalid UTC offset in timestamptz text {original:?}: expected \
                 '±HH', '±HH:MM', or '±HH:MM:SS'"
            )));
        }
    };
    let sign: i8 = if negative { -1 } else { 1 };
    UtcOffset::from_hms(sign * hours, sign * minutes, sign * seconds).map_err(|e| {
        PgError::TypeConversion(format!(
            "invalid UTC offset in timestamptz text {original:?}: {e}"
        ))
    })
}

/// Parses a PostgreSQL `UUID` text value into [`Value::Uuid`]'s big-endian
/// `u128` representation.
fn parse_uuid(s: &str) -> Result<u128, PgError> {
    uuid::Uuid::parse_str(s)
        .map(|u| u.as_u128())
        .map_err(|e| PgError::TypeConversion(format!("invalid uuid text {s:?}: {e}")))
}

// ── Binary-format value decoding ─────────────────────────────────────────────

/// Decodes a [`TupleColumn::Binary`] cell's raw wire bytes per its resolved
/// PostgreSQL type, mirroring `crate::types::extract_value`'s OID ->
/// [`Value`] conventions type-by-type (see this module's [relationship to
/// `crate::types`](self#relationship-to-cratetypes)) — but, since there is
/// no live `tokio_postgres::Row` to call `Row::try_get` on here (only raw
/// bytes plus an OID), every scalar type is instead decoded by calling
/// `<T as FromSql>::from_sql(&ty, raw)` directly via the [`decode_binary`]
/// helper.
///
/// An OID that [`Type::from_oid`] does not recognize cannot be decoded at
/// all — there is no live connection to ask what it even is, unlike
/// `crate::types::extract_value`'s final fallback, which always has a
/// `Statement`/`Row` in hand to name it. Unlike [`text_to_value`]'s
/// equivalent fallback (`Value::Text(s.to_string())`, a real,
/// already-meaningful string), raw binary bytes for an unrecognized type
/// are not text at all — lossily reinterpreting them as UTF-8 (replacing
/// invalid sequences rather than failing outright) at least keeps the
/// payload legible for diagnostic purposes, which is the closest available
/// analogue to `crate::types`'s `<opaque:name>` placeholder now that there
/// is no name to put in it.
fn binary_to_value(type_oid: u32, raw: &[u8]) -> Result<Value, PgError> {
    let Some(ty) = Type::from_oid(type_oid) else {
        return Ok(Value::Text(String::from_utf8_lossy(raw).into_owned()));
    };
    if let Kind::Array(elem_ty) = ty.kind() {
        return binary_array_to_value(&ty, elem_ty, raw);
    }
    match ty {
        Type::BOOL => decode_binary::<bool>(&ty, raw).map(Value::Bool),
        Type::INT2 => decode_binary::<i16>(&ty, raw).map(|n| Value::I64(i64::from(n))),
        Type::INT4 => decode_binary::<i32>(&ty, raw).map(|n| Value::I64(i64::from(n))),
        Type::INT8 => decode_binary::<i64>(&ty, raw).map(Value::I64),
        Type::FLOAT4 => decode_binary::<f32>(&ty, raw).map(|f| Value::F64(f64::from(f))),
        Type::FLOAT8 => decode_binary::<f64>(&ty, raw).map(Value::F64),
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME => {
            decode_binary::<String>(&ty, raw).map(Value::Text)
        }
        Type::BYTEA => decode_binary::<Vec<u8>>(&ty, raw).map(Value::Blob),
        Type::DATE => decode_binary::<Date>(&ty, raw).map(date_to_value),
        Type::TIMESTAMP => {
            decode_binary::<PrimitiveDateTime>(&ty, raw).map(primitive_datetime_to_value)
        }
        Type::TIMESTAMPTZ => {
            decode_binary::<OffsetDateTime>(&ty, raw).map(offset_datetime_to_value)
        }
        Type::TIME => decode_binary::<Time>(&ty, raw).map(time_of_day_to_value),
        Type::UUID => decode_binary::<uuid::Uuid>(&ty, raw).map(|u| Value::Uuid(u.as_u128())),
        Type::JSON => decode_json_element(raw, false).map(Value::Json),
        Type::JSONB => decode_json_element(raw, true).map(Value::Json),
        Type::NUMERIC => decode_binary_numeric(raw).map(Value::Decimal),
        Type::INTERVAL => decode_binary_interval(raw).map(Value::Text),
        ref other => {
            if <String as FromSql>::accepts(other) {
                decode_binary::<String>(other, raw).map(Value::Text)
            } else {
                Ok(Value::Text(format!("<opaque:{}>", other.name())))
            }
        }
    }
}

/// Calls `<T as FromSql>::from_sql(ty, raw)`, translating a decode failure
/// into [`PgError::TypeConversion`] instead of the boxed `dyn Error`
/// `FromSql` itself returns.
fn decode_binary<'a, T: FromSql<'a>>(ty: &Type, raw: &'a [u8]) -> Result<T, PgError> {
    T::from_sql(ty, raw).map_err(|e| {
        PgError::TypeConversion(format!(
            "invalid binary payload for type {} (OID {}): {e}",
            ty.name(),
            ty.oid()
        ))
    })
}

/// Converts a decoded [`time::Date`] to [`Value::Date`]'s
/// days-since-Unix-epoch convention — the same arithmetic [`parse_date`]
/// uses on the text path.
fn date_to_value(d: Date) -> Value {
    Value::Date(d.to_julian_day() - UNIX_EPOCH_JDN)
}

/// Converts a decoded [`time::Time`] to [`Value::Time`]'s
/// microseconds-since-midnight convention — the same arithmetic
/// [`parse_time`] uses on the text path.
fn time_of_day_to_value(t: Time) -> Value {
    let (h, m, s, ns) = t.as_hms_nano();
    let us = i64::from(h) * 3_600_000_000
        + i64::from(m) * 60_000_000
        + i64::from(s) * 1_000_000
        + i64::from(ns) / 1_000;
    Value::Time(us)
}

/// Converts a decoded [`time::PrimitiveDateTime`] (treated as UTC, matching
/// PostgreSQL's own `TIMESTAMP` semantics) to [`Value::Timestamp`]'s
/// microseconds-since-Unix-epoch convention — the same arithmetic
/// [`parse_timestamp`] uses on the text path.
fn primitive_datetime_to_value(dt: PrimitiveDateTime) -> Value {
    let us = dt.assume_utc().unix_timestamp_nanos() / 1_000;
    #[allow(clippy::cast_possible_truncation)]
    Value::Timestamp(us as i64)
}

/// Converts a decoded [`time::OffsetDateTime`] to [`Value::Timestamp`]'s
/// microseconds-since-Unix-epoch convention — the same arithmetic
/// [`parse_timestamptz`] uses on the text path.
fn offset_datetime_to_value(dt: OffsetDateTime) -> Value {
    let us = dt.unix_timestamp_nanos() / 1_000;
    #[allow(clippy::cast_possible_truncation)]
    Value::Timestamp(us as i64)
}

/// Decodes a binary `JSON`/`JSONB` payload's raw bytes into the string that
/// becomes [`Value::Json`].
///
/// `JSON`'s binary wire representation is simply its UTF-8 text verbatim.
/// `JSONB`'s binary wire representation additionally prepends a one-byte
/// format version number (`0x01`, the only version PostgreSQL has ever
/// shipped) before that same UTF-8 text; `strip_version_byte` selects which
/// of the two shapes `raw` is in, erroring if asked to strip a version byte
/// that either isn't there (empty payload) or isn't `1`.
fn decode_json_element(raw: &[u8], strip_version_byte: bool) -> Result<String, PgError> {
    let payload = if strip_version_byte {
        let Some((&version, rest)) = raw.split_first() else {
            return Err(PgError::TypeConversion(
                "JSONB binary payload is empty (missing the leading version byte)".to_string(),
            ));
        };
        if version != 1 {
            return Err(PgError::TypeConversion(format!(
                "JSONB binary payload has an unsupported version byte {version} (expected 1)"
            )));
        }
        rest
    } else {
        raw
    };
    String::from_utf8(payload.to_vec()).map_err(|e| {
        PgError::TypeConversion(format!(
            "{} binary payload is not valid UTF-8: {e}",
            if strip_version_byte { "JSONB" } else { "JSON" }
        ))
    })
}

// ── Binary NUMERIC / INTERVAL decoders ──────────────────────────────────────

//
// PostgreSQL's NUMERIC and INTERVAL types have no `tokio_postgres::FromSql`
// impl for any convenient built-in Rust type, so (matching
// `crate::types::decode_pg_numeric`/`decode_pg_interval`'s approach for the
// ordinary query path, but independently implemented — see this module's
// [relationship to `crate::types`](self#relationship-to-cratetypes)) both
// wire formats are decoded here by hand, one big-endian field at a time, via
// the bounds-checked [`split_checked`]/`read_be_*` helpers below. Every read
// is bounds-checked and returns [`PgError::TypeConversion`] on truncated
// input rather than indexing/slicing (which would panic on attacker- or
// corrupted-server-controlled bytes).

/// Splits `buf` into its first `n` bytes and the remainder, erroring
/// (rather than panicking, unlike [`slice::split_at`] alone) if `buf` has
/// fewer than `n` bytes left. `what` names the field being read, for the
/// error message.
fn split_checked<'a>(buf: &'a [u8], n: usize, what: &str) -> Result<(&'a [u8], &'a [u8]), PgError> {
    if buf.len() < n {
        return Err(PgError::TypeConversion(format!(
            "{what} truncated: expected {n} more byte(s), found {}",
            buf.len()
        )));
    }
    Ok(buf.split_at(n))
}

/// Reads a big-endian `i16` from the front of `*buf`, advancing `*buf` past
/// it.
fn read_be_i16(buf: &mut &[u8], what: &str) -> Result<i16, PgError> {
    let (head, tail) = split_checked(buf, 2, what)?;
    *buf = tail;
    Ok(i16::from_be_bytes([head[0], head[1]]))
}

/// Reads a big-endian `u16` from the front of `*buf`, advancing `*buf` past
/// it.
fn read_be_u16(buf: &mut &[u8], what: &str) -> Result<u16, PgError> {
    let (head, tail) = split_checked(buf, 2, what)?;
    *buf = tail;
    Ok(u16::from_be_bytes([head[0], head[1]]))
}

/// Reads a big-endian `i32` from the front of `*buf`, advancing `*buf` past
/// it.
fn read_be_i32(buf: &mut &[u8], what: &str) -> Result<i32, PgError> {
    let (head, tail) = split_checked(buf, 4, what)?;
    *buf = tail;
    Ok(i32::from_be_bytes([head[0], head[1], head[2], head[3]]))
}

/// Reads a big-endian `u32` from the front of `*buf`, advancing `*buf` past
/// it.
fn read_be_u32(buf: &mut &[u8], what: &str) -> Result<u32, PgError> {
    let (head, tail) = split_checked(buf, 4, what)?;
    *buf = tail;
    Ok(u32::from_be_bytes([head[0], head[1], head[2], head[3]]))
}

/// Reads a big-endian `i64` from the front of `*buf`, advancing `*buf` past
/// it.
fn read_be_i64(buf: &mut &[u8], what: &str) -> Result<i64, PgError> {
    let (head, tail) = split_checked(buf, 8, what)?;
    *buf = tail;
    Ok(i64::from_be_bytes([
        head[0], head[1], head[2], head[3], head[4], head[5], head[6], head[7],
    ]))
}

/// Decodes PostgreSQL's `NUMERIC` **binary** wire format into the same
/// canonical decimal string PostgreSQL's text output would produce (e.g.
/// `"123.450"`, `"-99.99"`, `"NaN"`).
///
/// Wire format (all big-endian, network byte order):
/// - `i16 ndigits` — count of base-10000 digit groups that follow
/// - `i16 weight`  — the base-10000 exponent of the first digit group
///   (weight 0 means that group covers `10000^0..10000^1`)
/// - `u16 sign`    — `0x0000` positive, `0x4000` negative, `0xC000` NaN
/// - `u16 dscale`  — the number of digits to display after the decimal
///   point
/// - `ndigits` digit groups, each a `u16` in `0..=9999`
///
/// This is a from-scratch, independent re-implementation of the same wire
/// format `crate::types::decode_pg_numeric` already decodes for the
/// ordinary (non-replication) query path — see this module's [relationship
/// to `crate::types`](self#relationship-to-cratetypes) for why the two
/// intentionally do not share an implementation.
fn decode_binary_numeric(raw: &[u8]) -> Result<String, PgError> {
    let mut buf = raw;
    let ndigits = read_be_i16(&mut buf, "NUMERIC ndigits")?;
    let weight = read_be_i16(&mut buf, "NUMERIC weight")?;
    let sign = read_be_u16(&mut buf, "NUMERIC sign")?;
    let dscale = read_be_u16(&mut buf, "NUMERIC dscale")?;
    const SIGN_NAN: u16 = 0xC000;
    const SIGN_NEG: u16 = 0x4000;
    if sign == SIGN_NAN {
        return Ok("NaN".to_string());
    }
    let ndigits = usize::try_from(ndigits).map_err(|_| {
        PgError::TypeConversion(format!(
            "NUMERIC binary payload has a negative ndigits count ({ndigits})"
        ))
    })?;
    let mut digits: Vec<u16> = Vec::with_capacity(ndigits);
    for _ in 0..ndigits {
        let digit = read_be_u16(&mut buf, "NUMERIC digit group")?;
        if digit > 9999 {
            return Err(PgError::TypeConversion(format!(
                "NUMERIC binary payload has an out-of-range digit group \
                 ({digit}, expected 0..=9999)"
            )));
        }
        digits.push(digit);
    }
    let dscale = usize::from(dscale);
    let int_groups = usize::try_from((i32::from(weight) + 1).max(0)).unwrap_or(0);
    let mut integer_part = String::new();
    for (i, digit) in (0..int_groups).map(|i| (i, digits.get(i).copied().unwrap_or(0))) {
        if i == 0 {
            integer_part.push_str(&digit.to_string());
        } else {
            integer_part.push_str(&format!("{digit:04}"));
        }
    }
    if integer_part.is_empty() {
        integer_part.push('0');
    }
    let mut fraction_part = String::new();
    if dscale > 0 {
        let mut remaining_scale = dscale;
        let frac_groups_needed = dscale.div_ceil(4);
        for i in 0..frac_groups_needed {
            let digit = digits.get(int_groups + i).copied().unwrap_or(0);
            let group_str = format!("{digit:04}");
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

/// Decodes PostgreSQL's `INTERVAL` **binary** wire format (16 bytes: a
/// big-endian `i64` microsecond count, a big-endian `i32` day count, and a
/// big-endian `i32` month count) into a human-readable string.
///
/// # Chosen text convention
///
/// [`text_to_value`]'s `INTERVAL` arm passes PostgreSQL's own text output
/// through verbatim (e.g. `"1 year 2 mons 3 days 01:30:00"`) since it is
/// already human-readable and there is no reason to reparse it. A *binary*
/// `INTERVAL`, however, arrives as three raw integers with no textual
/// representation at all, so this function must synthesize one from
/// scratch. Rather than inventing a third convention, it reuses the exact
/// condensed `"{years}Y {months}M {days}D {HH:MM:SS[.ffffff]}"`-style format
/// `crate::types::decode_pg_interval` already produces for the ordinary
/// (non-replication) binary query path — e.g. `"1Y 2M 3D 01:30:00"` — so
/// that the two *binary* INTERVAL decode paths in this crate at least agree
/// on one convention, even though (per this module's documented policy)
/// they do not share an implementation. Zero-valued components are omitted;
/// an all-zero interval renders as `"00:00:00"`.
fn decode_binary_interval(raw: &[u8]) -> Result<String, PgError> {
    let mut buf = raw;
    let micros = read_be_i64(&mut buf, "INTERVAL microseconds")?;
    let days = read_be_i32(&mut buf, "INTERVAL days")?;
    let months = read_be_i32(&mut buf, "INTERVAL months")?;
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

// ── Binary array decoding ────────────────────────────────────────────────────

/// Decodes a binary array cell (any OID whose [`Type::kind`] is
/// [`Kind::Array`]) given the array's own [`Type`] (`ty`, needed because
/// `tokio_postgres`'s `Vec<Option<T>>: FromSql` impl expects the *array*
/// type, not the element type, and extracts the element type back out of it
/// internally) and its already-extracted element [`Type`] (`elem_ty`).
fn binary_array_to_value(ty: &Type, elem_ty: &Type, raw: &[u8]) -> Result<Value, PgError> {
    let Some(element_type) = array_element_type(elem_ty) else {
        return Ok(Value::Array(decode_opaque_binary_array(raw)?));
    };
    decode_typed_binary_array(ty, element_type, raw)
}

/// Decodes a binary array whose element type has a known
/// [`ArrayElementType`] mapping, covering exactly the same OIDs
/// `crate::types::extract_value`'s array arms do (`BOOL_ARRAY` through
/// `JSONB_ARRAY` — see this module's doc comment). `NUMERIC[]`/
/// `INTERVAL[]` binary arrays are deliberately *not* covered here even
/// though `NUMERIC` itself maps to [`ArrayElementType::Decimal`] (that
/// mapping exists for the *text* array path, which has no trouble with it
/// — see [`decode_text_array`]): `crate::types::extract_value` has no
/// `NUMERIC_ARRAY`/`INTERVAL_ARRAY` arm either, so this module has nothing
/// to mirror there, and falls back to the same structural, per-element
/// raw-bytes decoding as a wholly unmapped element type.
fn decode_typed_binary_array(
    ty: &Type,
    element_type: ArrayElementType,
    raw: &[u8],
) -> Result<Value, PgError> {
    let values: Vec<Value> = match element_type {
        ArrayElementType::Bool => decode_binary::<Vec<Option<bool>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, Value::Bool))
            .collect(),
        ArrayElementType::Int2 => decode_binary::<Vec<Option<i16>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, |n| Value::I64(i64::from(n))))
            .collect(),
        ArrayElementType::Int4 => decode_binary::<Vec<Option<i32>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, |n| Value::I64(i64::from(n))))
            .collect(),
        ArrayElementType::Int8 => decode_binary::<Vec<Option<i64>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, Value::I64))
            .collect(),
        ArrayElementType::Float4 => decode_binary::<Vec<Option<f32>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, |f| Value::F64(f64::from(f))))
            .collect(),
        ArrayElementType::Float8 => decode_binary::<Vec<Option<f64>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, Value::F64))
            .collect(),
        ArrayElementType::Text => decode_binary::<Vec<Option<String>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, Value::Text))
            .collect(),
        ArrayElementType::Bytea => decode_binary::<Vec<Option<Vec<u8>>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, Value::Blob))
            .collect(),
        ArrayElementType::Date => decode_binary::<Vec<Option<Date>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, date_to_value))
            .collect(),
        ArrayElementType::Time => decode_binary::<Vec<Option<Time>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, time_of_day_to_value))
            .collect(),
        ArrayElementType::Timestamp => decode_binary::<Vec<Option<PrimitiveDateTime>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, primitive_datetime_to_value))
            .collect(),
        ArrayElementType::TimestampTz => decode_binary::<Vec<Option<OffsetDateTime>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, offset_datetime_to_value))
            .collect(),
        ArrayElementType::Uuid => decode_binary::<Vec<Option<uuid::Uuid>>>(ty, raw)?
            .into_iter()
            .map(|opt| opt.map_or(Value::Null, |u| Value::Uuid(u.as_u128())))
            .collect(),
        ArrayElementType::Json => decode_json_array(raw, false)?,
        ArrayElementType::Jsonb => decode_json_array(raw, true)?,
        _ => decode_opaque_binary_array(raw)?,
    };
    Ok(Value::TypedArray {
        element_type,
        values,
    })
}

/// Structural (element-type-oblivious) binary array parser: walks
/// PostgreSQL's array wire format far enough to identify each element's raw
/// byte span (or that it is NULL), without attempting to interpret those
/// bytes — used where the element type has no dedicated decoder in this
/// module (see [`decode_opaque_binary_array`], [`decode_json_array`]).
///
/// Wire format (all big-endian, network byte order):
/// - `i32 ndim`        — number of array dimensions
/// - `i32 flags`       — bit 0 set if the array contains any NULL element
///   (ignored here: each element already carries its own NULL marker via a
///   `-1` length prefix)
/// - `u32 element_oid` — the array's element type OID (ignored here; the
///   caller already knows the element type from the column schema)
/// - `ndim` × (`i32 dim_size`, `i32 lower_bound`) — one pair per dimension
///   (the lower bound is ignored: this module has no use for non-default
///   array lower bounds)
/// - `dim_size` × (`i32 elem_len`, `elem_len` bytes) — `elem_len == -1`
///   marks a NULL element (no following bytes)
///
/// Only 0- or 1-dimensional arrays are supported — the same limitation
/// `tokio_postgres`'s own `Vec<Option<T>>: FromSql` impl has (it rejects
/// arrays with more than one dimension), which every OID this module *does*
/// have a dedicated decoder for relies on. A higher-dimensional array
/// reaching this structural fallback is rejected with
/// [`PgError::TypeConversion`] rather than guessing at a nesting shape.
///
/// Never trusts the claimed `dim_size` for pre-allocation — a maliciously
/// large count in a short buffer would otherwise let a few header bytes
/// request an enormous allocation before any of the (nonexistent) element
/// bytes are even read. Growth is instead purely data-driven: the loop can
/// never iterate more times than there are actual length-prefixed elements
/// in `raw`, since each iteration consumes real bytes and errors out via
/// [`split_checked`] once they run out.
fn raw_array_elements(raw: &[u8]) -> Result<Vec<Option<Vec<u8>>>, PgError> {
    let mut buf = raw;
    let ndim = read_be_i32(&mut buf, "array ndim")?;
    let _flags = read_be_i32(&mut buf, "array flags")?;
    let _element_oid = read_be_u32(&mut buf, "array element OID")?;
    if ndim == 0 {
        return Ok(Vec::new());
    }
    if ndim != 1 {
        return Err(PgError::TypeConversion(format!(
            "binary array structural decoding only supports 0- or \
             1-dimensional arrays, found {ndim} dimension(s)"
        )));
    }
    let dim_size = read_be_i32(&mut buf, "array dimension size")?;
    let _lower_bound = read_be_i32(&mut buf, "array dimension lower bound")?;
    let dim_size = usize::try_from(dim_size).map_err(|_| {
        PgError::TypeConversion(format!("array dimension size is negative ({dim_size})"))
    })?;
    let mut elements = Vec::new();
    for _ in 0..dim_size {
        let elem_len = read_be_i32(&mut buf, "array element length")?;
        if elem_len < 0 {
            elements.push(None);
            continue;
        }
        let elem_len = usize::try_from(elem_len).map_err(|_| {
            PgError::TypeConversion(format!("array element length is negative ({elem_len})"))
        })?;
        let (elem_bytes, rest) = split_checked(buf, elem_len, "array element data")?;
        buf = rest;
        elements.push(Some(elem_bytes.to_vec()));
    }
    Ok(elements)
}

/// Fallback for a binary array element type with no [`ArrayElementType`]
/// mapping at all (an enum array, a composite array, or some other exotic
/// type this module has no scalar binary decoder for): each element's raw,
/// still-undecoded bytes become a [`Value::Blob`] (there is no sound way to
/// interpret them further without knowing the concrete element type), and
/// each NULL element becomes [`Value::Null`]. This is a deliberate choice
/// between the two the array-decoding requirements call out (best-effort
/// per-element decoding vs. an outright error) — see [`raw_array_elements`]
/// for the wire-format details this relies on.
fn decode_opaque_binary_array(raw: &[u8]) -> Result<Vec<Value>, PgError> {
    Ok(raw_array_elements(raw)?
        .into_iter()
        .map(|opt| opt.map_or(Value::Null, Value::Blob))
        .collect())
}

/// Decodes a binary `JSON[]`/`JSONB[]` array. Each element needs its own
/// [`decode_json_element`] call (to strip a per-element `JSONB` version
/// byte), which does not fit `tokio_postgres`'s generic `Vec<Option<T>>:
/// FromSql` machinery (no `T: FromSql` in this crate's dependency graph
/// decodes "UTF-8 text with an optional leading version byte" — `String`'s
/// `FromSql` impl always decodes plain text with no version-byte handling),
/// so this walks the same structural array framing
/// [`decode_opaque_binary_array`] does and decodes each element explicitly
/// instead.
fn decode_json_array(raw: &[u8], strip_version_byte: bool) -> Result<Vec<Value>, PgError> {
    raw_array_elements(raw)?
        .into_iter()
        .map(|opt| match opt {
            Some(bytes) => decode_json_element(&bytes, strip_version_byte).map(Value::Json),
            None => Ok(Value::Null),
        })
        .collect()
}

// ── Array element type mapping (shared: binary + text array decoding) ──────

/// Maps a PostgreSQL scalar element [`Type`] to its
/// [`oxisql_core::ArrayElementType`] tag, used by both binary
/// ([`binary_array_to_value`]) and text ([`decode_text_array`]) array
/// decoding to decide the resulting array's nominal element type (and, for
/// text arrays, whether a flat array can become a [`Value::TypedArray`] at
/// all).
///
/// Returns `None` for any element type this module does not have a
/// specific mapping for (custom/enum/composite/range types, etc.) — the two
/// array-decoding functions each define their own fallback for that case.
/// `NUMERIC` maps to [`ArrayElementType::Decimal`] here (needed by the
/// *text* array path — `NUMERIC`'s text form has no decoding difficulty at
/// all), even though the *binary* array path does not actually implement
/// `NUMERIC[]`/`INTERVAL[]` decoding — see [`decode_typed_binary_array`]'s
/// doc comment.
fn array_element_type(elem_ty: &Type) -> Option<ArrayElementType> {
    match *elem_ty {
        Type::BOOL => Some(ArrayElementType::Bool),
        Type::INT2 => Some(ArrayElementType::Int2),
        Type::INT4 => Some(ArrayElementType::Int4),
        Type::INT8 => Some(ArrayElementType::Int8),
        Type::FLOAT4 => Some(ArrayElementType::Float4),
        Type::FLOAT8 => Some(ArrayElementType::Float8),
        Type::TEXT | Type::VARCHAR => Some(ArrayElementType::Text),
        Type::BYTEA => Some(ArrayElementType::Bytea),
        Type::DATE => Some(ArrayElementType::Date),
        Type::TIME => Some(ArrayElementType::Time),
        Type::TIMESTAMP => Some(ArrayElementType::Timestamp),
        Type::TIMESTAMPTZ => Some(ArrayElementType::TimestampTz),
        Type::UUID => Some(ArrayElementType::Uuid),
        Type::JSON => Some(ArrayElementType::Json),
        Type::JSONB => Some(ArrayElementType::Jsonb),
        Type::NUMERIC => Some(ArrayElementType::Decimal),
        _ => None,
    }
}

// ── Array text-format decoding ───────────────────────────────────────────────

/// One node of the intermediate parse tree produced while scanning a
/// PostgreSQL array-literal text value, before it is converted into
/// [`Value`]s. Kept separate from [`Value`] itself because whether the
/// *outermost* brace group becomes a flat [`Value::TypedArray`] or a plain
/// nested [`Value::Array`] can only be decided once the whole tree is known
/// (see [`decode_text_array`]).
#[derive(Debug, Clone, PartialEq)]
enum ArrayElem {
    /// An unquoted, case-insensitive `NULL` bareword.
    Null,
    /// A leaf element's unescaped source text, not yet parsed against the
    /// element type (that happens in [`array_node_to_value`], since
    /// converting a leaf requires the element type but scanning the array
    /// syntax does not).
    Scalar(String),
    /// A nested `{...}` sub-array.
    Nested(Vec<ArrayElem>),
}

/// Decodes PostgreSQL's array-literal text format (e.g. `"{1,2,3}"`,
/// `"{}"`, `"{NULL,2}"`, quoted/escaped elements, nested multi-dimensional
/// arrays, and an optional leading `[l:u]=` dimension prefix) into a
/// [`Value`], for a resolved element type `elem_ty`.
///
/// This is a real recursive-descent scanner over PostgreSQL's array-literal
/// grammar (braces, comma separators, double-quoted elements with `\"`/`\\`
/// escapes, unquoted barewords, and nested sub-arrays) — not a naive
/// `split(',')`, which cannot distinguish a comma that separates elements
/// from one embedded inside a quoted element or a nested sub-array. Leaf
/// elements are decoded via [`text_to_value`] (with `elem_ty`'s OID),
/// reusing every existing scalar text parser; only the array *syntax*
/// itself is new here.
///
/// A flat (1-dimensional) array whose element type has a known
/// [`array_element_type`] mapping becomes a [`Value::TypedArray`]; a
/// multi-dimensional/nested array, or one whose element type has no known
/// mapping, becomes a plain nested [`Value::Array`] — matching the same
/// convention [`binary_array_to_value`] uses on the binary path.
fn decode_text_array(elem_ty: &Type, s: &str) -> Result<Value, PgError> {
    let mut pos = skip_dimension_prefix(elem_ty, s)?;
    let node = parse_element(elem_ty, s, &mut pos)?;
    skip_ascii_ws(s, &mut pos);
    if pos != s.len() {
        return Err(array_syntax_error(
            elem_ty,
            s,
            &format!("unexpected trailing content at byte offset {pos}"),
        ));
    }
    let ArrayElem::Nested(items) = node else {
        return Err(array_syntax_error(
            elem_ty,
            s,
            "array literal must be a brace-delimited '{...}' group",
        ));
    };
    let is_flat = items.iter().all(|it| !matches!(it, ArrayElem::Nested(_)));
    let values = items
        .into_iter()
        .map(|it| array_node_to_value(elem_ty, it))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(match (is_flat, array_element_type(elem_ty)) {
        (true, Some(element_type)) => Value::TypedArray {
            element_type,
            values,
        },
        _ => Value::Array(values),
    })
}

/// Converts one parsed [`ArrayElem`] node into a [`Value`], recursing for
/// nested sub-arrays (always producing a plain [`Value::Array`] at that
/// level — only [`decode_text_array`]'s outermost call decides whether the
/// *top-level* result gets upgraded to a [`Value::TypedArray`]).
fn array_node_to_value(elem_ty: &Type, node: ArrayElem) -> Result<Value, PgError> {
    match node {
        ArrayElem::Null => Ok(Value::Null),
        ArrayElem::Scalar(text) => text_to_value(elem_ty.oid(), &text),
        ArrayElem::Nested(items) => items
            .into_iter()
            .map(|it| array_node_to_value(elem_ty, it))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
    }
}

/// Builds a [`PgError::TypeConversion`] describing a malformed array
/// literal, always including the element type's OID and a `{s:?}`-quoted
/// snippet of the offending input, per this module's array-decoding error
/// convention.
fn array_syntax_error(elem_ty: &Type, s: &str, detail: &str) -> PgError {
    PgError::TypeConversion(format!(
        "malformed array literal for element type {} (OID {}): {detail} (input: {s:?})",
        elem_ty.name(),
        elem_ty.oid(),
    ))
}

/// Skips an optional PostgreSQL array dimension-bound prefix (`[l:u]`,
/// possibly repeated once per dimension, e.g. `[1:2][1:3]=`) preceding the
/// `{...}` literal proper. The bounds are parsed only enough to find where
/// they end — this module has no use for `l`/`u` themselves (see
/// [`decode_text_array`]'s doc comment) — so they are discarded rather than
/// retained. Returns the byte offset of the first byte after the prefix
/// (i.e. where the `{...}` literal itself should start), which is simply
/// `0` if there is no dimension prefix at all.
fn skip_dimension_prefix(elem_ty: &Type, s: &str) -> Result<usize, PgError> {
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    let mut saw_prefix = false;
    while bytes.get(pos) == Some(&b'[') {
        saw_prefix = true;
        pos += 1;
        pos = skip_signed_digits(bytes, pos).ok_or_else(|| {
            array_syntax_error(
                elem_ty,
                s,
                "invalid dimension lower bound in '[l:u]' prefix",
            )
        })?;
        if bytes.get(pos) != Some(&b':') {
            return Err(array_syntax_error(
                elem_ty,
                s,
                "expected ':' in '[l:u]' dimension prefix",
            ));
        }
        pos += 1;
        pos = skip_signed_digits(bytes, pos).ok_or_else(|| {
            array_syntax_error(
                elem_ty,
                s,
                "invalid dimension upper bound in '[l:u]' prefix",
            )
        })?;
        if bytes.get(pos) != Some(&b']') {
            return Err(array_syntax_error(
                elem_ty,
                s,
                "expected ']' to close a '[l:u]' dimension prefix",
            ));
        }
        pos += 1;
    }
    if saw_prefix {
        if bytes.get(pos) != Some(&b'=') {
            return Err(array_syntax_error(
                elem_ty,
                s,
                "expected '=' after the '[l:u]' dimension prefix(es)",
            ));
        }
        pos += 1;
    }
    Ok(pos)
}

/// Skips an optional leading `-` followed by one or more ASCII digits,
/// starting at `bytes[pos]`. Returns `None` (rather than panicking or
/// silently accepting zero digits) if there is no digit at all after the
/// optional sign.
fn skip_signed_digits(bytes: &[u8], mut pos: usize) -> Option<usize> {
    if bytes.get(pos) == Some(&b'-') {
        pos += 1;
    }
    let start = pos;
    while matches!(bytes.get(pos), Some(b'0'..=b'9')) {
        pos += 1;
    }
    if pos == start {
        None
    } else {
        Some(pos)
    }
}

/// Advances `*pos` past any run of ASCII space/tab/newline/carriage-return
/// bytes.
fn skip_ascii_ws(s: &str, pos: &mut usize) {
    let bytes = s.as_bytes();
    while matches!(bytes.get(*pos), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        *pos += 1;
    }
}

/// Parses one array-literal element starting at `s.as_bytes()[*pos]`
/// (after skipping leading whitespace): a nested `{...}` sub-array, a
/// double-quoted string, or an unquoted bareword. Advances `*pos` past
/// what it consumed.
fn parse_element(elem_ty: &Type, s: &str, pos: &mut usize) -> Result<ArrayElem, PgError> {
    skip_ascii_ws(s, pos);
    match s.as_bytes().get(*pos) {
        Some(b'{') => parse_braces(elem_ty, s, pos).map(ArrayElem::Nested),
        Some(b'"') => parse_quoted(elem_ty, s, pos).map(ArrayElem::Scalar),
        Some(_) => parse_bareword(elem_ty, s, pos),
        None => Err(array_syntax_error(
            elem_ty,
            s,
            "expected an array element, found end of input",
        )),
    }
}

/// Parses a `{...}` brace group into its comma-separated elements
/// (recursing via [`parse_element`] for nested sub-arrays), starting at
/// `s.as_bytes()[*pos] == b'{'`. Advances `*pos` to just past the matching
/// closing `}`. An empty `{}` group yields an empty `Vec`.
fn parse_braces(elem_ty: &Type, s: &str, pos: &mut usize) -> Result<Vec<ArrayElem>, PgError> {
    *pos += 1;
    let mut items = Vec::new();
    skip_ascii_ws(s, pos);
    if s.as_bytes().get(*pos) == Some(&b'}') {
        *pos += 1;
        return Ok(items);
    }
    loop {
        items.push(parse_element(elem_ty, s, pos)?);
        skip_ascii_ws(s, pos);
        match s.as_bytes().get(*pos) {
            Some(b',') => {
                *pos += 1;
            }
            Some(b'}') => {
                *pos += 1;
                break;
            }
            Some(&other) => {
                return Err(array_syntax_error(
                    elem_ty,
                    s,
                    &format!(
                        "expected ',' or '}}' at byte offset {}, found {:?}",
                        *pos, other as char
                    ),
                ));
            }
            None => {
                return Err(array_syntax_error(
                    elem_ty,
                    s,
                    "unterminated '{' (missing closing '}')",
                ));
            }
        }
    }
    Ok(items)
}

/// Parses a double-quoted array element starting at
/// `s.as_bytes()[*pos] == b'"'`, unescaping `\"` and `\\` (the only two
/// escape sequences PostgreSQL's array-literal quoting uses) — whitespace
/// inside the quotes is preserved verbatim, unlike an unquoted bareword
/// (see [`parse_bareword`]). Advances `*pos` to just past the closing `"`.
fn parse_quoted(elem_ty: &Type, s: &str, pos: &mut usize) -> Result<String, PgError> {
    *pos += 1;
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut segment_start = *pos;
    loop {
        match bytes.get(*pos) {
            None => {
                return Err(array_syntax_error(
                    elem_ty,
                    s,
                    "unterminated quoted array element (missing closing '\"')",
                ));
            }
            Some(b'"') => {
                out.push_str(&s[segment_start..*pos]);
                *pos += 1;
                return Ok(out);
            }
            Some(b'\\') => {
                out.push_str(&s[segment_start..*pos]);
                *pos += 1;
                match bytes.get(*pos) {
                    Some(b'"') => out.push('"'),
                    Some(b'\\') => out.push('\\'),
                    _ => {
                        return Err(array_syntax_error(
                            elem_ty,
                            s,
                            "invalid escape sequence in a quoted array element \
                             (expected \\\" or \\\\)",
                        ));
                    }
                }
                *pos += 1;
                segment_start = *pos;
            }
            Some(_) => *pos += 1,
        }
    }
}

/// Parses an unquoted array element: everything up to (but not including)
/// the next unescaped `,` or `}`, with leading/trailing ASCII whitespace
/// trimmed. A bareword spelling `NULL` case-insensitively becomes
/// [`ArrayElem::Null`] (a quoted `"NULL"` is never treated as SQL NULL —
/// only reached via [`parse_quoted`], which always returns
/// [`ArrayElem::Scalar`]).
fn parse_bareword(elem_ty: &Type, s: &str, pos: &mut usize) -> Result<ArrayElem, PgError> {
    let bytes = s.as_bytes();
    let start = *pos;
    loop {
        match bytes.get(*pos) {
            None | Some(b',') | Some(b'}') => break,
            Some(b'{') => {
                return Err(array_syntax_error(
                    elem_ty,
                    s,
                    "unexpected '{' inside an unquoted array element",
                ));
            }
            Some(_) => *pos += 1,
        }
    }
    let trimmed = s[start..*pos].trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    if trimmed.is_empty() {
        return Err(array_syntax_error(
            elem_ty,
            s,
            "empty unquoted array element",
        ));
    }
    if trimmed.eq_ignore_ascii_case("null") {
        Ok(ArrayElem::Null)
    } else {
        Ok(ArrayElem::Scalar(trimmed.to_string()))
    }
}

#[cfg(test)]
mod tests;
