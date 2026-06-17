#![allow(unused_variables)]
use super::super::Register;
use crate::schema::Affinity;
use crate::{
    types::Value,
    util::{cast_real_to_integer, checked_cast_text_to_numeric},
};

#[derive(Debug, PartialEq)]
pub(super) enum NumericParseResult {
    NotNumeric,
    PureInteger,
    HasDecimalOrExp,
    ValidPrefixOnly,
}
#[derive(Debug)]
pub(super) enum ParsedNumber {
    None,
    Integer(i64),
    Float(f64),
}
impl ParsedNumber {
    pub(super) fn as_integer(&self) -> Option<i64> {
        match self {
            ParsedNumber::Integer(i) => Some(*i),
            _ => None,
        }
    }
    pub(super) fn as_float(&self) -> Option<f64> {
        match self {
            ParsedNumber::Float(f) => Some(*f),
            _ => None,
        }
    }
}
pub(super) fn apply_affinity_char(target: &mut Register, affinity: Affinity) -> bool {
    if let Register::Value(value) = target {
        if matches!(value, Value::Blob(_)) {
            return true;
        }
        match affinity {
            Affinity::Blob => return true,
            Affinity::Text => {
                if matches!(value, Value::Text(_) | Value::Null) {
                    return true;
                }
                let text = value.to_string();
                *value = Value::Text(text.into());
                return true;
            }
            Affinity::Integer | Affinity::Numeric => {
                if matches!(value, Value::Integer(_)) {
                    return true;
                }
                if !matches!(value, Value::Text(_) | Value::Float(_)) {
                    return true;
                }
                if let Value::Float(fl) = *value {
                    return try_float_to_integer_affinity(value, fl);
                }
                if let Value::Text(t) = value {
                    let text = t.as_str();
                    if text.starts_with("0x") {
                        return false;
                    }
                    let Ok(num) = checked_cast_text_to_numeric(text) else {
                        return false;
                    };
                    match num {
                        Value::Integer(i) => {
                            *value = Value::Integer(i);
                            return true;
                        }
                        Value::Float(fl) => {
                            if affinity == Affinity::Numeric {
                                return try_float_to_integer_affinity(value, fl);
                            } else {
                                *value = Value::Float(fl);
                                return true;
                            }
                        }
                        other => {
                            *value = other;
                            return true;
                        }
                    }
                }
                return false;
            }
            Affinity::Real => {
                if let Value::Integer(i) = *value {
                    *value = Value::Float(i as f64);
                    return true;
                }
                if let Value::Text(t) = value {
                    let s = t.as_str();
                    if s.starts_with("0x") {
                        return false;
                    }
                    if let Ok(num) = checked_cast_text_to_numeric(s) {
                        *value = num;
                        return true;
                    } else {
                        return false;
                    }
                }
                return true;
            }
        }
    }
    true
}
fn try_float_to_integer_affinity(value: &mut Value, fl: f64) -> bool {
    if let Ok(int_val) = cast_real_to_integer(fl) {
        if (int_val as f64) == fl && int_val > i64::MIN + 1 && int_val < i64::MAX - 1 {
            *value = Value::Integer(int_val);
            return true;
        }
    }
    *value = Value::Float(fl);
    false
}
pub(super) fn execute_sqlite_version(version_integer: i64) -> String {
    let major = version_integer / 1_000_000;
    let minor = (version_integer % 1_000_000) / 1_000;
    let release = version_integer % 1_000;
    format!("{}.{}.{}", major, minor, release)
}
pub fn extract_int_value(value: &Value) -> i64 {
    match value {
        Value::Integer(i) => *i,
        Value::Float(f) => {
            if *f < -9223372036854774784.0 {
                i64::MIN
            } else if *f > 9223372036854774784.0 {
                i64::MAX
            } else {
                *f as i64
            }
        }
        Value::Text(t) => t.as_str().parse::<i64>().unwrap_or(0),
        Value::Blob(b) => {
            if let Ok(s) = std::str::from_utf8(b) {
                s.parse::<i64>().unwrap_or(0)
            } else {
                0
            }
        }
        Value::Null => 0,
    }
}
fn try_for_float(text: &str) -> (NumericParseResult, ParsedNumber) {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return (NumericParseResult::NotNumeric, ParsedNumber::None);
    }
    let mut pos = 0;
    let len = bytes.len();
    while pos < len && is_space(bytes[pos]) {
        pos += 1;
    }
    if pos >= len {
        return (NumericParseResult::NotNumeric, ParsedNumber::None);
    }
    let start_pos = pos;
    let mut sign = 1i64;
    if bytes[pos] == b'-' {
        sign = -1;
        pos += 1;
    } else if bytes[pos] == b'+' {
        pos += 1;
    }
    if pos >= len {
        return (NumericParseResult::NotNumeric, ParsedNumber::None);
    }
    let mut significand = 0u64;
    let mut digit_count = 0;
    let mut decimal_adjust = 0i32;
    let mut has_digits = false;
    while pos < len && bytes[pos].is_ascii_digit() {
        has_digits = true;
        let digit = (bytes[pos] - b'0') as u64;
        if significand <= (u64::MAX - 9) / 10 {
            significand = significand * 10 + digit;
            digit_count += 1;
        } else {
            decimal_adjust += 1;
        }
        pos += 1;
    }
    let mut has_decimal = false;
    let mut has_exponent = false;
    if pos < len && bytes[pos] == b'.' {
        has_decimal = true;
        pos += 1;
        while pos < len && bytes[pos].is_ascii_digit() {
            has_digits = true;
            let digit = (bytes[pos] - b'0') as u64;
            if significand <= (u64::MAX - 9) / 10 {
                significand = significand * 10 + digit;
                digit_count += 1;
                decimal_adjust -= 1;
            }
            pos += 1;
        }
    }
    if !has_digits {
        return (NumericParseResult::NotNumeric, ParsedNumber::None);
    }
    let mut exponent = 0i32;
    if pos < len && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        has_exponent = true;
        pos += 1;
        if pos >= len {
            return create_result_from_significand(
                significand,
                sign,
                decimal_adjust,
                has_decimal,
                has_exponent,
                NumericParseResult::ValidPrefixOnly,
            );
        }
        let mut exp_sign = 1i32;
        if bytes[pos] == b'-' {
            exp_sign = -1;
            pos += 1;
        } else if bytes[pos] == b'+' {
            pos += 1;
        }
        if pos >= len || !bytes[pos].is_ascii_digit() {
            return create_result_from_significand(
                significand,
                sign,
                decimal_adjust,
                has_decimal,
                false,
                NumericParseResult::ValidPrefixOnly,
            );
        }
        while pos < len && bytes[pos].is_ascii_digit() {
            let digit = (bytes[pos] - b'0') as i32;
            if exponent < 10000 {
                exponent = exponent * 10 + digit;
            } else {
                exponent = 10000;
            }
            pos += 1;
        }
        exponent *= exp_sign;
    }
    while pos < len && is_space(bytes[pos]) {
        pos += 1;
    }
    let consumed_all = pos >= len;
    let final_exponent = decimal_adjust + exponent;
    let parse_result = if !consumed_all {
        NumericParseResult::ValidPrefixOnly
    } else if has_decimal || has_exponent {
        NumericParseResult::HasDecimalOrExp
    } else {
        NumericParseResult::PureInteger
    };
    create_result_from_significand(
        significand,
        sign,
        final_exponent,
        has_decimal,
        has_exponent,
        parse_result,
    )
}
fn create_result_from_significand(
    significand: u64,
    sign: i64,
    exponent: i32,
    has_decimal: bool,
    has_exponent: bool,
    parse_result: NumericParseResult,
) -> (NumericParseResult, ParsedNumber) {
    if significand == 0 {
        match parse_result {
            NumericParseResult::PureInteger => {
                return (parse_result, ParsedNumber::Integer(0));
            }
            _ => {
                return (parse_result, ParsedNumber::Float(0.0));
            }
        }
    }
    if !has_decimal && !has_exponent && exponent == 0 {
        let signed_val = (significand as i64).wrapping_mul(sign);
        if (significand as i64) * sign == signed_val {
            return (parse_result, ParsedNumber::Integer(signed_val));
        }
    }
    let mut result = significand as f64;
    let mut exp = exponent;
    if exp > 0 {
        while exp >= 100 {
            result *= 1e100;
            exp -= 100;
        }
        while exp >= 10 {
            result *= 1e10;
            exp -= 10;
        }
        while exp >= 1 {
            result *= 10.0;
            exp -= 1;
        }
    } else if exp < 0 {
        while exp <= -100 {
            result *= 1e-100;
            exp += 100;
        }
        while exp <= -10 {
            result *= 1e-10;
            exp += 10;
        }
        while exp <= -1 {
            result *= 0.1;
            exp += 1;
        }
    }
    if sign < 0 {
        result = -result;
    }
    (parse_result, ParsedNumber::Float(result))
}
pub fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')
}
fn real_to_i64(r: f64) -> i64 {
    if r < -9223372036854774784.0 {
        i64::MIN
    } else if r > 9223372036854774784.0 {
        i64::MAX
    } else {
        r as i64
    }
}
fn apply_integer_affinity(register: &mut Register) -> bool {
    let Register::Value(Value::Float(f)) = register else {
        return false;
    };
    let ix = real_to_i64(*f);
    if *f == (ix as f64) && ix > i64::MIN && ix < i64::MAX {
        *register = Register::Value(Value::Integer(ix));
        true
    } else {
        false
    }
}
/// Try to convert a value into a numeric representation if we can
/// do so without loss of information. In other words, if the string
/// looks like a number, convert it into a number. If it does not
/// look like a number, leave it alone.
pub fn apply_numeric_affinity(register: &mut Register, try_for_int: bool) -> bool {
    let Register::Value(Value::Text(text)) = register else {
        return false;
    };
    let text_str = text.as_str();
    let (parse_result, parsed_value) = try_for_float(text_str);
    match parse_result {
        NumericParseResult::NotNumeric | NumericParseResult::ValidPrefixOnly => false,
        NumericParseResult::PureInteger => {
            if let Some(int_val) = parsed_value.as_integer() {
                *register = Register::Value(Value::Integer(int_val));
                true
            } else {
                false
            }
        }
        NumericParseResult::HasDecimalOrExp => {
            if let Some(float_val) = parsed_value.as_float() {
                *register = Register::Value(Value::Float(float_val));
                if try_for_int {
                    apply_integer_affinity(register);
                }
                true
            } else {
                false
            }
        }
    }
}
pub(super) fn is_numeric_value(reg: &Register) -> bool {
    matches!(reg.get_owned_value(), Value::Integer(_) | Value::Float(_))
}
pub(super) fn stringify_register(reg: &mut Register) -> bool {
    match reg.get_owned_value() {
        Value::Integer(i) => {
            *reg = Register::Value(Value::build_text(&i.to_string()));
            true
        }
        Value::Float(f) => {
            *reg = Register::Value(Value::build_text(&f.to_string()));
            true
        }
        Value::Text(_) | Value::Null | Value::Blob(_) => false,
    }
}
