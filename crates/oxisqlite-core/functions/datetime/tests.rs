//! Auto-generated test module (consolidated from inline `#[cfg(test)] mod` blocks)

use crate::Result;
use crate::{types::Value, vdbe::Register};
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike};

use super::*;

fn apply_modifier(dt: &mut NaiveDateTime, modifier: &str) -> Result<bool> {
    super::apply_modifier(
        dt,
        modifier,
        &mut MonthRounding::default(),
        RawTimeValue::Other,
        &mut DtZone::Utc,
    )
}

#[test]
fn test_valid_get_date_from_time_value() {
    let now = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let prev_date_str = "2024-07-20";
    let test_date_str = "2024-07-21";
    let next_date_str = "2024-07-22";
    let test_cases = vec![
        (Value::build_text("2024-07-21"), test_date_str),
        (Value::build_text("2024-07-21 22:30"), test_date_str),
        (Value::build_text("2024-07-21 22:30+02:00"), test_date_str),
        (Value::build_text("2024-07-21 22:30-05:00"), next_date_str),
        (Value::build_text("2024-07-21 01:30+05:00"), prev_date_str),
        (Value::build_text("2024-07-21 22:30Z"), test_date_str),
        (Value::build_text("2024-07-21 22:30:45"), test_date_str),
        (
            Value::build_text("2024-07-21 22:30:45+02:00"),
            test_date_str,
        ),
        (
            Value::build_text("2024-07-21 22:30:45-05:00"),
            next_date_str,
        ),
        (
            Value::build_text("2024-07-21 01:30:45+05:00"),
            prev_date_str,
        ),
        (Value::build_text("2024-07-21 22:30:45Z"), test_date_str),
        (Value::build_text("2024-07-21 22:30:45.123"), test_date_str),
        (
            Value::build_text("2024-07-21 22:30:45.123+02:00"),
            test_date_str,
        ),
        (
            Value::build_text("2024-07-21 22:30:45.123-05:00"),
            next_date_str,
        ),
        (
            Value::build_text("2024-07-21 01:30:45.123+05:00"),
            prev_date_str,
        ),
        (Value::build_text("2024-07-21 22:30:45.123Z"), test_date_str),
        (Value::build_text("2024-07-21T22:30"), test_date_str),
        (Value::build_text("2024-07-21T22:30+02:00"), test_date_str),
        (Value::build_text("2024-07-21T22:30-05:00"), next_date_str),
        (Value::build_text("2024-07-21T01:30+05:00"), prev_date_str),
        (Value::build_text("2024-07-21T22:30Z"), test_date_str),
        (Value::build_text("2024-07-21T22:30:45"), test_date_str),
        (
            Value::build_text("2024-07-21T22:30:45+02:00"),
            test_date_str,
        ),
        (
            Value::build_text("2024-07-21T22:30:45-05:00"),
            next_date_str,
        ),
        (
            Value::build_text("2024-07-21T01:30:45+05:00"),
            prev_date_str,
        ),
        (Value::build_text("2024-07-21T22:30:45Z"), test_date_str),
        (Value::build_text("2024-07-21T22:30:45.123"), test_date_str),
        (
            Value::build_text("2024-07-21T22:30:45.123+02:00"),
            test_date_str,
        ),
        (
            Value::build_text("2024-07-21T22:30:45.123-05:00"),
            next_date_str,
        ),
        (
            Value::build_text("2024-07-21T01:30:45.123+05:00"),
            prev_date_str,
        ),
        (Value::build_text("2024-07-21T22:30:45.123Z"), test_date_str),
        (Value::build_text("22:30"), "2000-01-01"),
        (Value::build_text("22:30+02:00"), "2000-01-01"),
        (Value::build_text("22:30-05:00"), "2000-01-02"),
        (Value::build_text("01:30+05:00"), "1999-12-31"),
        (Value::build_text("22:30Z"), "2000-01-01"),
        (Value::build_text("22:30:45"), "2000-01-01"),
        (Value::build_text("22:30:45+02:00"), "2000-01-01"),
        (Value::build_text("22:30:45-05:00"), "2000-01-02"),
        (Value::build_text("01:30:45+05:00"), "1999-12-31"),
        (Value::build_text("22:30:45Z"), "2000-01-01"),
        (Value::build_text("22:30:45.123"), "2000-01-01"),
        (Value::build_text("22:30:45.123+02:00"), "2000-01-01"),
        (Value::build_text("22:30:45.123-05:00"), "2000-01-02"),
        (Value::build_text("01:30:45.123+05:00"), "1999-12-31"),
        (Value::build_text("22:30:45.123Z"), "2000-01-01"),
        (Value::build_text("now"), &now),
        (Value::Float(2460512.5), test_date_str),
        (Value::Integer(2460513), test_date_str),
    ];
    for (input, expected) in test_cases {
        let result = exec_date(&[Register::Value(input.clone())]);
        assert_eq!(
            result,
            Value::build_text(expected),
            "Failed for input: {:?}",
            input
        );
    }
}

