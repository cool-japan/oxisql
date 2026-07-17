//! Code adapted from Chrono StrftimeItems but for sqlite strftime compatibility
//! Sqlite reference <https://www.sqlite.org/lang_datefunc.html>

use chrono::format::{Fixed, Item, Numeric, Pad};

const fn num(numeric: Numeric) -> Item<'static> {
    Item::Numeric(numeric, Pad::None)
}

const fn num0(numeric: Numeric) -> Item<'static> {
    Item::Numeric(numeric, Pad::Zero)
}

const fn nums(numeric: Numeric) -> Item<'static> {
    Item::Numeric(numeric, Pad::Space)
}

const fn fixed(fixed: Fixed) -> Item<'static> {
    Item::Fixed(fixed)
}

#[derive(Clone, Debug)]
pub struct CustomStrftimeItems<'a> {
    // Remaining portion of the string.
    remainder: &'a str,
    /// If the current specifier is composed of multiple formatting items (e.g. `%+`),
    /// `queue` stores a slice of `Item`s that have to be returned one by one.
    queue: &'static [Item<'static>],
    /// Pre-computed Julian day number for `%J`, SQLite's own extension with no chrono
    /// `Numeric`/`Fixed` equivalent. It has to be threaded in from the caller (rather than
    /// computed lazily while parsing) because `CustomStrftimeItems` only ever sees the
    /// format string, not the `NaiveDateTime` being formatted.
    julian_day: f64,
}

impl<'a> CustomStrftimeItems<'a> {
    pub const fn new(s: &'a str, julian_day: f64) -> CustomStrftimeItems<'a> {
        CustomStrftimeItems {
            remainder: s,
            queue: &[],
            julian_day,
        }
    }
}

// const HAVE_ALTERNATES: &str = "z";

impl<'a> Iterator for CustomStrftimeItems<'a> {
    type Item = Item<'a>;

    fn next(&mut self) -> Option<Item<'a>> {
        // We have items queued to return from a specifier composed of multiple formatting items.
        if let Some((item, remainder)) = self.queue.split_first() {
            self.queue = remainder;
            return Some(item.clone());
        }

        // Normal: we are parsing the formatting string.
        let (remainder, item) = self.parse_next_item(self.remainder)?;
        self.remainder = remainder;
        Some(item)
    }
}

