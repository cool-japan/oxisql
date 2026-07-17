// Copyright 2026 COOLJAPAN OU (Team KitaSan)
// SPDX-License-Identifier: Apache-2.0

use crate::LimboError::InvalidModifier;
use crate::Result;
use crate::{types::Value, vdbe::Register};
use chrono::{
    DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Timelike, Utc,
};

/// Execution of date/time/datetime functions
#[inline(always)]
pub fn exec_date(values: &[Register]) -> Value {
    exec_datetime(values, DateTimeOutput::Date)
}

#[inline(always)]
pub fn exec_time(values: &[Register]) -> Value {
    exec_datetime(values, DateTimeOutput::Time)
}

#[inline(always)]
pub fn exec_datetime_full(values: &[Register]) -> Value {
    exec_datetime(values, DateTimeOutput::DateTime)
}

#[inline(always)]
pub fn exec_strftime(values: &[Register]) -> Value {
    if values.is_empty() {
        return Value::Null;
    }
    let value = &values[0].get_owned_value();
    let format_str = if matches!(value, Value::Text(_) | Value::Integer(_) | Value::Float(_)) {
        format!("{}", value)
    } else {
        return Value::Null;
    };
    exec_datetime(&values[1..], DateTimeOutput::StrfTime(format_str))
}

enum DateTimeOutput {
    Date,
    Time,
    DateTime,
    StrfTime(String),
    JuliaDay,
}

/// Controls how `add_years_and_months`/`add_one_month` resolve the ambiguity
/// that arises when shifting a date by whole months/years lands on a
/// day-of-month that does not exist in the target month (e.g. 2024-01-31
/// plus 1 month). Mirrors sqlite's 'ceiling'/'floor' modifiers (14/15):
/// <https://www.sqlite.org/lang_datefunc.html>
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum MonthRounding {
    /// Spill over into the following month, landing on a later date.
    /// This is sqlite's documented default behavior.
    #[default]
    Ceiling,
    /// Clamp to the last valid day of the target month instead of spilling
    /// over into the next one.
    Floor,
}

/// Tracks which timezone convention the datetime currently being built is
/// expressed in, so 'utc'/'localtime' modifiers can become no-ops when
/// applied to a value that is already in the requested zone. Without this,
/// e.g. `datetime('now','utc')` would silently double-convert: 'now' is
/// already UTC, but the naive 'utc' modifier assumes its input is local and
/// shifts it by the local offset anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DtZone {
    Utc,
    Local,
}

/// The raw, pre-conversion form of the first argument to date()/time()/
/// datetime()/julianday()/strftime(), kept around so a following 'auto',
/// 'unixepoch', or 'julianday' modifier can reinterpret a numeric argument
/// under a different convention than the default (julian day). Non-numeric
/// arguments (ISO-8601 text, 'now', ...) have nothing to reinterpret: sqlite
/// documents 'auto' as a no-op for those, and 'unixepoch'/'julianday' as
/// errors when they don't immediately follow a numeric time-value.
#[derive(Debug, Clone, Copy)]
enum RawTimeValue {
    Numeric(f64),
    Other,
}

fn raw_time_value(value: &Value) -> RawTimeValue {
    match value {
        Value::Integer(i) => RawTimeValue::Numeric(*i as f64),
        Value::Float(f) => RawTimeValue::Numeric(*f),
        _ => RawTimeValue::Other,
    }
}

/// sqlite's 'auto' modifier range for reinterpreting an out-of-julianday-range
/// number as a unix timestamp instead of returning NULL: "within the range of
/// -210866760000 to 253402300799" (roughly -4713-11-24 to 9999-12-31).
/// <https://www.sqlite.org/lang_datefunc.html>
const AUTO_UNIXEPOCH_MIN: f64 = -210_866_760_000.0;
const AUTO_UNIXEPOCH_MAX: f64 = 253_402_300_799.0;

fn is_auto_unixepoch_value(value: f64) -> bool {
    (AUTO_UNIXEPOCH_MIN..=AUTO_UNIXEPOCH_MAX).contains(&value)
}

