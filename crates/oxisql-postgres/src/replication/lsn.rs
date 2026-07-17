//! PostgreSQL Log Sequence Numbers (LSN) — WAL byte-offset positions.
//!
//! A Log Sequence Number identifies an exact byte position in PostgreSQL's
//! write-ahead log (WAL).  It appears throughout the streaming/logical
//! replication protocol:
//!
//! - `XLogData` (`CopyData` message, tag `w`) headers carry the starting WAL
//!   position of the record and the current end-of-WAL position.
//! - Primary and standby keepalive messages (tags `k` and `r`) exchange the
//!   current WAL flush/write/apply positions.
//! - `IDENTIFY_SYSTEM` returns the current WAL flush position of the server.
//! - `CREATE_REPLICATION_SLOT` returns the WAL position at which the new
//!   slot becomes valid.
//!
//! PostgreSQL formats an LSN in text as two hexadecimal numbers separated by
//! a slash, `"X/Y"`, where `X` holds the high 32 bits and `Y` the low 32
//! bits of the 64-bit log position (this mirrors `pg_lsn_out` in the
//! PostgreSQL backend).  [`Lsn`] wraps the combined `u64` value and provides
//! conversions to and from that text form via [`FromStr`](std::str::FromStr)
//! and [`Display`](std::fmt::Display).
//!
//! # Timestamps
//!
//! This module also provides free functions to convert between PostgreSQL's
//! replication-protocol timestamp convention (microseconds since
//! `2000-01-01 00:00:00 UTC`, the "PostgreSQL epoch") and the Unix-epoch
//! microsecond convention used by [`oxisql_core::Value::Timestamp`] elsewhere
//! in this crate (see [`crate::types`]).  Replication messages such as
//! `XLogData`, primary keepalives, and standby status updates all carry
//! PostgreSQL-epoch timestamps, so callers building or consuming those
//! messages need to convert at the boundary.

use crate::error::PgError;

// ── Lsn ────────────────────────────────────────────────────────────────────────

/// A PostgreSQL Log Sequence Number: a byte offset into the write-ahead log.
///
/// Wraps the 64-bit WAL position exactly as PostgreSQL represents it
/// internally.  Parses from and formats to PostgreSQL's canonical text form,
/// e.g. `"16/B374D848"`, via [`FromStr`](std::str::FromStr) and
/// [`Display`](std::fmt::Display).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct Lsn(pub u64);

impl Lsn {
    /// Returns the raw 64-bit WAL position.
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Constructs an [`Lsn`] from a raw 64-bit WAL position.
    pub const fn from_u64(v: u64) -> Self {
        Self(v)
    }
}

// ── FromStr ──────────────────────────────────────────────────────────────────

impl std::str::FromStr for Lsn {
    type Err = PgError;

    /// Parses PostgreSQL's canonical LSN text form, `"X/Y"`, where `X` and
    /// `Y` are hexadecimal (no `0x` prefix, case-insensitive) and each fits
    /// in 32 bits.  The resulting value is `(X << 32) | Y`.
    ///
    /// # Errors
    ///
    /// Returns [`PgError::Protocol`] if:
    /// - the `/` separator is missing, or appears more than once;
    /// - either half is empty;
    /// - either half contains a non-hexadecimal character; or
    /// - either half's numeric value does not fit in 32 bits.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((high_str, low_str)) = s.split_once('/') else {
            return Err(PgError::Protocol(format!(
                "invalid LSN {s:?}: missing '/' separator"
            )));
        };
        // `split_once` only splits on the *first* '/'; a second '/' would
        // otherwise silently end up inside `low_str`.
        if low_str.contains('/') {
            return Err(PgError::Protocol(format!(
                "invalid LSN {s:?}: expected exactly one '/' separator, found more than one"
            )));
        }
        if high_str.is_empty() {
            return Err(PgError::Protocol(format!(
                "invalid LSN {s:?}: segment before '/' is empty"
            )));
        }
        if low_str.is_empty() {
            return Err(PgError::Protocol(format!(
                "invalid LSN {s:?}: segment after '/' is empty"
            )));
        }

        let high = u32::from_str_radix(high_str, 16).map_err(|e| {
            PgError::Protocol(format!(
                "invalid LSN {s:?}: segment {high_str:?} is not a valid 32-bit hexadecimal value: {e}"
            ))
        })?;
        let low = u32::from_str_radix(low_str, 16).map_err(|e| {
            PgError::Protocol(format!(
                "invalid LSN {s:?}: segment {low_str:?} is not a valid 32-bit hexadecimal value: {e}"
            ))
        })?;

        Ok(Lsn((u64::from(high) << 32) | u64::from(low)))
    }
}