#[test]
fn test_invalid_get_date_from_time_value() {
    let invalid_cases = vec![
        Value::build_text("2024-07-21 25:00"),
        Value::build_text("2024-07-21 24:00:00"),
        Value::build_text("2024-07-21 23:60:00"),
        Value::build_text("2024-07-21 22:58:60"),
        Value::build_text("2024-07-32"),
        Value::build_text("2024-13-01"),
        Value::build_text("invalid_date"),
        Value::build_text(""),
        Value::Integer(i64::MAX),
        Value::Integer(-1),
        Value::Float(f64::MAX),
        Value::Float(-1.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Null,
        Value::Blob(vec![1, 2, 3].into()),
        Value::build_text("2024-07-21T12:00:00+24:00"),
        Value::build_text("2024-07-21T12:00:00-24:00"),
        Value::build_text("2024-07-21T12:00:00+00:60"),
        Value::build_text("2024-07-21T12:00:00+00:00:00"),
        Value::build_text("2024-07-21T12:00:00+"),
        Value::build_text("2024-07-21T12:00:00+Z"),
        Value::build_text("2024-07-21T12:00:00+00:00Z"),
        Value::build_text("2024-07-21T12:00:00UTC"),
    ];
    for case in invalid_cases.iter() {
        let result = exec_date(&[Register::Value(case.clone())]);
        match result {
            Value::Text(ref result_str) if result_str.value.is_empty() => {}
            _ => {
                panic!(
                    "Expected empty string for input: {:?}, but got: {:?}",
                    case, result
                )
            }
        }
    }
}

#[test]
fn test_valid_get_time_from_datetime_value() {
    let test_time_str = "22:30:45";
    let prev_time_str = "20:30:45";
    let next_time_str = "03:30:45";
    let test_cases = vec![
        (Value::build_text("2024-07-21"), "00:00:00"),
        (Value::build_text("2024-07-21 22:30"), "22:30:00"),
        (Value::build_text("2024-07-21 22:30+02:00"), "20:30:00"),
        (Value::build_text("2024-07-21 22:30-05:00"), "03:30:00"),
        (Value::build_text("2024-07-21 22:30Z"), "22:30:00"),
        (Value::build_text("2024-07-21 22:30:45"), test_time_str),
        (
            Value::build_text("2024-07-21 22:30:45+02:00"),
            prev_time_str,
        ),
        (
            Value::build_text("2024-07-21 22:30:45-05:00"),
            next_time_str,
        ),
        (Value::build_text("2024-07-21 22:30:45Z"), test_time_str),
        (Value::build_text("2024-07-21 22:30:45.123"), test_time_str),
        (
            Value::build_text("2024-07-21 22:30:45.123+02:00"),
            prev_time_str,
        ),
        (
            Value::build_text("2024-07-21 22:30:45.123-05:00"),
            next_time_str,
        ),
        (Value::build_text("2024-07-21 22:30:45.123Z"), test_time_str),
        (Value::build_text("2024-07-21T22:30"), "22:30:00"),
        (Value::build_text("2024-07-21T22:30+02:00"), "20:30:00"),
        (Value::build_text("2024-07-21T22:30-05:00"), "03:30:00"),
        (Value::build_text("2024-07-21T22:30Z"), "22:30:00"),
        (Value::build_text("2024-07-21T22:30:45"), test_time_str),
        (
            Value::build_text("2024-07-21T22:30:45+02:00"),
            prev_time_str,
        ),
        (
            Value::build_text("2024-07-21T22:30:45-05:00"),
            next_time_str,
        ),
        (Value::build_text("2024-07-21T22:30:45Z"), test_time_str),
        (Value::build_text("2024-07-21T22:30:45.123"), test_time_str),
        (
            Value::build_text("2024-07-21T22:30:45.123+02:00"),
            prev_time_str,
        ),
        (
            Value::build_text("2024-07-21T22:30:45.123-05:00"),
            next_time_str,
        ),
        (Value::build_text("2024-07-21T22:30:45.123Z"), test_time_str),
        (Value::build_text("22:30"), "22:30:00"),
        (Value::build_text("22:30+02:00"), "20:30:00"),
        (Value::build_text("22:30-05:00"), "03:30:00"),
        (Value::build_text("22:30Z"), "22:30:00"),
        (Value::build_text("22:30:45"), test_time_str),
        (Value::build_text("22:30:45+02:00"), prev_time_str),
        (Value::build_text("22:30:45-05:00"), next_time_str),
        (Value::build_text("22:30:45Z"), test_time_str),
        (Value::build_text("22:30:45.123"), test_time_str),
        (Value::build_text("22:30:45.123+02:00"), prev_time_str),
        (Value::build_text("22:30:45.123-05:00"), next_time_str),
        (Value::build_text("22:30:45.123Z"), test_time_str),
        (Value::Float(2460082.1), "14:24:00"),
        (Value::Integer(2460082), "12:00:00"),
    ];
    for (input, expected) in test_cases {
        let result = exec_time(&[Register::Value(input)]);
        if let Value::Text(result_str) = result {
            assert_eq!(result_str.as_str(), expected);
        } else {
            panic!("Expected Value::Text, but got: {:?}", result);
        }
    }
}

#[test]
fn test_invalid_get_time_from_datetime_value() {
    let invalid_cases = vec![
        Value::build_text("2024-07-21 25:00"),
        Value::build_text("2024-07-21 24:00:00"),
        Value::build_text("2024-07-21 23:60:00"),
        Value::build_text("2024-07-21 22:58:60"),
        Value::build_text("2024-07-32"),
        Value::build_text("2024-13-01"),
        Value::build_text("invalid_date"),
        Value::build_text(""),
        Value::Integer(i64::MAX),
        Value::Integer(-1),
        Value::Float(f64::MAX),
        Value::Float(-1.0),
        Value::Float(f64::NAN),
        Value::Float(f64::INFINITY),
        Value::Null,
        Value::Blob(vec![1, 2, 3].into()),
        Value::build_text("2024-07-21T12:00:00+24:00"),
        Value::build_text("2024-07-21T12:00:00-24:00"),
        Value::build_text("2024-07-21T12:00:00+00:60"),
        Value::build_text("2024-07-21T12:00:00+00:00:00"),
        Value::build_text("2024-07-21T12:00:00+"),
        Value::build_text("2024-07-21T12:00:00+Z"),
        Value::build_text("2024-07-21T12:00:00+00:00Z"),
        Value::build_text("2024-07-21T12:00:00UTC"),
    ];
    for case in invalid_cases {
        let result = exec_time(&[Register::Value(case.clone())]);
        match result {
            Value::Text(ref result_str) if result_str.value.is_empty() => {}
            _ => {
                panic!(
                    "Expected empty string for input: {:?}, but got: {:?}",
                    case, result
                )
            }
        }
    }
}

#[test]
fn test_parse_days() {
    assert_eq!(parse_modifier("5 days").unwrap(), Modifier::Days(5));
    assert_eq!(parse_modifier("-3 days").unwrap(), Modifier::Days(-3));
    assert_eq!(parse_modifier("+2 days").unwrap(), Modifier::Days(2));
    assert_eq!(parse_modifier("4  days").unwrap(), Modifier::Days(4));
    assert_eq!(parse_modifier("6   DAYS").unwrap(), Modifier::Days(6));
    assert_eq!(parse_modifier("+5  DAYS").unwrap(), Modifier::Days(5));
}

#[test]
fn test_parse_hours() {
    assert_eq!(parse_modifier("12 hours").unwrap(), Modifier::Hours(12));
    assert_eq!(parse_modifier("-2 hours").unwrap(), Modifier::Hours(-2));
    assert_eq!(parse_modifier("+3  HOURS").unwrap(), Modifier::Hours(3));
}

#[test]
fn test_parse_minutes() {
    assert_eq!(parse_modifier("30 minutes").unwrap(), Modifier::Minutes(30));
    assert_eq!(
        parse_modifier("-15 minutes").unwrap(),
        Modifier::Minutes(-15)
    );
    assert_eq!(
        parse_modifier("+45  MINUTES").unwrap(),
        Modifier::Minutes(45)
    );
}

#[test]
fn test_parse_seconds() {
    assert_eq!(parse_modifier("45 seconds").unwrap(), Modifier::Seconds(45));
    assert_eq!(
        parse_modifier("-10 seconds").unwrap(),
        Modifier::Seconds(-10)
    );
    assert_eq!(
        parse_modifier("+20  SECONDS").unwrap(),
        Modifier::Seconds(20)
    );
}

#[test]
fn test_parse_months() {
    assert_eq!(parse_modifier("3 months").unwrap(), Modifier::Months(3));
    assert_eq!(parse_modifier("-1 months").unwrap(), Modifier::Months(-1));
    assert_eq!(parse_modifier("+6  MONTHS").unwrap(), Modifier::Months(6));
}

#[test]
fn test_parse_years() {
    assert_eq!(parse_modifier("2 years").unwrap(), Modifier::Years(2));
    assert_eq!(parse_modifier("-1 years").unwrap(), Modifier::Years(-1));
    assert_eq!(parse_modifier("+10  YEARS").unwrap(), Modifier::Years(10));
}

#[test]
fn test_parse_time_offset() {
    assert_eq!(
        parse_modifier("+01:30").unwrap(),
        Modifier::TimeOffset(TimeDelta::hours(1) + TimeDelta::minutes(30))
    );
    assert_eq!(
        parse_modifier("-00:45").unwrap(),
        Modifier::TimeOffset(TimeDelta::minutes(-45))
    );
    assert_eq!(
        parse_modifier("+02:15:30").unwrap(),
        Modifier::TimeOffset(TimeDelta::hours(2) + TimeDelta::minutes(15) + TimeDelta::seconds(30))
    );
    assert_eq!(
        parse_modifier("+02:15:30.250").unwrap(),
        Modifier::TimeOffset(TimeDelta::hours(2) + TimeDelta::minutes(15) + TimeDelta::seconds(30))
    );
}

#[test]
fn test_parse_date_offset() {
    assert_eq!(
        parse_modifier("+2023-05-15").unwrap(),
        Modifier::DateOffset {
            years: 2023,
            months: 5,
            days: 15,
        }
    );
    assert_eq!(
        parse_modifier("-2023-05-15").unwrap(),
        Modifier::DateOffset {
            years: -2023,
            months: -5,
            days: -15,
        }
    );
}

#[test]
fn test_parse_date_time_offset() {
    assert_eq!(
        parse_modifier("+2023-05-15 14:30").unwrap(),
        Modifier::DateTimeOffset {
            years: 2023,
            months: 5,
            days: 15,
            seconds: (14 * 60 + 30) * 60,
        }
    );
    assert_eq!(
        parse_modifier("-0001-05-15 14:30").unwrap(),
        Modifier::DateTimeOffset {
            years: -1,
            months: -5,
            days: -15,
            seconds: -((14 * 60 + 30) * 60),
        }
    );
}

#[test]
fn test_parse_start_of() {
    assert_eq!(
        parse_modifier("start of month").unwrap(),
        Modifier::StartOfMonth
    );
    assert_eq!(
        parse_modifier("START OF MONTH").unwrap(),
        Modifier::StartOfMonth
    );
    assert_eq!(
        parse_modifier("start of year").unwrap(),
        Modifier::StartOfYear
    );
    assert_eq!(
        parse_modifier("START OF YEAR").unwrap(),
        Modifier::StartOfYear
    );
    assert_eq!(
        parse_modifier("start of day").unwrap(),
        Modifier::StartOfDay
    );
    assert_eq!(
        parse_modifier("START OF DAY").unwrap(),
        Modifier::StartOfDay
    );
}

#[test]
fn test_parse_weekday() {
    assert_eq!(parse_modifier("weekday 0").unwrap(), Modifier::Weekday(0));
    assert_eq!(parse_modifier("WEEKDAY 6").unwrap(), Modifier::Weekday(6));
}

#[test]
fn test_parse_other_modifiers() {
    assert_eq!(parse_modifier("unixepoch").unwrap(), Modifier::UnixEpoch);
    assert_eq!(parse_modifier("UNIXEPOCH").unwrap(), Modifier::UnixEpoch);
    assert_eq!(parse_modifier("julianday").unwrap(), Modifier::JulianDay);
    assert_eq!(parse_modifier("JULIANDAY").unwrap(), Modifier::JulianDay);
    assert_eq!(parse_modifier("auto").unwrap(), Modifier::Auto);
    assert_eq!(parse_modifier("AUTO").unwrap(), Modifier::Auto);
    assert_eq!(parse_modifier("localtime").unwrap(), Modifier::Localtime);
    assert_eq!(parse_modifier("LOCALTIME").unwrap(), Modifier::Localtime);
    assert_eq!(parse_modifier("utc").unwrap(), Modifier::Utc);
    assert_eq!(parse_modifier("UTC").unwrap(), Modifier::Utc);
    assert_eq!(parse_modifier("subsec").unwrap(), Modifier::Subsec);
    assert_eq!(parse_modifier("SUBSEC").unwrap(), Modifier::Subsec);
    assert_eq!(parse_modifier("subsecond").unwrap(), Modifier::Subsec);
    assert_eq!(parse_modifier("SUBSECOND").unwrap(), Modifier::Subsec);
}

#[test]
fn test_parse_invalid_modifier() {
    assert!(parse_modifier("invalid modifier").is_err());
    assert!(parse_modifier("5").is_err());
    assert!(parse_modifier("days").is_err());
    assert!(parse_modifier("++5 days").is_err());
    assert!(parse_modifier("weekday 7").is_err());
}

fn create_datetime(
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
    sec: u32,
) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, sec)
        .unwrap()
}