/// Interprets `seconds` as a unix timestamp (seconds since 1970-01-01 UTC),
/// for the 'unixepoch' modifier and the unixepoch branch of 'auto'.
fn unix_timestamp_to_datetime(seconds: f64) -> Option<NaiveDateTime> {
    if !seconds.is_finite() {
        return None;
    }
    let secs = seconds.trunc() as i64;
    let frac = seconds.fract();
    let nanos = if frac >= 0.0 {
        (frac * 1_000_000_000.0).round() as u32
    } else {
        0
    };
    DateTime::from_timestamp(secs, nanos).map(|dt| dt.naive_utc())
}

fn exec_datetime(values: &[Register], output_type: DateTimeOutput) -> Value {
    if values.is_empty() {
        let now = parse_naive_date_time(&Value::build_text("now"))
            .expect("'now' always parses successfully");
        return format_dt(now, output_type, false);
    }
    let raw = raw_time_value(values[0].get_owned_value());
    if let Some(mut dt) = parse_naive_date_time(values[0].get_owned_value()) {
        modify_dt(&mut dt, &values[1..], output_type, raw)
    } else if let RawTimeValue::Numeric(n) = raw {
        modify_dt_reinterpret_numeric(n, &values[1..], output_type)
    } else {
        // UTC, not Local: SQLite's `datetime('now')` semantics return the
        // current time in UTC; `Local::now().to_utc()` was a needless
        // round-trip through the host OS's local-timezone database.
        let mut dt = chrono::Utc::now().naive_utc();
        modify_dt(&mut dt, values, output_type, RawTimeValue::Other)
    }
}

/// Handles a numeric first argument to date()/time()/datetime()/etc. that
/// failed the default julian-day parse. Per sqlite's docs, only a modifier
/// that *immediately* follows the original time-value can reinterpret it
/// (here: 'unixepoch', or 'auto' when the magnitude looks like a unix
/// timestamp); anything else means the value has no valid interpretation,
/// matching the NULL sqlite would return for an out-of-range bare number.
/// <https://www.sqlite.org/lang_datefunc.html>
fn modify_dt_reinterpret_numeric(
    raw: f64,
    mods: &[Register],
    output_type: DateTimeOutput,
) -> Value {
    let (first, rest) = match mods.split_first() {
        Some(pair) => pair,
        None => return Value::build_text(""),
    };
    let modifier_text = match first.get_owned_value() {
        Value::Text(text_rc) => text_rc.as_str(),
        _ => return Value::build_text(""),
    };
    let modifier = match parse_modifier(modifier_text) {
        Ok(m) => m,
        Err(_) => return Value::build_text(""),
    };
    let reinterpreted = match modifier {
        Modifier::UnixEpoch => unix_timestamp_to_datetime(raw),
        Modifier::Auto if is_auto_unixepoch_value(raw) => unix_timestamp_to_datetime(raw),
        _ => None,
    };
    match reinterpreted {
        Some(mut dt) => modify_dt(&mut dt, rest, output_type, RawTimeValue::Other),
        None => Value::build_text(""),
    }
}

fn modify_dt(
    dt: &mut NaiveDateTime,
    mods: &[Register],
    output_type: DateTimeOutput,
    raw: RawTimeValue,
) -> Value {
    let mut subsec_requested = false;
    let mut rounding_mode = MonthRounding::default();
    let mut zone = DtZone::Utc;

    for (i, modifier) in mods.iter().enumerate() {
        if let Value::Text(ref text_rc) = modifier.get_owned_value() {
            // sqlite documents 'ceiling'/'floor' as resolving the ambiguity of
            // "the time shift [it] immediately follow[s]". Peeking one
            // modifier ahead lets a *trailing* ceiling/floor retroactively
            // pick the rounding used by *this* shift (sqlite's documented
            // position for it), while one placed earlier in the list still
            // sticks for whatever shift comes after it. Both orderings work.
            if let Some(next_mode) = mods
                .get(i + 1)
                .and_then(|next| match next.get_owned_value() {
                    Value::Text(t) => match parse_modifier(t.as_str()) {
                        Ok(Modifier::Ceiling) => Some(MonthRounding::Ceiling),
                        Ok(Modifier::Floor) => Some(MonthRounding::Floor),
                        _ => None,
                    },
                    _ => None,
                })
            {
                rounding_mode = next_mode;
            }

            match apply_modifier(dt, text_rc.as_str(), &mut rounding_mode, raw, &mut zone) {
                Ok(true) => subsec_requested = true,
                Ok(false) => {}
                Err(_) => return Value::build_text(""),
            }
        } else {
            return Value::build_text("");
        }
    }
    if is_leap_second(dt) || *dt > get_max_datetime_exclusive() {
        return Value::build_text("");
    }
    format_dt(*dt, output_type, subsec_requested)
}

