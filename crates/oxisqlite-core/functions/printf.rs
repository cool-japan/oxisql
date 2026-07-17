use crate::types::Value;
use crate::vdbe::Register;
use crate::LimboError;

// TODO: Support flag/width/precision modifiers, e.g. `%!.3s`, `%05d`, `%-10s`, `%+d`, and the
// `-`, `+`, `0`, `!`, `,` flag characters (see https://www.sqlite.org/printf.html). Doing this
// properly needs a real pre-parse grammar -- an optional flag set, then an optional width,
// then an optional `.precision`, all consumed *before* the specifier character -- rather than
// the flat one-character-at-a-time `match` below, which only ever sees a bare specifier.
// That grammar would hook in right where `chars.next()` below reads the character
// immediately after `%`. Bare (unmodified) `%d`, `%i`, `%s`, `%f`, `%x`, `%X`, `%o`, `%c`,
// `%e`, `%E` are supported.
#[inline(always)]
pub fn exec_printf(values: &[Register]) -> crate::Result<Value> {
    if values.is_empty() {
        return Ok(Value::Null);
    }
    let format_str = match &values[0].get_owned_value() {
        Value::Text(t) => t.as_str(),
        _ => return Ok(Value::Null),
    };

    let mut result = String::new();
    let mut args_index = 1;
    let mut chars = format_str.chars().peekable();

    while let Some(c) = chars.next() {
        if c != '%' {
            result.push(c);
            continue;
        }

        match chars.next() {
            Some('%') => {
                result.push('%');
                continue;
            }
            // %i is a plain alias for %d (both print "a signed integer ... in decimal",
            // per SQLite's printf() docs), so they share one arm.
            Some('d') | Some('i') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let value = &values[args_index].get_owned_value();
                match value {
                    Value::Integer(_) => result.push_str(&format!("{}", value)),
                    Value::Float(_) => result.push_str(&format!("{}", value)),
                    _ => result.push_str("0".into()),
                }
                args_index += 1;
            }
            Some('s') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                match &values[args_index].get_owned_value() {
                    Value::Text(t) => result.push_str(t.as_str()),
                    Value::Null => result.push_str("(null)"),
                    v => result.push_str(&format!("{}", v)),
                }
                args_index += 1;
            }
            Some('f') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let value = &values[args_index].get_owned_value();
                match value {
                    Value::Float(f) => result.push_str(&format!("{:.6}", f)),
                    Value::Integer(i) => result.push_str(&format!("{:.6}", *i as f64)),
                    _ => result.push_str("0.0".into()),
                }
                args_index += 1;
            }
            Some('x') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let n = printf_integer_arg(values[args_index].get_owned_value());
                result.push_str(&format!("{:x}", n as u64));
                args_index += 1;
            }
            Some('X') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let n = printf_integer_arg(values[args_index].get_owned_value());
                result.push_str(&format!("{:X}", n as u64));
                args_index += 1;
            }
            Some('o') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let n = printf_integer_arg(values[args_index].get_owned_value());
                result.push_str(&format!("{:o}", n as u64));
                args_index += 1;
            }
            Some('c') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                // Per SQLite's printf() docs, the *SQL* function's %c (unlike the
                // C-language interface, which treats the argument as a raw character code
                // point) takes "a string from which the first character is extracted and
                // displayed" -- i.e. the argument is stringified exactly like %s would,
                // and only its first character is kept.
                let value = values[args_index].get_owned_value();
                if let Some(first_char) = format!("{value}").chars().next() {
                    result.push(first_char);
                }
                args_index += 1;
            }
            Some('e') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let value = &values[args_index].get_owned_value();
                let f = match value {
                    Value::Float(f) => *f,
                    Value::Integer(i) => *i as f64,
                    _ => 0.0,
                };
                result.push_str(&format_scientific(f, false));
                args_index += 1;
            }
            Some('E') => {
                if args_index >= values.len() {
                    return Err(LimboError::InvalidArgument("not enough arguments".into()));
                }
                let value = &values[args_index].get_owned_value();
                let f = match value {
                    Value::Float(f) => *f,
                    Value::Integer(i) => *i as f64,
                    _ => 0.0,
                };
                result.push_str(&format_scientific(f, true));
                args_index += 1;
            }
            None => {
                return Err(LimboError::InvalidArgument(
                    "incomplete format specifier".into(),
                ))
            }
            _ => {
                return Err(LimboError::InvalidFormatter(
                    "this formatter is not supported".into(),
                ));
            }
        }
    }
    Ok(Value::build_text(&result))
}