impl<'a> CustomStrftimeItems<'a> {
    fn parse_next_item(&mut self, mut remainder: &'a str) -> Option<(&'a str, Item<'a>)> {
        // use InternalInternal::*;
        use Item::{Literal, Space};
        use Numeric::*;

        match remainder.chars().next() {
            // we are done
            None => None,

            // the next item is a specifier
            Some('%') => {
                remainder = &remainder[1..];

                macro_rules! next {
                    () => {
                        match remainder.chars().next() {
                            Some(x) => {
                                remainder = &remainder[x.len_utf8()..];
                                x
                            }
                            None => return Some((remainder, Item::Error)), // premature end of string
                        }
                    };
                }

                let spec = next!();
                let pad_override = match spec {
                    '-' => Some(Pad::None),
                    '0' => Some(Pad::Zero),
                    '_' => Some(Pad::Space),
                    _ => None,
                };

                // Once a pad-override flag has been consumed, re-read the actual specifier
                // character that follows it (mirrors chrono's own `StrftimeItems::parse_next_item`
                // in chrono/src/format/strftime.rs). Without this, `spec` would still hold the
                // flag character itself (e.g. `'0'`) instead of the specifier it modifies (e.g.
                // `'d'` in `%0d`), so the `item` match below would never recognize it and every
                // padded specifier would resolve to `Item::Error`.
                let spec = if pad_override.is_some() {
                    next!()
                } else {
                    spec
                };

                // let is_alternate = spec == '#';
                // if is_alternate && !HAVE_ALTERNATES.contains(spec) {
                //     return Some((remainder, Item::Error));
                // }

                macro_rules! queue {
                    [$head:expr, $($tail:expr),+ $(,)*] => ({
                        const QUEUE: &'static [Item<'static>] = &[$($tail),+];
                        self.queue = QUEUE;
                        $head
                    })
                }

                // macro_rules! queue_from_slice {
                //     ($slice:expr) => {{
                //         self.queue = &$slice[1..];
                //         $slice[0].clone()
                //     }};
                // }

                let item = match spec {
                    // day of month: 01-31
                    'd' => num0(Day),
                    // day of month without leading zero: 1-31
                    'e' => nums(Day),
                    // fractional seconds: SS.SSS
                    'f' => {
                        queue![num0(Second), fixed(Fixed::Nanosecond3)]
                    }
                    // ISO 8601 date: YYYY-MM-DD
                    'F' => queue![
                        num0(Year),
                        Literal("-"),
                        num0(Month),
                        Literal("-"),
                        num0(Day)
                    ],
                    // ISO 8601 year corresponding to %V
                    'G' => num0(IsoYear),
                    // 2-digit ISO 8601 year corresponding to %V
                    'g' => num0(IsoYearMod100),
                    // hour: 00-24
                    'H' => num0(Hour),
                    // hour for 12-hour clock: 01-12
                    'I' => num0(Hour12),
                    // day of year: 001-366
                    'j' => num0(Ordinal),
                    // hour without leading zero: 0-24
                    'k' => nums(Hour),
                    // %I without leading zero: 1-12
                    'l' => nums(Hour12),
                    // month: 01-12
                    'm' => num0(Month),
                    // minute: 00-59
                    'M' => num0(Minute),
                    // "AM" or "PM" depending on the hour
                    'p' => fixed(Fixed::UpperAmPm),
                    // "am" or "pm" depending on the hour
                    'P' => fixed(Fixed::LowerAmPm),
                    // ISO 8601 time: HH:MM
                    'R' => queue![num0(Hour), Literal(":"), num0(Minute)],
                    // seconds since 1970-01-01
                    's' => num(Timestamp),
                    // seconds: 00-59
                    'S' => num0(Second),
                    // ISO 8601 time: HH:MM:SS
                    'T' => {
                        queue![
                            num0(Hour),
                            Literal(":"),
                            num0(Minute),
                            Literal(":"),
                            num0(Second)
                        ]
                    }
                    // week of year (00-53) - week 01 starts on the first Sunday
                    'U' => num0(WeekFromSun),
                    // day of week 1-7 with Monday==1
                    'u' => num(WeekdayFromMon),
                    // ISO 8601 week of year
                    'V' => num0(IsoWeek),
                    // day of week 0-6 with Sunday==0
                    'w' => num(NumDaysFromSun),
                    // week of year (00-53) - week 01 starts on the first Monday
                    'W' => num0(WeekFromMon),
                    // year: 0000-9999
                    'Y' => num0(Year),
                    // %
                    '%' => Literal("%"),
                    // Julian day number, e.g. `2451545.000000000` (SQLite-specific; chrono's
                    // `Numeric` enum has no floating-point field to represent it, so it can't be
                    // expressed as a plain `num0(...)`/`nums(...)` item like the specifiers
                    // above). Handled inline, during this normal per-specifier parsing stage,
                    // instead of the pre-processing string-replace this used to go through in
                    // `datetime.rs`'s `strftime_format` -- so it correctly participates in
                    // `%`-escaping and honors pad overrides (`%0J`, `%_J`, `%-J`) exactly like
                    // every other specifier. Since the generic pad-override adjustment further
                    // below only understands `Item::Numeric`, the padding is applied here
                    // directly and the result is returned early, bypassing that generic step.
                    'J' => {
                        let rendered = format_julian_day(self.julian_day, pad_override);
                        return Some((remainder, Item::OwnedLiteral(rendered.into_boxed_str())));
                    }
                    _ => Item::Error, // no such specifier
                };

                // Adjust `item` if we have any padding modifier.
                // Not allowed on non-numeric items or on specifiers composed out of multiple
                // formatting items.
                if let Some(new_pad) = pad_override {
                    match item {
                        Item::Numeric(ref kind, _pad) if self.queue.is_empty() => {
                            Some((remainder, Item::Numeric(kind.clone(), new_pad)))
                        }
                        _ => Some((remainder, Item::Error)),
                    }
                } else {
                    Some((remainder, item))
                }
            }

            // the next item is space
            Some(c) if c.is_whitespace() => {
                // `%` is not a whitespace, so `c != '%'` is redundant
                let nextspec = remainder
                    .find(|c: char| !c.is_whitespace())
                    .unwrap_or(remainder.len());
                assert!(nextspec > 0);
                let item = Space(&remainder[..nextspec]);
                remainder = &remainder[nextspec..];
                Some((remainder, item))
            }

            // the next item is literal
            _ => {
                let nextspec = remainder
                    .find(|c: char| c.is_whitespace() || c == '%')
                    .unwrap_or(remainder.len());
                assert!(nextspec > 0);
                let item = Literal(&remainder[..nextspec]);
                remainder = &remainder[nextspec..];
                Some((remainder, item))
            }
        }
    }
}