fn format_dt(dt: NaiveDateTime, output_type: DateTimeOutput, subsec: bool) -> Value {
    match output_type {
        DateTimeOutput::Date => Value::from_text(dt.format("%Y-%m-%d").to_string().as_str()),
        DateTimeOutput::Time => {
            let t = if subsec {
                dt.format("%H:%M:%S%.3f").to_string()
            } else {
                dt.format("%H:%M:%S").to_string()
            };
            Value::from_text(t.as_str())
        }
        DateTimeOutput::DateTime => {
            let t = if subsec {
                dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
            } else {
                dt.format("%Y-%m-%d %H:%M:%S").to_string()
            };
            Value::from_text(t.as_str())
        }
        DateTimeOutput::StrfTime(format_str) => {
            Value::from_text(strftime_format(&dt, &format_str).as_str())
        }
        DateTimeOutput::JuliaDay => Value::Float(to_julian_day_exact(&dt)),
    }
}

// Not as fast as if the formatting was native to chrono, but a good enough
// for now, just to have the feature implemented
fn strftime_format(dt: &NaiveDateTime, format_str: &str) -> String {
    use crate::functions::strftime::CustomStrftimeItems;
    use std::fmt::Write;
    // %f and %J are exclusive to SQLite; chrono has no native support for either. %f is
    // handled directly by `CustomStrftimeItems`. %J (Julian day, rendered with SQLite's 9
    // decimal places) isn't backed by any of chrono's `Numeric`/`Fixed` fields, so the
    // already-computed value is threaded in here and applied during the normal per-specifier
    // parsing stage in `CustomStrftimeItems`, rather than via a pre-processing string-replace
    // over `format_str` (which, among other things, wouldn't have honored pad overrides and
    // could misfire inside an escaped `%%J`).
    let items = CustomStrftimeItems::new(format_str, to_julian_day_exact(dt));

    // The write! macro is used here as chrono's format can panic if the formatting string contains
    // unknown specifiers. By using a writer, we can catch the panic and handle the error
    let mut formatted = String::new();
    match write!(formatted, "{}", dt.format_with_items(items)) {
        Ok(_) => formatted,
        // On sqlite when the formatting fails nothing is printed
        Err(_) => "".to_string(),
    }
}