/// Extracts the integer argument used by `%x`/`%X`/`%o`: integers pass through unchanged,
/// floats truncate toward zero (matching Rust's `as` cast, same as C), and anything else
/// (text, blob, null) falls back to `0` -- mirroring the existing `%d` arm's fallback for
/// non-numeric arguments.
#[inline(always)]
fn printf_integer_arg(value: &Value) -> i64 {
    match value {
        Value::Integer(i) => *i,
        Value::Float(f) => *f as i64,
        _ => 0,
    }
}

/// Formats `value` the way SQLite's `%e`/`%E` do: `[-]d.dddddde±dd`, with exactly 6 digits
/// after the decimal point and an explicit sign plus a minimum of 2 digits in the exponent
/// (e.g. `1.234568e+04`, `1.230000e-04`).
///
/// Rust's `{:e}`/`{:E}` (`LowerExp`/`UpperExp`) already perform correctly-rounded scientific
/// formatting, but render the exponent bare, with no sign for positive values and no minimum
/// digit count (e.g. `1.234568e4`), so the exponent is reformatted below to match C's `%e`.
#[inline(always)]
fn format_scientific(value: f64, uppercase: bool) -> String {
    // Normalize negative zero so it prints as `0.000000e+00`, matching SQLite, instead of
    // Rust's sign-preserving `-0.000000e0`.
    let value = if value == 0.0 { 0.0 } else { value };
    let formatted = if uppercase {
        format!("{value:.6E}")
    } else {
        format!("{value:.6e}")
    };

    let exp_marker = if uppercase { 'E' } else { 'e' };
    match formatted.split_once(exp_marker) {
        Some((mantissa, exponent)) => {
            let (sign, digits) = match exponent.strip_prefix('-') {
                Some(rest) => ('-', rest),
                None => ('+', exponent),
            };
            format!("{mantissa}{exp_marker}{sign}{digits:0>2}")
        }
        // Rust always emits the exponent marker for finite numbers; this is only reachable
        // for NaN/Infinity (which SQLite's own REAL values should never carry), so fall back
        // to Rust's rendering rather than panicking.
        None => formatted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(value: &str) -> Register {
        Register::Value(Value::build_text(value))
    }

    fn integer(value: i64) -> Register {
        Register::Value(Value::Integer(value))
    }

    fn float(value: f64) -> Register {
        Register::Value(Value::Float(value))
    }

    #[test]
    fn test_printf_no_args() {
        assert_eq!(exec_printf(&[]).unwrap(), Value::Null);
    }

    #[test]
    fn test_printf_basic_string() {
        assert_eq!(
            exec_printf(&[text("Hello World")]).unwrap(),
            *text("Hello World").get_owned_value()
        );
    }

    #[test]
    fn test_printf_string_formatting() {
        let test_cases = vec![
            // Simple string substitution
            (
                vec![text("Hello, %s!"), text("World")],
                text("Hello, World!"),
            ),
            // Multiple string substitutions
            (
                vec![text("%s %s!"), text("Hello"), text("World")],
                text("Hello World!"),
            ),
            // String with null value
            (
                vec![text("Hello, %s!"), Register::Value(Value::Null)],
                text("Hello, (null)!"),
            ),
            // String with number conversion
            (vec![text("Value: %s"), integer(42)], text("Value: 42")),
            // Escaping percent sign
            (vec![text("100%% complete")], text("100% complete")),
        ];
        for (input, output) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *output.get_owned_value());
        }
    }

    #[test]
    fn test_printf_integer_formatting() {
        let test_cases = vec![
            // Basic integer formatting
            (vec![text("Number: %d"), integer(42)], text("Number: 42")),
            // Negative integer
            (vec![text("Number: %d"), integer(-42)], text("Number: -42")),
            // Multiple integers
            (
                vec![text("%d + %d = %d"), integer(2), integer(3), integer(5)],
                text("2 + 3 = 5"),
            ),
            // Non-numeric value defaults to 0
            (
                vec![text("Number: %d"), text("not a number")],
                text("Number: 0"),
            ),
        ];
        for (input, output) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *output.get_owned_value())
        }
    }

    #[test]
    fn test_printf_float_formatting() {
        let test_cases = vec![
            // Basic float formatting
            (
                vec![text("Number: %f"), float(42.5)],
                text("Number: 42.500000"),
            ),
            // Negative float
            (
                vec![text("Number: %f"), float(-42.5)],
                text("Number: -42.500000"),
            ),
            // Integer as float
            (
                vec![text("Number: %f"), integer(42)],
                text("Number: 42.000000"),
            ),
            // Multiple floats
            (
                vec![text("%f + %f = %f"), float(2.5), float(3.5), float(6.0)],
                text("2.500000 + 3.500000 = 6.000000"),
            ),
            // Non-numeric value defaults to 0.0
            (
                vec![text("Number: %f"), text("not a number")],
                text("Number: 0.0"),
            ),
        ];

        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    // Expected outputs below were all verified against real sqlite3 3.51.0, e.g.:
    //   sqlite3 :memory: "SELECT printf('%i', -42);"

    #[test]
    fn test_printf_i_is_alias_for_d() {
        let test_cases = vec![
            (vec![text("Number: %i"), integer(42)], text("Number: 42")),
            (vec![text("Number: %i"), integer(-42)], text("Number: -42")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_hex_lowercase() {
        let test_cases = vec![
            (vec![text("%x"), integer(255)], text("ff")),
            (vec![text("%x"), integer(16)], text("10")),
            // Negative integers reinterpret the 64-bit signed value as unsigned.
            (vec![text("%x"), integer(-1)], text("ffffffffffffffff")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_hex_uppercase() {
        let test_cases = vec![
            (vec![text("%X"), integer(255)], text("FF")),
            (vec![text("%X"), integer(-1)], text("FFFFFFFFFFFFFFFF")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_octal() {
        let test_cases = vec![
            (vec![text("%o"), integer(8)], text("10")),
            (vec![text("%o"), integer(64)], text("100")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_char() {
        // Per SQLite's printf() docs, for the SQL function %c stringifies the argument
        // (like %s would) and keeps only its first character -- it is NOT a character
        // code point conversion (that's only true of the C-language printf interface).
        let test_cases = vec![
            // "65" is the string form of the integer 65; its first character is '6'.
            (vec![text("%c"), integer(65)], text("6")),
            (vec![text("%c"), text("ABC")], text("A")),
            (vec![text("%c"), Register::Value(Value::Null)], text("")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_scientific_lowercase() {
        let test_cases = vec![
            (vec![text("%e"), float(12345.6789)], text("1.234568e+04")),
            (vec![text("%e"), float(0.000123)], text("1.230000e-04")),
            (vec![text("%e"), float(-12345.6789)], text("-1.234568e+04")),
            (vec![text("%e"), float(0.0)], text("0.000000e+00")),
        ];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_scientific_uppercase() {
        let test_cases = vec![(vec![text("%E"), float(12345.6789)], text("1.234568E+04"))];
        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_mixed_formatting() {
        let test_cases = vec![
            // Mix of string and integer
            (
                vec![text("%s: %d"), text("Count"), integer(42)],
                text("Count: 42"),
            ),
            // Mix of all types
            (
                vec![
                    text("%s: %d (%f%%)"),
                    text("Progress"),
                    integer(75),
                    float(75.5),
                ],
                text("Progress: 75 (75.500000%)"),
            ),
            // Complex format
            (
                vec![
                    text("Name: %s, ID: %d, Score: %f"),
                    text("John"),
                    integer(123),
                    float(95.5),
                ],
                text("Name: John, ID: 123, Score: 95.500000"),
            ),
        ];

        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }

    #[test]
    fn test_printf_error_cases() {
        let error_cases = vec![
            // Not enough arguments
            vec![text("%d %d"), integer(42)],
            // Invalid format string
            vec![text("%z"), integer(42)],
            // Incomplete format specifier
            vec![text("incomplete %")],
        ];

        for case in error_cases {
            assert!(exec_printf(&case).is_err());
        }
    }

    #[test]
    fn test_printf_edge_cases() {
        let test_cases = vec![
            // Empty format string
            (vec![text("")], text("")),
            // Only percent signs
            (vec![text("%%%%")], text("%%")),
            // String with no format specifiers
            (vec![text("No substitutions")], text("No substitutions")),
            // Multiple consecutive format specifiers
            (
                vec![text("%d%d%d"), integer(1), integer(2), integer(3)],
                text("123"),
            ),
            // Format string with special characters
            (
                vec![text("Special chars: %s"), text("\n\t\r")],
                text("Special chars: \n\t\r"),
            ),
        ];

        for (input, expected) in test_cases {
            assert_eq!(exec_printf(&input).unwrap(), *expected.get_owned_value());
        }
    }
}