// ── Display ──────────────────────────────────────────────────────────────────

impl std::fmt::Display for Lsn {
    /// Formats as PostgreSQL's canonical LSN text form: `"X/Y"`, with `X`
    /// and `Y` the high and low 32 bits of the value rendered in uppercase
    /// hexadecimal without leading zeros (e.g. `Lsn(0x1000)` displays as
    /// `"0/1000"`, not `"0/00001000"`).
    #[allow(clippy::cast_possible_truncation)]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let high = (self.0 >> 32) as u32;
        let low = (self.0 & 0xFFFF_FFFF) as u32;
        write!(f, "{high:X}/{low:X}")
    }
}

// ── PostgreSQL-epoch <-> Unix-epoch microsecond conversion ───────────────────

/// Offset between the PostgreSQL epoch (`2000-01-01 00:00:00 UTC`) and the
/// Unix epoch (`1970-01-01 00:00:00 UTC`), in microseconds.
///
/// `946_684_800` seconds separate the two epochs (10,957 days); the
/// PostgreSQL epoch is the later of the two, so this offset is *added* when
/// converting PostgreSQL-epoch microseconds to Unix-epoch microseconds, and
/// *subtracted* for the inverse conversion.
const PG_EPOCH_OFFSET_MICROS: i64 = 946_684_800_000_000;

/// Converts a PostgreSQL replication-protocol timestamp — microseconds since
/// `2000-01-01 00:00:00 UTC` — to a Unix-epoch timestamp in microseconds
/// (microseconds since `1970-01-01 00:00:00 UTC`), matching the convention
/// used by [`oxisql_core::Value::Timestamp`] elsewhere in this crate.
///
/// # Range
///
/// Uses plain (non-checked) `i64` addition. An `i64` count of microseconds
/// spans roughly ±292,000 years from its epoch, far beyond any realistic
/// timestamp, so overflow cannot occur in practice.
pub fn pg_micros_to_unix_micros(pg_micros: i64) -> i64 {
    pg_micros + PG_EPOCH_OFFSET_MICROS
}