// to prevent stripping the modifier string and comparing multiple times, this returns
// whether the modifier was a subsec modifier because it impacts the format string
fn apply_modifier(
    dt: &mut NaiveDateTime,
    modifier: &str,
    rounding_mode: &mut MonthRounding,
    raw: RawTimeValue,
    zone: &mut DtZone,
) -> Result<bool> {
    let parsed_modifier = parse_modifier(modifier)?;

    match parsed_modifier {
        Modifier::Days(days) => *dt += TimeDelta::days(days),
        Modifier::Hours(hours) => *dt += TimeDelta::hours(hours),
        Modifier::Minutes(minutes) => *dt += TimeDelta::minutes(minutes),
        Modifier::Seconds(seconds) => *dt += TimeDelta::seconds(seconds),
        Modifier::Months(m) => {
            // Convert months to years + leftover months
            let years = m / 12;
            let leftover = m % 12;
            add_years_and_months(dt, years, leftover, *rounding_mode)?;
        }
        Modifier::Years(y) => {
            add_years_and_months(dt, y, 0, *rounding_mode)?;
        }
        Modifier::TimeOffset(offset) => *dt += offset,
        Modifier::DateOffset {
            years,
            months,
            days,
        } => {
            *dt = dt
                .checked_add_months(chrono::Months::new((years * 12 + months) as u32))
                .ok_or_else(|| InvalidModifier("Invalid date offset".to_string()))?;
            *dt += TimeDelta::days(days as i64);
        }
        Modifier::DateTimeOffset {
            years,
            months,
            days,
            seconds,
        } => {
            add_years_and_months(dt, years, months, *rounding_mode)?;
            *dt += chrono::Duration::days(days as i64);
            *dt += chrono::Duration::seconds(seconds.into());
        }
        Modifier::Ceiling => *rounding_mode = MonthRounding::Ceiling,
        Modifier::Floor => *rounding_mode = MonthRounding::Floor,
        Modifier::StartOfMonth => {
            *dt = NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1)
                .ok_or_else(|| InvalidModifier("Invalid start of month date".to_string()))?
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| InvalidModifier("Invalid time 00:00:00".to_string()))?;
        }
        Modifier::StartOfYear => {
            *dt = NaiveDate::from_ymd_opt(dt.year(), 1, 1)
                .ok_or_else(|| InvalidModifier("Invalid start of year date".to_string()))?
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| InvalidModifier("Invalid time 00:00:00".to_string()))?;
        }
        Modifier::StartOfDay => {
            *dt = dt.date().and_hms_opt(0, 0, 0).ok_or_else(|| {
                InvalidModifier("Invalid time 00:00:00 for start of day".to_string())
            })?;
        }
        Modifier::Weekday(day) => {
            let current_day = dt.weekday().num_days_from_sunday();
            let target_day = day;
            let days_to_add = (target_day + 7 - current_day) % 7;
            *dt += TimeDelta::days(days_to_add as i64);
        }
        Modifier::Auto => match raw {
            RawTimeValue::Numeric(n) => {
                if is_julian_day_value(n) {
                    // Already the default interpretation; a no-op.
                } else if is_auto_unixepoch_value(n) {
                    *dt = unix_timestamp_to_datetime(n).ok_or_else(|| {
                        InvalidModifier("auto: value out of supported date range".to_string())
                    })?;
                } else {
                    return Err(InvalidModifier(
                        "auto: numeric time-value out of range".to_string(),
                    ));
                }
            }
            // "The auto modifier is a no-op for ISO 8601 text time-values."
            RawTimeValue::Other => {}
        },
        Modifier::UnixEpoch => match raw {
            RawTimeValue::Numeric(n) => {
                *dt = unix_timestamp_to_datetime(n).ok_or_else(|| {
                    InvalidModifier("unixepoch: value out of supported date range".to_string())
                })?;
            }
            RawTimeValue::Other => {
                return Err(InvalidModifier(
                    "unixepoch modifier must immediately follow a numeric time-value".to_string(),
                ));
            }
        },
        Modifier::JulianDay => match raw {
            RawTimeValue::Numeric(n) => {
                *dt = get_date_time_from_time_value_float(n)
                    .ok_or_else(|| InvalidModifier("julianday: value out of range".to_string()))?;
            }
            RawTimeValue::Other => {
                return Err(InvalidModifier(
                    "julianday modifier must immediately follow a numeric time-value".to_string(),
                ));
            }
        },
        Modifier::Localtime => {
            // no-op if already local: applying it twice (or to a value that
            // never left local) must not shift the offset a second time.
            if *zone != DtZone::Local {
                let utc_dt = DateTime::<Utc>::from_naive_utc_and_offset(*dt, Utc);
                *dt = utc_dt.with_timezone(&chrono::Local).naive_local();
                *zone = DtZone::Local;
            }
        }
        Modifier::Utc => {
            // no-op if already utc (e.g. `datetime('now','utc')`): 'now' is
            // already expressed in UTC, so converting "local -> utc" again
            // would silently shift it by the local offset a second time.
            if *zone != DtZone::Utc {
                let local_dt = chrono::Local
                    .from_local_datetime(dt)
                    .single()
                    .ok_or_else(|| {
                        InvalidModifier(
                            "Ambiguous or invalid local datetime for UTC conversion".to_string(),
                        )
                    })?;
                *dt = local_dt.with_timezone(&Utc).naive_utc();
                *zone = DtZone::Utc;
            }
        }
        Modifier::Subsec => {
            *dt = dt.with_nanosecond(dt.nanosecond()).ok_or_else(|| {
                InvalidModifier("Invalid nanosecond value for subsec".to_string())
            })?;
            return Ok(true);
        }
    }

    Ok(false)
}

fn is_julian_day_value(value: f64) -> bool {
    (0.0..5373484.5).contains(&value)
}

fn add_years_and_months(
    dt: &mut NaiveDateTime,
    years: i32,
    months: i32,
    rounding: MonthRounding,
) -> Result<()> {
    add_whole_years(dt, years, rounding)?;
    add_months_in_increments(dt, months, rounding)?;
    Ok(())
}

