//! Pure-Rust Julian Day Number conversions.
//!
//! Replaces the GPL-licensed `julian_day_converter` crate with an inline
//! implementation using `chrono`.  The formulae are standard astronomical
//! conventions:
//!
//!   Unix seconds = (JD − 2440587.5) × 86400
//!   JD           = Unix seconds ÷ 86400 + 2440587.5

use crate::LimboError;
use chrono::NaiveDateTime;

/// Convert a Julian Day Number to a [`NaiveDateTime`].
///
/// # Formula
/// ```text
/// unix_secs = (jd - 2440587.5) * 86400
/// ```
///
/// Returns `Err` when the resulting timestamp is out of the range supported
/// by `chrono`.
pub fn julian_day_to_datetime(jd: f64) -> crate::Result<NaiveDateTime> {
    let unix_secs_f = (jd - 2440587.5) * 86400.0;
    let secs = unix_secs_f.trunc() as i64;
    // Guard against negative fract that would cause u32 overflow
    let frac = unix_secs_f.fract();
    let nanos = if frac >= 0.0 {
        (frac * 1_000_000_000.0).round() as u32
    } else {
        0
    };

    chrono::DateTime::from_timestamp(secs, nanos)
        .map(|dt| dt.naive_utc())
        .ok_or_else(|| {
            LimboError::InvalidDate(format!(
                "Julian day {} is out of the supported date range",
                jd
            ))
        })
}

/// Convert a [`NaiveDateTime`] to a Julian Day Number.
///
/// # Formula
/// ```text
/// jd = unix_secs_total / 86400.0 + 2440587.5
/// ```
#[allow(dead_code)]
pub fn datetime_to_julian_day(dt: NaiveDateTime) -> f64 {
    let secs = dt.and_utc().timestamp() as f64;
    let nanos = dt.and_utc().timestamp_subsec_nanos() as f64;
    let unix_secs_total = secs + nanos / 1_000_000_000.0;
    unix_secs_total / 86400.0 + 2440587.5
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn test_unix_epoch_to_datetime() {
        // JD 2440587.5 == 1970-01-01T00:00:00
        let jd = 2440587.5_f64;
        let dt = julian_day_to_datetime(jd).expect("conversion failed");
        let expected = NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(dt, expected);
    }

    #[test]
    fn test_j2000_to_datetime() {
        // JD 2451545.0 == 2000-01-01T12:00:00
        let jd = 2451545.0_f64;
        let dt = julian_day_to_datetime(jd).expect("conversion failed");
        let expected = NaiveDate::from_ymd_opt(2000, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        assert_eq!(dt, expected);
    }

    #[test]
    fn test_roundtrip() {
        // datetime -> JD -> datetime should be identity (within floating-point tolerance)
        let original = NaiveDate::from_ymd_opt(2024, 6, 15)
            .unwrap()
            .and_hms_opt(8, 30, 0)
            .unwrap();
        let jd = datetime_to_julian_day(original);
        let recovered = julian_day_to_datetime(jd).expect("roundtrip conversion failed");
        // Allow up to 1 second difference due to floating-point rounding
        let diff = (recovered - original).num_seconds().abs();
        assert!(diff <= 1, "roundtrip diff was {} seconds", diff);
    }

    #[test]
    fn test_datetime_to_jd_epoch() {
        let dt = NaiveDate::from_ymd_opt(1970, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let jd = datetime_to_julian_day(dt);
        // Expect 2440587.5 within floating-point precision
        assert!((jd - 2440587.5).abs() < 1e-6, "jd={}", jd);
    }

    #[test]
    fn test_out_of_range_returns_err() {
        // An astronomically large JD that exceeds chrono's range
        let result = julian_day_to_datetime(f64::MAX);
        assert!(result.is_err());
    }
}