/// Converts a Unix-epoch timestamp in microseconds (as used by
/// [`oxisql_core::Value::Timestamp`]) to a PostgreSQL replication-protocol
/// timestamp — microseconds since `2000-01-01 00:00:00 UTC`.
///
/// Inverse of [`pg_micros_to_unix_micros`].
///
/// # Range
///
/// See [`pg_micros_to_unix_micros`] for why unchecked `i64` arithmetic is
/// safe here.
pub fn unix_micros_to_pg_micros(unix_micros: i64) -> i64 {
    unix_micros - PG_EPOCH_OFFSET_MICROS
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Lsn::from_str: valid cases ──────────────────────────────────────────

    #[test]
    fn parse_zero() {
        assert_eq!("0/0".parse::<Lsn>().unwrap(), Lsn(0));
    }

    #[test]
    fn parse_documented_example() {
        // "16/B374D848" is the canonical example from the PostgreSQL docs.
        let lsn: Lsn = "16/B374D848".parse().unwrap();
        assert_eq!(lsn, Lsn((0x16_u64 << 32) | 0xB374_D848_u64));
    }

    #[test]
    fn parse_lowercase_hex() {
        let lsn: Lsn = "a/b".parse().unwrap();
        assert_eq!(lsn, Lsn((0xa_u64 << 32) | 0xb_u64));
    }

    #[test]
    fn parse_mixed_case() {
        let lsn: Lsn = "aB/Cd".parse().unwrap();
        assert_eq!(lsn, Lsn((0xAB_u64 << 32) | 0xCD_u64));
    }

    #[test]
    fn parse_max_values() {
        let lsn: Lsn = "FFFFFFFF/FFFFFFFF".parse().unwrap();
        assert_eq!(lsn, Lsn(u64::MAX));
    }

    // ── Lsn::from_str: error cases ──────────────────────────────────────────

    #[test]
    fn parse_err_no_slash() {
        let result = "16B374D848".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_empty_string() {
        let result = "".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_two_slashes() {
        let result = "1/2/3".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_empty_high_half() {
        let result = "/1".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_empty_low_half() {
        let result = "1/".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_non_hex_high() {
        let result = "G/1".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_non_hex_low() {
        let result = "1/ZZZZ".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_high_overflows_u32() {
        // 9 hex digits => value >= 16^8 = 4_294_967_296 > u32::MAX.
        let result = "100000000/1".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    #[test]
    fn parse_err_low_overflows_u32() {
        let result = "1/100000000".parse::<Lsn>();
        assert!(matches!(result, Err(PgError::Protocol(_))));
    }

    // ── Display ──────────────────────────────────────────────────────────────

    #[test]
    fn display_zero() {
        assert_eq!(Lsn(0).to_string(), "0/0");
    }

    #[test]
    fn display_max() {
        assert_eq!(Lsn(u64::MAX).to_string(), "FFFFFFFF/FFFFFFFF");
    }

    #[test]
    fn display_no_leading_zeros() {
        // High half is zero, low half is small: neither half should be
        // zero-padded to 8 digits.
        assert_eq!(Lsn(0x1000).to_string(), "0/1000");
    }

    #[test]
    fn display_documented_example() {
        let lsn = Lsn((0x16_u64 << 32) | 0xB374_D848_u64);
        assert_eq!(lsn.to_string(), "16/B374D848");
    }

    // ── Round-trip: Display -> FromStr ───────────────────────────────────────

    #[test]
    fn round_trip_zero() {
        let lsn = Lsn(0);
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_max() {
        let lsn = Lsn(u64::MAX);
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_one() {
        let lsn = Lsn(1);
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_mid_range() {
        let lsn = Lsn(0x1234_5678_9ABC_DEF0);
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_high_bits_only() {
        let lsn = Lsn(0xFFFF_FFFF_0000_0000);
        assert_eq!(lsn.to_string(), "FFFFFFFF/0");
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_low_bits_only() {
        let lsn = Lsn(0x0000_0000_FFFF_FFFF);
        assert_eq!(lsn.to_string(), "0/FFFFFFFF");
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    #[test]
    fn round_trip_realistic_wal_position() {
        let lsn: Lsn = "16/B374D848".parse().unwrap();
        assert_eq!(lsn.to_string().parse::<Lsn>().unwrap(), lsn);
    }

    // ── as_u64 / from_u64 ─────────────────────────────────────────────────────

    #[test]
    fn as_u64_from_u64_round_trip() {
        let lsn = Lsn::from_u64(0x1234_5678);
        assert_eq!(lsn.as_u64(), 0x1234_5678);
        assert_eq!(Lsn::from_u64(lsn.as_u64()), lsn);
    }

    // ── Epoch conversion ──────────────────────────────────────────────────────

    #[test]
    fn pg_epoch_zero_is_known_unix_micros() {
        // 2000-01-01T00:00:00Z expressed in Unix-epoch microseconds.
        assert_eq!(pg_micros_to_unix_micros(0), 946_684_800_000_000);
    }

    #[test]
    fn unix_epoch_zero_is_known_pg_micros() {
        // 1970-01-01T00:00:00Z expressed in PostgreSQL-epoch microseconds
        // (negative, since the Unix epoch precedes the PostgreSQL epoch).
        assert_eq!(unix_micros_to_pg_micros(0), -946_684_800_000_000);
    }

    #[test]
    fn epoch_pg_to_unix_round_trip() {
        for pg_micros in [
            0_i64,
            1,
            -1,
            946_684_800_000_000,
            1_234_567_890_123_456,
            -1_234_567_890,
            i64::MIN / 2,
            i64::MAX / 2,
        ] {
            let unix_micros = pg_micros_to_unix_micros(pg_micros);
            assert_eq!(unix_micros_to_pg_micros(unix_micros), pg_micros);
        }
    }

    #[test]
    fn epoch_unix_to_pg_round_trip() {
        for unix_micros in [0_i64, 946_684_800_000_000, -946_684_800_000_000, 1] {
            let pg_micros = unix_micros_to_pg_micros(unix_micros);
            assert_eq!(pg_micros_to_unix_micros(pg_micros), unix_micros);
        }
    }
}