fn add_whole_years(dt: &mut NaiveDateTime, years: i32, rounding: MonthRounding) -> Result<()> {
    if years == 0 {
        return Ok(());
    }
    let target_year = dt.year() + years;
    let (m, d, hh, mm, ss) = (dt.month(), dt.day(), dt.hour(), dt.minute(), dt.second());

    // attempt same (month, day) in new year
    if let Some(date) = NaiveDate::from_ymd_opt(target_year, m, d) {
        *dt = date
            .and_hms_opt(hh, mm, ss)
            .ok_or_else(|| InvalidModifier("Invalid datetime format".to_string()))?;
        return Ok(());
    }

    // if invalid: compute overflow days
    let last_day_in_feb = last_day_in_month(target_year, m);
    if d > last_day_in_feb {
        // base date is last_day_in_feb
        let base_date = NaiveDate::from_ymd_opt(target_year, m, last_day_in_feb)
            .ok_or_else(|| InvalidModifier("Invalid datetime format".to_string()))?
            .and_hms_opt(hh, mm, ss)
            .ok_or_else(|| InvalidModifier("Invalid time format".to_string()))?;

        *dt = match rounding {
            // leftover = d - last_day_in_feb; spill over into the next month
            MonthRounding::Ceiling => {
                base_date + chrono::Duration::days((d - last_day_in_feb) as i64)
            }
            // clamp to the last valid day of the target month
            MonthRounding::Floor => base_date,
        };
    } else {
        // do we fall back here?
    }
    Ok(())
}

fn add_months_in_increments(
    dt: &mut NaiveDateTime,
    months: i32,
    rounding: MonthRounding,
) -> Result<()> {
    let step = if months >= 0 { 1 } else { -1 };
    for _ in 0..months.abs() {
        add_one_month(dt, step, rounding)?;
    }
    Ok(())
}

// sqlite resolves any ambiguity between advancing months by using the 'ceiling'
// value, computing overflow days and advancing to the next valid date
// e.g. 2024-01-31 + 1 month = 2024-03-02
//
// the 'floor' modifier resolves the same ambiguity by clamping to the last
// valid day of the target month instead, e.g. 2024-01-31 + 1 month = 2024-02-29
fn add_one_month(dt: &mut NaiveDateTime, step: i32, rounding: MonthRounding) -> Result<()> {
    let (y0, m0, d0) = (dt.year(), dt.month(), dt.day());
    let (hh, mm, ss) = (dt.hour(), dt.minute(), dt.second());

    let mut new_year = y0;
    let mut new_month = m0 as i32 + step;
    if new_month > 12 {
        new_month -= 12;
        new_year += 1;
    } else if new_month < 1 {
        new_month += 12;
        new_year -= 1;
    }

    let last_day = last_day_in_month(new_year, new_month as u32);
    if d0 <= last_day {
        // valid date
        *dt = NaiveDate::from_ymd_opt(new_year, new_month as u32, d0)
            .ok_or_else(|| InvalidModifier("Invalid Auto format".to_string()))?
            .and_hms_opt(hh, mm, ss)
            .ok_or_else(|| InvalidModifier("Invalid Auto format".to_string()))?;
    } else {
        let base_date = NaiveDate::from_ymd_opt(new_year, new_month as u32, last_day)
            .ok_or_else(|| InvalidModifier("Invalid Auto format".to_string()))?
            .and_hms_opt(hh, mm, ss)
            .ok_or_else(|| InvalidModifier("Invalid Auto format".to_string()))?;

        *dt = match rounding {
            MonthRounding::Ceiling => base_date + chrono::Duration::days((d0 - last_day) as i64),
            MonthRounding::Floor => base_date,
        };
    }
    Ok(())
}

#[inline(always)]
fn last_day_in_month(year: i32, month: u32) -> u32 {
    for day in (28..=31).rev() {
        if NaiveDate::from_ymd_opt(year, month, day).is_some() {
            return day;
        }
    }
    28
}

pub fn exec_julianday(values: &[Register]) -> Value {
    exec_datetime(values, DateTimeOutput::JuliaDay)
}