/// Minimum digit width of the integer part of a pad-overridden `%J` value. SQLite does not
/// define a canonical padded width for the Julian day number (real SQLite's `strftime()`
/// doesn't support pad-override flags on any specifier in the first place -- they're a chrono
/// convenience this fork carries over), so this is simply chosen with enough headroom to make
/// `%0J`/`%_J` padding visible: every date this engine can represent (years `0000`-`9999`)
/// yields a 7-digit Julian day integer part, so 9 digits always leaves room to spare.
const JULIAN_DAY_MIN_INTEGER_WIDTH: usize = 9;

/// Number of fractional digits SQLite always uses when rendering `%J`.
const JULIAN_DAY_FRACTIONAL_DIGITS: usize = 9;

/// Renders the Julian day number for `%J`, honoring the same pad-override flags
/// (`Pad::Zero`/`Pad::Space`/`Pad::None`) that numeric specifiers like `%d`/`%e` get from
/// [`Self::parse_next_item`]. With no override this is exactly SQLite's own unpadded
/// `%.9f`-style rendering.
fn format_julian_day(julian_day: f64, pad_override: Option<Pad>) -> String {
    const FRAC: usize = JULIAN_DAY_FRACTIONAL_DIGITS;
    match pad_override {
        // No flag, or an explicit `-` (no padding): identical to SQLite's own rendering.
        None | Some(Pad::None) => format!("{julian_day:.FRAC$}"),
        Some(Pad::Zero) => {
            let width = JULIAN_DAY_MIN_INTEGER_WIDTH + 1 + FRAC;
            format!("{julian_day:0width$.FRAC$}")
        }
        Some(Pad::Space) => {
            let width = JULIAN_DAY_MIN_INTEGER_WIDTH + 1 + FRAC;
            format!("{julian_day:width$.FRAC$}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::fmt::Write;

    /// Formats `fmt` against `dt` the same way `datetime.rs`'s `strftime_format` does,
    /// including computing the Julian day and threading it into `CustomStrftimeItems`.
    fn format(dt: chrono::NaiveDateTime, fmt: &str, julian_day: f64) -> String {
        let items = CustomStrftimeItems::new(fmt, julian_day);
        let mut formatted = String::new();
        write!(formatted, "{}", dt.format_with_items(items)).unwrap();
        formatted
    }

    fn j2000() -> (chrono::NaiveDateTime, f64) {
        // 2000-01-01 12:00:00 UTC is exactly Julian day 2451545.0 (the J2000.0 epoch).
        let dt = NaiveDate::from_ymd_opt(2000, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap();
        (dt, 2451545.0)
    }

    #[test]
    fn test_julian_day_default_is_unpadded() {
        let (dt, jd) = j2000();
        assert_eq!(format(dt, "%J", jd), "2451545.000000000");
    }

    #[test]
    fn test_julian_day_escaping_is_unaffected_by_substitution() {
        // Regression check for the old pre-processing implementation, which did a blind
        // string replace of "%J" and could misfire inside an escaped "%%J" (literal "%J").
        let (dt, jd) = j2000();
        assert_eq!(format(dt, "%%J", jd), "%J");
    }

    #[test]
    fn test_julian_day_pad_override_honors_width() {
        let (dt, jd) = j2000();

        // Zero-padded override widens the integer part with leading zeros.
        let zero_padded = format(dt, "%0J", jd);
        assert_eq!(zero_padded, "002451545.000000000");

        // Space-padded override widens the integer part with leading spaces.
        let space_padded = format(dt, "%_J", jd);
        assert_eq!(space_padded, "  2451545.000000000");

        // `-` explicitly requests no padding, identical to the bare specifier.
        let unpadded = format(dt, "%-J", jd);
        assert_eq!(unpadded, "2451545.000000000");

        // The overrides actually changed the rendered width versus the default.
        let default = format(dt, "%J", jd);
        assert!(zero_padded.len() > default.len());
        assert!(space_padded.len() > default.len());
        assert_eq!(zero_padded.len(), space_padded.len());
    }

    #[test]
    fn test_pad_override_now_works_for_other_specifiers_too() {
        // `%J`'s pad override piggybacks on the same `pad_override` mechanism every other
        // specifier uses; this confirms that shared mechanism (previously dead code, since
        // the flag character was never re-read as the real specifier) now actually works.
        let dt = NaiveDate::from_ymd_opt(2024, 3, 5)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        assert_eq!(format(dt, "%d", 0.0), "05"); // default: zero-padded
        assert_eq!(format(dt, "%-d", 0.0), "5"); // no padding
        assert_eq!(format(dt, "%_d", 0.0), " 5"); // space-padded
    }
}