fn setup_datetime() -> NaiveDateTime {
    create_datetime(2023, 6, 15, 12, 30, 45)
}

#[test]
fn test_apply_modifier_days() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "5 days").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 20, 12, 30, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-3 days").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 12, 12, 30, 45));
}

#[test]
fn test_apply_modifier_hours() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "6 hours").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 18, 30, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-2 hours").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 10, 30, 45));
}

#[test]
fn test_apply_modifier_minutes() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "45 minutes").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 13, 15, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-15 minutes").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 12, 15, 45));
}

#[test]
fn test_apply_modifier_seconds() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "30 seconds").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 12, 31, 15));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-20 seconds").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 12, 30, 25));
}

#[test]
fn test_apply_modifier_time_offset() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "+01:30").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 14, 0, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-00:45").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 11, 45, 45));
}

#[test]
fn test_apply_modifier_date_time_offset() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "+0001-01-01 01:01").unwrap();
    assert_eq!(dt, create_datetime(2024, 7, 16, 13, 31, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-0001-01-01 01:01").unwrap();
    assert_eq!(dt, create_datetime(2022, 5, 14, 11, 29, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "+0002-03-04 05:06").unwrap();
    assert_eq!(dt, create_datetime(2025, 9, 19, 17, 36, 45));
    dt = setup_datetime();
    apply_modifier(&mut dt, "-0002-03-04 05:06").unwrap();
    assert_eq!(dt, create_datetime(2021, 3, 11, 7, 24, 45));
}

#[test]
fn test_apply_modifier_start_of_year() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "start of year").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 1, 0, 0, 0));
}