fn to_julian_day_exact(dt: &NaiveDateTime) -> f64 {
    let year = dt.year();
    let month = dt.month() as i32;
    let day = dt.day() as i32;
    let (adjusted_year, adjusted_month) = if month <= 2 {
        (year - 1, month + 12)
    } else {
        (year, month)
    };

    let a = adjusted_year / 100;
    let b = 2 - a + a / 4;
    let jd_days = (365.25 * ((adjusted_year + 4716) as f64)).floor()
        + (30.6001 * ((adjusted_month + 1) as f64)).floor()
        + (day as f64)
        + (b as f64)
        - 1524.5;

    let seconds = dt.hour() as f64 * 3600.0
        + dt.minute() as f64 * 60.0
        + dt.second() as f64
        + (dt.nanosecond() as f64) / 1_000_000_000.0;

    let jd_fraction = seconds / 86400.0;
    jd_days + jd_fraction
}

pub fn exec_unixepoch(time_value: &Value) -> Result<String> {
    let dt = parse_naive_date_time(time_value);
    match dt {
        Some(dt) => Ok(get_unixepoch_from_naive_datetime(dt)),
        None => Ok(String::new()),
    }
}

fn get_unixepoch_from_naive_datetime(value: NaiveDateTime) -> String {
    if is_leap_second(&value) {
        return String::new();
    }
    value.and_utc().timestamp().to_string()
}

fn parse_naive_date_time(time_value: &Value) -> Option<NaiveDateTime> {
    match time_value {
        Value::Text(s) => get_date_time_from_time_value_string(s.as_str()),
        Value::Integer(i) => get_date_time_from_time_value_integer(*i),
        Value::Float(f) => get_date_time_from_time_value_float(*f),
        _ => None,
    }
}

fn get_date_time_from_time_value_string(value: &str) -> Option<NaiveDateTime> {
    // Time-value formats:
    // 1-7. YYYY-MM-DD[THH:MM[:SS[.SSS]]]
    // 8-10. HH:MM[:SS[.SSS]]
    // 11. 'now'
    // 12. DDDDDDDDDD (Julian day number as integer or float)
    //
    // Ref: https://sqlite.org/lang_datefunc.html#tmval

    // Check for 'now'
    if value.trim().eq_ignore_ascii_case("now") {
        // UTC, not Local: SQLite's 'now' time-value is UTC.
        return Some(chrono::Utc::now().naive_utc());
    }

    // Check for Julian day number (integer or float)
    if let Ok(julian_day) = value.parse::<f64>() {
        return get_date_time_from_time_value_float(julian_day);
    }

    // Attempt to parse with various formats
    let date_only_format = "%Y-%m-%d";
    let datetime_formats: [&str; 9] = [
        "%Y-%m-%d %H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%H:%M",
        "%H:%M:%S",
        "%H:%M:%S%.f",
    ];

    // First, try to parse as date-only format
    if let Ok(date) = NaiveDate::parse_from_str(value, date_only_format) {
        return Some(
            date.and_time(
                NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is always a valid time"),
            ),
        );
    }

    for format in &datetime_formats {
        if let Some(dt) = if format.starts_with("%H") {
            // For time-only formats, assume date 2000-01-01
            // Ref: https://sqlite.org/lang_datefunc.html#tmval
            parse_datetime_with_optional_tz(
                &format!("2000-01-01 {}", value),
                &format!("%Y-%m-%d {}", format),
            )
        } else {
            parse_datetime_with_optional_tz(value, format)
        } {
            return Some(dt);
        }
    }
    None
}

fn parse_datetime_with_optional_tz(value: &str, format: &str) -> Option<NaiveDateTime> {
    // Try parsing with timezone
    let with_tz_format = format.to_owned() + "%:z";
    if let Ok(dt) = DateTime::parse_from_str(value, &with_tz_format) {
        return Some(dt.with_timezone(&Utc).naive_utc());
    }

    let mut value_without_tz = value;
    if value.ends_with('Z') {
        value_without_tz = &value[0..value.len() - 1];
    }

    // Parse without timezone
    if let Ok(dt) = NaiveDateTime::parse_from_str(value_without_tz, format) {
        return Some(dt);
    }
    None
}

fn get_date_time_from_time_value_integer(value: i64) -> Option<NaiveDateTime> {
    i32::try_from(value).map_or_else(
        |_| None,
        |value| {
            if value.is_negative() || !is_julian_day_value(value as f64) {
                return None;
            }
            get_date_time_from_time_value_float(value as f64)
        },
    )
}

fn get_date_time_from_time_value_float(value: f64) -> Option<NaiveDateTime> {
    if value.is_infinite() || value.is_nan() || !is_julian_day_value(value) {
        return None;
    }
    match super::julian_day::julian_day_to_datetime(value) {
        Ok(dt) => Some(dt),
        Err(_) => None,
    }
}

fn is_leap_second(dt: &NaiveDateTime) -> bool {
    // The range from 1,000,000,000 to 1,999,999,999 represents the leap second.
    dt.second() == 59 && dt.nanosecond() > 999_999_999
}

fn get_max_datetime_exclusive() -> NaiveDateTime {
    // The maximum date in SQLite is 9999-12-31
    NaiveDateTime::new(
        NaiveDate::from_ymd_opt(10000, 1, 1).expect("10000-01-01 is a valid date"),
        NaiveTime::from_hms_milli_opt(00, 00, 00, 000).expect("00:00:00.000 is a valid time"),
    )
}

/// Modifier doc https://www.sqlite.org/lang_datefunc.html#modifiers
#[allow(dead_code)]
#[derive(Debug, PartialEq)]
enum Modifier {
    Days(i64),
    Hours(i64),
    Minutes(i64),
    Seconds(i64),
    Months(i32),
    Years(i32),
    TimeOffset(TimeDelta),
    DateOffset {
        years: i32,
        months: i32,
        days: i32,
    },
    DateTimeOffset {
        years: i32,
        months: i32,
        days: i32,
        seconds: i32,
    },
    Ceiling,
    Floor,
    StartOfMonth,
    StartOfYear,
    StartOfDay,
    Weekday(u32),
    UnixEpoch,
    JulianDay,
    Auto,
    Localtime,
    Utc,
    Subsec,
}

fn parse_modifier_number(s: &str) -> Result<i64> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| InvalidModifier(format!("Invalid number: {}", s)))
}

/// supports YYYY-MM-DD format for time shift modifiers
fn parse_modifier_date(s: &str) -> Result<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| InvalidModifier("Invalid date format".to_string()))
}

/// supports following formats for time shift modifiers
/// - HH:MM
/// - HH:MM:SS
/// - HH:MM:SS.SSS
fn parse_modifier_time(s: &str) -> Result<NaiveTime> {
    match s.len() {
        5 => NaiveTime::parse_from_str(s, "%H:%M"),
        8 => NaiveTime::parse_from_str(s, "%H:%M:%S"),
        12 => NaiveTime::parse_from_str(s, "%H:%M:%S.%3f"),
        _ => return Err(InvalidModifier(format!("Invalid time format: {}", s))),
    }
    .map_err(|_| InvalidModifier(format!("Invalid time format: {}", s)))
}