#[test]
fn test_apply_modifier_start_of_day() {
    let mut dt = setup_datetime();
    apply_modifier(&mut dt, "start of day").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 15, 0, 0, 0));
}

fn text(value: &str) -> Register {
    Register::Value(Value::build_text(value))
}

fn format(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn weekday_sunday_based(dt: &NaiveDateTime) -> u32 {
    dt.weekday().num_days_from_sunday()
}

#[test]
fn test_single_modifier() {
    let time = setup_datetime();
    let expected = format(time - TimeDelta::days(1));
    let result = exec_datetime(
        &[text("2023-06-15 12:30:45"), text("-1 day")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_multiple_modifiers() {
    let time = setup_datetime();
    let expected = format(time - TimeDelta::days(1) + TimeDelta::hours(3));
    let result = exec_datetime(
        &[
            text("2023-06-15 12:30:45"),
            text("-1 day"),
            text("+3 hours"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_subsec_modifier() {
    let time = setup_datetime();
    let result = exec_datetime(
        &[text("2023-06-15 12:30:45"), text("subsec")],
        DateTimeOutput::Time,
    );
    let result = NaiveTime::parse_from_str(&result.to_string(), "%H:%M:%S%.3f").unwrap();
    assert_eq!(time.time(), result);
}

#[test]
fn test_start_of_day_modifier() {
    let time = setup_datetime();
    let start_of_day = time.date().and_hms_opt(0, 0, 0).unwrap();
    let expected = format(start_of_day - TimeDelta::days(1));
    let result = exec_datetime(
        &[
            text("2023-06-15 12:30:45"),
            text("start of day"),
            text("-1 day"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_start_of_month_modifier() {
    let time = setup_datetime();
    let start_of_month = NaiveDate::from_ymd_opt(time.year(), time.month(), 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let expected = format(start_of_month + TimeDelta::days(1));
    let result = exec_datetime(
        &[
            text("2023-06-15 12:30:45"),
            text("start of month"),
            text("+1 day"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_start_of_year_modifier() {
    let time = setup_datetime();
    let start_of_year = NaiveDate::from_ymd_opt(time.year(), 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let expected = format(start_of_year + TimeDelta::days(30) + TimeDelta::hours(5));
    let result = exec_datetime(
        &[
            text("2023-06-15 12:30:45"),
            text("start of year"),
            text("+30 days"),
            text("+5 hours"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_timezone_modifiers() {
    let dt = setup_datetime();
    let result_local = exec_datetime(
        &[text("2023-06-15 12:30:45"), text("localtime")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(
        result_local,
        *text(
            &dt.and_utc()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        )
        .get_owned_value()
    );
}

#[test]
fn test_utc_modifier_is_a_no_op_on_now() {
    let plain = exec_datetime(&[text("now")], DateTimeOutput::DateTime);
    let with_utc = exec_datetime(&[text("now"), text("utc")], DateTimeOutput::DateTime);
    assert_eq!(plain, with_utc);
}

#[test]
fn test_utc_modifier_is_a_no_op_on_explicit_utc_input() {
    let result = exec_datetime(
        &[text("2024-07-15 08:15:30"), text("utc")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text("2024-07-15 08:15:30").get_owned_value());
}

#[test]
fn test_localtime_then_utc_round_trips_without_double_conversion() {
    let result = exec_datetime(
        &[text("2024-07-15 08:15:30"), text("localtime"), text("utc")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text("2024-07-15 08:15:30").get_owned_value());
}

#[test]
fn test_ceiling_modifier_matches_sqlite_default_spillover() {
    let mut dt = create_datetime(2023, 12, 31, 10, 0, 0);
    apply_modifier(&mut dt, "+2 months").unwrap();
    assert_eq!(dt, create_datetime(2024, 3, 2, 10, 0, 0));
    let result = exec_datetime(
        &[
            text("2024-01-31 10:00:00"),
            text("+1 month"),
            text("ceiling"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text("2024-03-02 10:00:00").get_owned_value());
}

#[test]
fn test_floor_modifier_clamps_to_last_valid_day_of_target_month() {
    let result = exec_datetime(
        &[text("2024-01-31 10:00:00"), text("+1 month"), text("floor")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(
        result,
        *text("2024-02-29 10:00:00").get_owned_value(),
        "2024 is a leap year, so february's last day is the 29th"
    );
    let result = exec_datetime(
        &[text("2023-01-31 10:00:00"), text("+1 month"), text("floor")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text("2023-02-28 10:00:00").get_owned_value());
}

#[test]
fn test_floor_modifier_placed_before_the_shift_also_applies() {
    let result = exec_datetime(
        &[text("2024-01-31 10:00:00"), text("floor"), text("+1 month")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text("2024-02-29 10:00:00").get_owned_value());
}

#[test]
fn test_ceiling_and_floor_year_boundary() {
    let ceiling = exec_datetime(
        &[text("2024-02-29"), text("+1 year"), text("ceiling")],
        DateTimeOutput::Date,
    );
    assert_eq!(ceiling, *text("2025-03-01").get_owned_value());
    let floor = exec_datetime(
        &[text("2024-02-29"), text("+1 year"), text("floor")],
        DateTimeOutput::Date,
    );
    assert_eq!(floor, *text("2025-02-28").get_owned_value());
}

#[test]
fn test_bare_unix_timestamp_without_modifier_is_null() {
    let result = exec_datetime(
        &[Register::Value(Value::Integer(1_700_000_000))],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, Value::build_text(""));
}

#[test]
fn test_unixepoch_modifier_reinterprets_numeric_argument() {
    let expected = DateTime::from_timestamp(1_700_000_000, 0)
        .expect("1700000000 is a representable unix timestamp")
        .naive_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let result = exec_datetime(
        &[
            Register::Value(Value::Integer(1_700_000_000)),
            text("unixepoch"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_auto_modifier_picks_unixepoch_for_large_numbers() {
    let expected = DateTime::from_timestamp(1_700_000_000, 0)
        .expect("1700000000 is a representable unix timestamp")
        .naive_utc()
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    let result = exec_datetime(
        &[Register::Value(Value::Integer(1_700_000_000)), text("auto")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_auto_modifier_keeps_julianday_for_valid_julian_day_numbers() {
    let plain = exec_datetime(
        &[Register::Value(Value::Integer(2_460_000))],
        DateTimeOutput::DateTime,
    );
    let with_auto = exec_datetime(
        &[Register::Value(Value::Integer(2_460_000)), text("auto")],
        DateTimeOutput::DateTime,
    );
    assert_ne!(plain, Value::build_text(""));
    assert_eq!(plain, with_auto);
}

#[test]
fn test_julianday_modifier_is_a_no_op_for_valid_julian_day_numbers() {
    let plain = exec_datetime(
        &[Register::Value(Value::Integer(2_460_000))],
        DateTimeOutput::DateTime,
    );
    let with_modifier = exec_datetime(
        &[
            Register::Value(Value::Integer(2_460_000)),
            text("julianday"),
        ],
        DateTimeOutput::DateTime,
    );
    assert_ne!(plain, Value::build_text(""));
    assert_eq!(plain, with_modifier);
}

#[test]
fn test_julianday_modifier_errors_when_not_following_a_numeric_value() {
    let result = exec_datetime(
        &[text("2024-01-01"), text("julianday")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, Value::build_text(""));
}

#[test]
fn test_unixepoch_modifier_errors_when_not_following_a_numeric_value() {
    let result = exec_datetime(
        &[text("2024-01-01"), text("unixepoch")],
        DateTimeOutput::DateTime,
    );
    assert_eq!(result, Value::build_text(""));
}

#[test]
fn test_combined_modifiers() {
    let time = create_datetime(2000, 1, 1, 0, 0, 0);
    let expected = time - TimeDelta::days(1)
        + TimeDelta::hours(5)
        + TimeDelta::minutes(30)
        + TimeDelta::seconds(15);
    let result = exec_datetime(
        &[
            text("2000-01-01 00:00:00"),
            text("-1 day"),
            text("+5 hours"),
            text("+30 minutes"),
            text("+15 seconds"),
            text("subsec"),
        ],
        DateTimeOutput::DateTime,
    );
    let result =
        NaiveDateTime::parse_from_str(&result.to_string(), "%Y-%m-%d %H:%M:%S%.3f").unwrap();
    assert_eq!(expected, result);
}

#[test]
fn test_max_datetime_limit() {
    let max = NaiveDate::from_ymd_opt(9999, 12, 31)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap();
    let expected = format(max);
    let result = exec_datetime(&[text("9999-12-31 23:59:59")], DateTimeOutput::DateTime);
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_leap_second_ignored() {
    let leap_second = NaiveDate::from_ymd_opt(2024, 6, 30)
        .unwrap()
        .and_hms_nano_opt(23, 59, 59, 1_500_000_000)
        .unwrap();
    let expected = String::new();
    let result = exec_datetime(&[text(&leap_second.to_string())], DateTimeOutput::DateTime);
    assert_eq!(result, *text(&expected).get_owned_value());
}

#[test]
fn test_already_on_weekday_no_change() {
    let mut dt = create_datetime(2023, 1, 1, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 0").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 1, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 0);
}

#[test]
fn test_move_forward_if_different() {
    let mut dt = create_datetime(2023, 1, 1, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 1").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 2, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 1);
    let mut dt = create_datetime(2023, 1, 3, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 5").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 6, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 5);
}

#[test]
fn test_wrap_around_weekend() {
    let mut dt = create_datetime(2023, 1, 6, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 0").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 8, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 0);
    apply_modifier(&mut dt, "weekday 0").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 8, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 0);
}

#[test]
fn test_same_day_stays_put() {
    let mut dt = create_datetime(2023, 1, 5, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 4").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 5, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 4);
}

#[test]
fn test_already_on_friday_no_change() {
    let mut dt = create_datetime(2023, 1, 6, 12, 0, 0);
    apply_modifier(&mut dt, "weekday 5").unwrap();
    assert_eq!(dt, create_datetime(2023, 1, 6, 12, 0, 0));
    assert_eq!(weekday_sunday_based(&dt), 5);
}

#[test]
fn test_apply_modifier_julianday() {
    let dt = create_datetime(2000, 1, 1, 12, 0, 0);
    let jd = crate::functions::julian_day::datetime_to_julian_day(dt);
    let dt_result = crate::functions::julian_day::julian_day_to_datetime(jd)
        .expect("julian_day_to_datetime failed");
    let diff = (dt_result - dt).num_seconds().abs();
    assert!(
        diff <= 1,
        "roundtrip diff was {} seconds for JD {}",
        diff,
        jd
    );
}

#[test]
fn test_apply_modifier_start_of_month() {
    let mut dt = create_datetime(2023, 6, 15, 12, 30, 45);
    apply_modifier(&mut dt, "start of month").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 1, 0, 0, 0));
}

#[test]
fn test_apply_modifier_subsec() {
    let mut dt = create_datetime(2023, 6, 15, 12, 30, 45);
    let dt_with_nanos = dt.with_nanosecond(123_456_789).unwrap();
    dt = dt_with_nanos;
    apply_modifier(&mut dt, "subsec").unwrap();
    assert_eq!(dt, dt_with_nanos);
}

#[test]
fn test_apply_modifier_start_of_month_basic() {
    let mut dt = create_datetime(2023, 6, 15, 12, 30, 45);
    apply_modifier(&mut dt, "start of month").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 1, 0, 0, 0));
}

#[test]
fn test_apply_modifier_start_of_month_already_at_first() {
    let mut dt = create_datetime(2023, 6, 1, 0, 0, 0);
    apply_modifier(&mut dt, "start of month").unwrap();
    assert_eq!(dt, create_datetime(2023, 6, 1, 0, 0, 0));
}

#[test]
fn test_apply_modifier_start_of_month_edge_case() {
    let mut dt = create_datetime(2023, 7, 31, 23, 59, 59);
    apply_modifier(&mut dt, "start of month").unwrap();
    assert_eq!(dt, create_datetime(2023, 7, 1, 0, 0, 0));
}

#[test]
fn test_apply_modifier_subsec_no_change() {
    let mut dt = create_datetime(2023, 6, 15, 12, 30, 45);
    let dt_with_nanos = dt.with_nanosecond(123_456_789).unwrap();
    dt = dt_with_nanos;
    apply_modifier(&mut dt, "subsec").unwrap();
    assert_eq!(dt, dt_with_nanos);
}

#[test]
fn test_apply_modifier_subsec_preserves_fractional_seconds() {
    let mut dt = create_datetime(2025, 1, 2, 4, 12, 21)
        .with_nanosecond(891_000_000)
        .unwrap();
    apply_modifier(&mut dt, "subsec").unwrap();
    let formatted = dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    assert_eq!(formatted, "2025-01-02 04:12:21.891");
}

#[test]
fn test_apply_modifier_subsec_no_fractional_seconds() {
    let mut dt = create_datetime(2025, 1, 2, 4, 12, 21);
    apply_modifier(&mut dt, "subsec").unwrap();
    let formatted = dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    assert_eq!(formatted, "2025-01-02 04:12:21.000");
}

#[test]
fn test_apply_modifier_subsec_truncate_to_milliseconds() {
    let mut dt = create_datetime(2025, 1, 2, 4, 12, 21)
        .with_nanosecond(891_123_456)
        .unwrap();
    apply_modifier(&mut dt, "subsec").unwrap();
    let formatted = dt.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    assert_eq!(formatted, "2025-01-02 04:12:21.891");
}

#[test]
fn test_is_leap_second() {
    let dt = DateTime::from_timestamp(1483228799, 999_999_999)
        .unwrap()
        .naive_utc();
    assert!(!is_leap_second(&dt));
    let dt = DateTime::from_timestamp(1483228799, 1_500_000_000)
        .unwrap()
        .naive_utc();
    assert!(is_leap_second(&dt));
}

#[test]
fn test_strftime() {}

#[test]
fn test_exec_timediff() {
    let start = Value::build_text("12:00:00");
    let end = Value::build_text("14:30:45");
    let expected = Value::build_text("-0000-00-00 02:30:45.000");
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("14:30:45");
    let end = Value::build_text("12:00:00");
    let expected = Value::build_text("+0000-00-00 02:30:45.000");
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("12:00:01.300");
    let end = Value::build_text("12:00:00.500");
    let expected = Value::build_text("+0000-00-00 00:00:00.800");
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("13:30:00");
    let end = Value::build_text("16:45:30");
    let expected = Value::build_text("-0000-00-00 03:15:30.000");
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("2023-05-10 23:30:00");
    let end = Value::build_text("2023-05-11 01:15:00");
    let expected = Value::build_text("-0000-00-00 01:45:00.000");
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::Null;
    let end = Value::build_text("12:00:00");
    let expected = Value::Null;
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("not a time");
    let end = Value::build_text("12:00:00");
    let expected = Value::Null;
    assert_eq!(
        exec_timediff(&[Register::Value(start), Register::Value(end)]),
        expected
    );
    let start = Value::build_text("12:00:00");
    let expected = Value::Null;
    assert_eq!(exec_timediff(&[Register::Value(start)]), expected);
}