fn parse_modifier(modifier: &str) -> Result<Modifier> {
    let modifier = modifier.trim().to_lowercase();

    match modifier.as_str() {
        // exact matches first
        "ceiling" => Ok(Modifier::Ceiling),
        "floor" => Ok(Modifier::Floor),
        "start of month" => Ok(Modifier::StartOfMonth),
        "start of year" => Ok(Modifier::StartOfYear),
        "start of day" => Ok(Modifier::StartOfDay),
        s if s.starts_with("weekday ") => {
            let day = parse_modifier_number(&s[8..])?;
            if !(0..=6).contains(&day) {
                Err(InvalidModifier(
                    "Weekday must be between 0 and 6".to_string(),
                ))
            } else {
                Ok(Modifier::Weekday(day as u32))
            }
        }
        "unixepoch" => Ok(Modifier::UnixEpoch),
        "julianday" => Ok(Modifier::JulianDay),
        "auto" => Ok(Modifier::Auto),
        "localtime" => Ok(Modifier::Localtime),
        "utc" => Ok(Modifier::Utc),
        "subsec" | "subsecond" => Ok(Modifier::Subsec),
        s if s.ends_with(" day") => Ok(Modifier::Days(parse_modifier_number(&s[..s.len() - 4])?)),
        s if s.ends_with(" days") => Ok(Modifier::Days(parse_modifier_number(&s[..s.len() - 5])?)),
        s if s.ends_with(" hour") => Ok(Modifier::Hours(parse_modifier_number(&s[..s.len() - 5])?)),
        s if s.ends_with(" hours") => {
            Ok(Modifier::Hours(parse_modifier_number(&s[..s.len() - 6])?))
        }
        s if s.ends_with(" minute") => {
            Ok(Modifier::Minutes(parse_modifier_number(&s[..s.len() - 7])?))
        }
        s if s.ends_with(" minutes") => {
            Ok(Modifier::Minutes(parse_modifier_number(&s[..s.len() - 8])?))
        }
        s if s.ends_with(" second") => {
            Ok(Modifier::Seconds(parse_modifier_number(&s[..s.len() - 7])?))
        }
        s if s.ends_with(" seconds") => {
            Ok(Modifier::Seconds(parse_modifier_number(&s[..s.len() - 8])?))
        }
        s if s.ends_with(" month") => Ok(Modifier::Months(
            parse_modifier_number(&s[..s.len() - 6])? as i32,
        )),
        s if s.ends_with(" months") => Ok(Modifier::Months(
            parse_modifier_number(&s[..s.len() - 7])? as i32,
        )),
        s if s.ends_with(" year") => Ok(Modifier::Years(
            parse_modifier_number(&s[..s.len() - 5])? as i32
        )),
        s if s.ends_with(" years") => Ok(Modifier::Years(
            parse_modifier_number(&s[..s.len() - 6])? as i32,
        )),
        s if s.starts_with('+') || s.starts_with('-') => {
            let sign = if s.starts_with('-') { -1 } else { 1 };
            let parts: Vec<&str> = s[1..].split(' ').collect();
            let digits_in_date = 10;
            match parts.len() {
                1 => {
                    if parts[0].len() == digits_in_date {
                        let date = parse_modifier_date(parts[0])?;
                        Ok(Modifier::DateOffset {
                            years: sign * date.year(),
                            months: sign * date.month() as i32,
                            days: sign * date.day() as i32,
                        })
                    } else {
                        // time values are either 12, 8 or 5 digits
                        let time = parse_modifier_time(parts[0])?;
                        let time_delta = sign * (time.num_seconds_from_midnight() as i32);
                        Ok(Modifier::TimeOffset(TimeDelta::seconds(time_delta.into())))
                    }
                }
                2 => {
                    let date = parse_modifier_date(parts[0])?;
                    let time = parse_modifier_time(parts[1])?;
                    // Convert time to total seconds (with sign)
                    let time_delta = sign * (time.num_seconds_from_midnight() as i32);
                    Ok(Modifier::DateTimeOffset {
                        years: sign * (date.year()),
                        months: sign * (date.month() as i32),
                        days: sign * date.day() as i32,
                        seconds: time_delta,
                    })
                }
                _ => Err(InvalidModifier(
                    "Invalid date/time offset format".to_string(),
                )),
            }
        }
        _ => Err(InvalidModifier(
            "Invalid date/time offset format".to_string(),
        )),
    }
}

pub fn exec_timediff(values: &[Register]) -> Value {
    if values.len() < 2 {
        return Value::Null;
    }

    let start = parse_naive_date_time(values[0].get_owned_value());
    let end = parse_naive_date_time(values[1].get_owned_value());

    match (start, end) {
        (Some(start), Some(end)) => {
            let duration = start.signed_duration_since(end);
            format_time_duration(&duration)
        }
        _ => Value::Null,
    }
}

/// Format the time duration as +/-YYYY-MM-DD HH:MM:SS.SSS as per SQLite's timediff() function
fn format_time_duration(duration: &chrono::Duration) -> Value {
    let is_negative = duration.num_seconds() < 0;

    let abs_duration = if is_negative {
        -duration.clone()
    } else {
        duration.clone()
    };

    let total_seconds = abs_duration.num_seconds();
    let hours = (total_seconds % 86400) / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let days = total_seconds / 86400;
    let years = days / 365;
    let remaining_days = days % 365;
    let months = 0;

    let total_millis = abs_duration.num_milliseconds();
    let millis = total_millis % 1000;

    let result = format!(
        "{}{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        if is_negative { "-" } else { "+" },
        years,
        months,
        remaining_days,
        hours,
        minutes,
        seconds,
        millis
    );

    Value::build_text(&result)
}

#[cfg(test)]
mod tests;
