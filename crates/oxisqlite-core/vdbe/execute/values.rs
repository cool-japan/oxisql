#![allow(unused_variables)]
use super::super::Register;
use crate::numeric::{NullableInteger, Numeric};
use crate::schema::{affinity, Affinity};
use crate::Result;
use crate::{error::LimboError, function::MathFunc};
use crate::{
    types::Value,
    util::{cast_text_to_integer, cast_text_to_numeric, cast_text_to_real, RoundToPrecision},
};
use regex::{Regex, RegexBuilder};
use std::collections::HashMap;

impl Value {
    pub fn exec_lower(&self) -> Option<Self> {
        match self {
            Value::Text(t) => Some(Value::build_text(&t.as_str().to_lowercase())),
            t => Some(t.to_owned()),
        }
    }
    pub fn exec_length(&self) -> Self {
        match self {
            Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                Value::Integer(self.to_string().chars().count() as i64)
            }
            Value::Blob(blob) => Value::Integer(blob.len() as i64),
            _ => self.to_owned(),
        }
    }
    pub fn exec_octet_length(&self) -> Self {
        match self {
            Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                Value::Integer(self.to_string().into_bytes().len() as i64)
            }
            Value::Blob(blob) => Value::Integer(blob.len() as i64),
            _ => self.to_owned(),
        }
    }
    pub fn exec_upper(&self) -> Option<Self> {
        match self {
            Value::Text(t) => Some(Value::build_text(&t.as_str().to_uppercase())),
            t => Some(t.to_owned()),
        }
    }
    pub fn exec_sign(&self) -> Option<Value> {
        let num = match self {
            Value::Integer(i) => *i as f64,
            Value::Float(f) => *f,
            Value::Text(s) => {
                if let Ok(i) = s.as_str().parse::<i64>() {
                    i as f64
                } else if let Ok(f) = s.as_str().parse::<f64>() {
                    f
                } else {
                    return Some(Value::Null);
                }
            }
            Value::Blob(b) => match std::str::from_utf8(b) {
                Ok(s) => {
                    if let Ok(i) = s.parse::<i64>() {
                        i as f64
                    } else if let Ok(f) = s.parse::<f64>() {
                        f
                    } else {
                        return Some(Value::Null);
                    }
                }
                Err(_) => return Some(Value::Null),
            },
            _ => return Some(Value::Null),
        };
        let sign = if num > 0.0 {
            1
        } else if num < 0.0 {
            -1
        } else {
            0
        };
        Some(Value::Integer(sign))
    }
    /// Generates the Soundex code for a given word
    pub fn exec_soundex(&self) -> Value {
        let s = match self {
            Value::Null => return Value::build_text("?000"),
            Value::Text(s) => {
                if !s.as_str().chars().all(|c| c.is_ascii_alphabetic()) {
                    return Value::build_text("?000");
                }
                s.clone()
            }
            _ => return Value::build_text("?000"),
        };
        let word: String = s
            .as_str()
            .chars()
            .filter(|c| !c.is_ascii_digit())
            .collect::<String>()
            .replace(" ", "");
        if word.is_empty() {
            return Value::build_text("0000");
        }
        let soundex_code = |c| match c {
            'b' | 'f' | 'p' | 'v' => Some('1'),
            'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => Some('2'),
            'd' | 't' => Some('3'),
            'l' => Some('4'),
            'm' | 'n' => Some('5'),
            'r' => Some('6'),
            _ => None,
        };
        let word = word.to_lowercase();
        let first_letter = word.chars().next().unwrap();
        let code: String = word
            .chars()
            .skip(1)
            .filter(|&ch| ch != 'h' && ch != 'w')
            .fold(first_letter.to_string(), |mut acc, ch| {
                acc.push(ch);
                acc
            });
        let tmp: String = code
            .chars()
            .map(|ch| match soundex_code(ch) {
                Some(code) => code.to_string(),
                None => ch.to_string(),
            })
            .collect();
        let tmp = tmp.chars().fold(String::new(), |mut acc, ch| {
            if !acc.ends_with(ch) {
                acc.push(ch);
            }
            acc
        });
        let mut result = tmp
            .chars()
            .enumerate()
            .filter(|(i, ch)| *i == 0 || !matches!(ch, 'a' | 'e' | 'i' | 'o' | 'u' | 'y'))
            .map(|(_, ch)| ch)
            .collect::<String>();
        if let Some(first_digit) = result.chars().next() {
            if first_digit.is_ascii_digit() {
                result.replace_range(0..1, &first_letter.to_string());
            }
        }
        while result.len() < 4 {
            result.push('0');
        }
        result.truncate(4);
        Value::build_text(&result.to_uppercase())
    }
    pub fn exec_abs(&self) -> Result<Self> {
        match self {
            Value::Integer(x) => match i64::checked_abs(*x) {
                Some(y) => Ok(Value::Integer(y)),
                None => Err(LimboError::IntegerOverflow),
            },
            Value::Float(x) => {
                if x < &0.0 {
                    Ok(Value::Float(-x))
                } else {
                    Ok(Value::Float(*x))
                }
            }
            Value::Null => Ok(Value::Null),
            _ => Ok(Value::Float(0.0)),
        }
    }
    pub fn exec_random() -> Self {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).unwrap();
        let random_number = i64::from_ne_bytes(buf);
        Value::Integer(random_number)
    }
    pub fn exec_randomblob(&self) -> Value {
        let length = match self {
            Value::Integer(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Text(t) => t.as_str().parse().unwrap_or(1),
            _ => 1,
        }
        .max(1) as usize;
        let mut blob: Vec<u8> = vec![0; length];
        getrandom::fill(&mut blob).expect("Failed to generate random blob");
        Value::Blob(blob)
    }
    pub fn exec_quote(&self) -> Self {
        match self {
            Value::Null => Value::build_text("NULL"),
            Value::Integer(_) | Value::Float(_) => self.to_owned(),
            Value::Blob(_) => todo!(),
            Value::Text(s) => {
                let mut quoted = String::with_capacity(s.as_str().len() + 2);
                quoted.push('\'');
                for c in s.as_str().chars() {
                    if c == '\0' {
                        break;
                    } else if c == '\'' {
                        quoted.push('\'');
                        quoted.push(c);
                    } else {
                        quoted.push(c);
                    }
                }
                quoted.push('\'');
                Value::build_text(&quoted)
            }
        }
    }
    pub fn exec_nullif(&self, second_value: &Self) -> Self {
        if self != second_value {
            self.clone()
        } else {
            Value::Null
        }
    }
    pub fn exec_substring(
        str_value: &Value,
        start_value: &Value,
        length_value: Option<&Value>,
    ) -> Value {
        if let (Value::Text(str), Value::Integer(start)) = (str_value, start_value) {
            let str_len = str.as_str().len() as i64;
            let first_position = if *start < 0 {
                str_len.saturating_sub((*start).abs())
            } else {
                *start - 1
            };
            let last_position = match length_value {
                Some(Value::Integer(length)) => first_position + *length,
                _ => str_len,
            };
            let (start, end) = if first_position <= last_position {
                (first_position, last_position)
            } else {
                (last_position, first_position)
            };
            Value::build_text(
                &str.as_str()[start.clamp(-0, str_len) as usize..end.clamp(0, str_len) as usize],
            )
        } else {
            Value::Null
        }
    }
    pub fn exec_instr(&self, pattern: &Value) -> Value {
        if self == &Value::Null || pattern == &Value::Null {
            return Value::Null;
        }
        if let (Value::Blob(reg), Value::Blob(pattern)) = (self, pattern) {
            let result = reg
                .windows(pattern.len())
                .position(|window| window == *pattern)
                .map_or(0, |i| i + 1);
            return Value::Integer(result as i64);
        }
        let reg_str;
        let reg = match self {
            Value::Text(s) => s.as_str(),
            _ => {
                reg_str = self.to_string();
                reg_str.as_str()
            }
        };
        let pattern_str;
        let pattern = match pattern {
            Value::Text(s) => s.as_str(),
            _ => {
                pattern_str = pattern.to_string();
                pattern_str.as_str()
            }
        };
        match reg.find(pattern) {
            Some(position) => Value::Integer(position as i64 + 1),
            None => Value::Integer(0),
        }
    }
    pub fn exec_typeof(&self) -> Value {
        match self {
            Value::Null => Value::build_text("null"),
            Value::Integer(_) => Value::build_text("integer"),
            Value::Float(_) => Value::build_text("real"),
            Value::Text(_) => Value::build_text("text"),
            Value::Blob(_) => Value::build_text("blob"),
        }
    }
    pub fn exec_hex(&self) -> Value {
        match self {
            Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                let text = self.to_string();
                Value::build_text(&hex::encode_upper(text))
            }
            Value::Blob(blob_bytes) => Value::build_text(&hex::encode_upper(blob_bytes)),
            _ => Value::Null,
        }
    }
    pub fn exec_unhex(&self, ignored_chars: Option<&Value>) -> Value {
        match self {
            Value::Null => Value::Null,
            _ => match ignored_chars {
                None => match hex::decode(self.to_string()) {
                    Ok(bytes) => Value::Blob(bytes),
                    Err(_) => Value::Null,
                },
                Some(ignore) => match ignore {
                    Value::Text(_) => {
                        let pat = ignore.to_string();
                        let trimmed = self
                            .to_string()
                            .trim_start_matches(|x| pat.contains(x))
                            .trim_end_matches(|x| pat.contains(x))
                            .to_string();
                        match hex::decode(trimmed) {
                            Ok(bytes) => Value::Blob(bytes),
                            Err(_) => Value::Null,
                        }
                    }
                    _ => Value::Null,
                },
            },
        }
    }
    pub fn exec_unicode(&self) -> Value {
        match self {
            Value::Text(_) | Value::Integer(_) | Value::Float(_) | Value::Blob(_) => {
                let text = self.to_string();
                if let Some(first_char) = text.chars().next() {
                    Value::Integer(first_char as u32 as i64)
                } else {
                    Value::Null
                }
            }
            _ => Value::Null,
        }
    }
    fn _to_float(&self) -> f64 {
        match self {
            Value::Text(x) => match cast_text_to_numeric(x.as_str()) {
                Value::Integer(i) => i as f64,
                Value::Float(f) => f,
                _ => unreachable!(),
            },
            Value::Integer(x) => *x as f64,
            Value::Float(x) => *x,
            _ => 0.0,
        }
    }
    pub fn exec_round(&self, precision: Option<&Value>) -> Value {
        let reg = self._to_float();
        let round = |reg: f64, f: f64| {
            let precision = if f < 1.0 { 0.0 } else { f };
            Value::Float(reg.round_to_precision(precision as i32))
        };
        match precision {
            Some(Value::Text(x)) => match cast_text_to_numeric(x.as_str()) {
                Value::Integer(i) => round(reg, i as f64),
                Value::Float(f) => round(reg, f),
                _ => unreachable!(),
            },
            Some(Value::Integer(i)) => round(reg, *i as f64),
            Some(Value::Float(f)) => round(reg, *f),
            None => round(reg, 0.0),
            _ => Value::Null,
        }
    }
    pub fn exec_trim(&self, pattern: Option<&Value>) -> Value {
        match (self, pattern) {
            (reg, Some(pattern)) => match reg {
                Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                    let pattern_chars: Vec<char> = pattern.to_string().chars().collect();
                    Value::build_text(reg.to_string().trim_matches(&pattern_chars[..]))
                }
                _ => reg.to_owned(),
            },
            (Value::Text(t), None) => Value::build_text(t.as_str().trim()),
            (reg, _) => reg.to_owned(),
        }
    }
    pub fn exec_rtrim(&self, pattern: Option<&Value>) -> Value {
        match (self, pattern) {
            (reg, Some(pattern)) => match reg {
                Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                    let pattern_chars: Vec<char> = pattern.to_string().chars().collect();
                    Value::build_text(reg.to_string().trim_end_matches(&pattern_chars[..]))
                }
                _ => reg.to_owned(),
            },
            (Value::Text(t), None) => Value::build_text(t.as_str().trim_end()),
            (reg, _) => reg.to_owned(),
        }
    }
    pub fn exec_ltrim(&self, pattern: Option<&Value>) -> Value {
        match (self, pattern) {
            (reg, Some(pattern)) => match reg {
                Value::Text(_) | Value::Integer(_) | Value::Float(_) => {
                    let pattern_chars: Vec<char> = pattern.to_string().chars().collect();
                    Value::build_text(reg.to_string().trim_start_matches(&pattern_chars[..]))
                }
                _ => reg.to_owned(),
            },
            (Value::Text(t), None) => Value::build_text(t.as_str().trim_start()),
            (reg, _) => reg.to_owned(),
        }
    }
    pub fn exec_zeroblob(&self) -> Value {
        let length: i64 = match self {
            Value::Integer(i) => *i,
            Value::Float(f) => *f as i64,
            Value::Text(s) => s.as_str().parse().unwrap_or(0),
            _ => 0,
        };
        Value::Blob(vec![0; length.max(0) as usize])
    }
    pub fn exec_if(&self, jump_if_null: bool, not: bool) -> bool {
        Numeric::from(self)
            .try_into_bool()
            .map(|jump| if not { !jump } else { jump })
            .unwrap_or(jump_if_null)
    }
    pub fn exec_cast(&self, datatype: &str) -> Value {
        if matches!(self, Value::Null) {
            return Value::Null;
        }
        match affinity(datatype) {
            Affinity::Blob => {
                let text = self.to_string();
                Value::Blob(text.into_bytes())
            }
            Affinity::Text => Value::build_text(&self.to_string()),
            Affinity::Real => match self {
                Value::Blob(b) => {
                    let text = String::from_utf8_lossy(b);
                    cast_text_to_real(&text)
                }
                Value::Text(t) => cast_text_to_real(t.as_str()),
                Value::Integer(i) => Value::Float(*i as f64),
                Value::Float(f) => Value::Float(*f),
                _ => Value::Float(0.0),
            },
            Affinity::Integer => match self {
                Value::Blob(b) => {
                    let text = String::from_utf8_lossy(b);
                    cast_text_to_integer(&text)
                }
                Value::Text(t) => cast_text_to_integer(t.as_str()),
                Value::Integer(i) => Value::Integer(*i),
                Value::Float(f) => {
                    let i = f.trunc() as i128;
                    if i > i64::MAX as i128 {
                        Value::Integer(i64::MAX)
                    } else if i < i64::MIN as i128 {
                        Value::Integer(i64::MIN)
                    } else {
                        Value::Integer(i as i64)
                    }
                }
                _ => Value::Integer(0),
            },
            Affinity::Numeric => match self {
                Value::Blob(b) => {
                    let text = String::from_utf8_lossy(b);
                    cast_text_to_numeric(&text)
                }
                Value::Text(t) => cast_text_to_numeric(t.as_str()),
                Value::Integer(i) => Value::Integer(*i),
                Value::Float(f) => Value::Float(*f),
                _ => self.clone(),
            },
        }
    }
    pub fn exec_replace(source: &Value, pattern: &Value, replacement: &Value) -> Value {
        if matches!(source, Value::Null)
            || matches!(pattern, Value::Null)
            || matches!(replacement, Value::Null)
        {
            return Value::Null;
        }
        let source = source.exec_cast("TEXT");
        let pattern = pattern.exec_cast("TEXT");
        let replacement = replacement.exec_cast("TEXT");
        match (&source, &pattern, &replacement) {
            (Value::Text(source), Value::Text(pattern), Value::Text(replacement)) => {
                if pattern.as_str().is_empty() {
                    return Value::Text(source.clone());
                }
                let result = source
                    .as_str()
                    .replace(pattern.as_str(), replacement.as_str());
                Value::build_text(&result)
            }
            _ => unreachable!("text cast should never fail"),
        }
    }
    fn to_f64(&self) -> Option<f64> {
        match self {
            Value::Integer(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            Value::Text(t) => t.as_str().parse::<f64>().ok(),
            _ => None,
        }
    }
    pub(super) fn exec_math_unary(&self, function: &MathFunc) -> Value {
        if let Value::Integer(_) = self {
            if matches! {
                function, MathFunc::Ceil | MathFunc::Ceiling | MathFunc::Floor |
                MathFunc::Trunc
            } {
                return self.clone();
            }
        }
        let f = match self.to_f64() {
            Some(f) => f,
            None => return Value::Null,
        };
        let result = match function {
            MathFunc::Acos => libm::acos(f),
            MathFunc::Acosh => libm::acosh(f),
            MathFunc::Asin => libm::asin(f),
            MathFunc::Asinh => libm::asinh(f),
            MathFunc::Atan => libm::atan(f),
            MathFunc::Atanh => libm::atanh(f),
            MathFunc::Ceil | MathFunc::Ceiling => libm::ceil(f),
            MathFunc::Cos => libm::cos(f),
            MathFunc::Cosh => libm::cosh(f),
            MathFunc::Degrees => f.to_degrees(),
            MathFunc::Exp => libm::exp(f),
            MathFunc::Floor => libm::floor(f),
            MathFunc::Ln => libm::log(f),
            MathFunc::Log10 => libm::log10(f),
            MathFunc::Log2 => libm::log2(f),
            MathFunc::Radians => f.to_radians(),
            MathFunc::Sin => libm::sin(f),
            MathFunc::Sinh => libm::sinh(f),
            MathFunc::Sqrt => libm::sqrt(f),
            MathFunc::Tan => libm::tan(f),
            MathFunc::Tanh => libm::tanh(f),
            MathFunc::Trunc => libm::trunc(f),
            _ => unreachable!("Unexpected mathematical unary function {:?}", function),
        };
        if result.is_nan() {
            Value::Null
        } else {
            Value::Float(result)
        }
    }
    pub(super) fn exec_math_binary(&self, rhs: &Value, function: &MathFunc) -> Value {
        let lhs = match self.to_f64() {
            Some(f) => f,
            None => return Value::Null,
        };
        let rhs = match rhs.to_f64() {
            Some(f) => f,
            None => return Value::Null,
        };
        let result = match function {
            MathFunc::Atan2 => libm::atan2(lhs, rhs),
            MathFunc::Mod => libm::fmod(lhs, rhs),
            MathFunc::Pow | MathFunc::Power => libm::pow(lhs, rhs),
            _ => unreachable!("Unexpected mathematical binary function {:?}", function),
        };
        if result.is_nan() {
            Value::Null
        } else {
            Value::Float(result)
        }
    }
    pub(super) fn exec_math_log(&self, base: Option<&Value>) -> Value {
        let f = match self.to_f64() {
            Some(f) => f,
            None => return Value::Null,
        };
        let base = match base {
            Some(base) => match base.to_f64() {
                Some(f) => f,
                None => return Value::Null,
            },
            None => 10.0,
        };
        if f <= 0.0 || base <= 0.0 || base == 1.0 {
            return Value::Null;
        }
        let log_x = libm::log(f);
        let log_base = libm::log(base);
        let result = log_x / log_base;
        Value::Float(result)
    }
    pub(super) fn exec_likely(&self) -> Value {
        self.clone()
    }
    pub(super) fn exec_likelihood(&self, _probability: &Value) -> Value {
        self.clone()
    }
    pub fn exec_add(&self, rhs: &Value) -> Value {
        (Numeric::from(self) + Numeric::from(rhs)).into()
    }
    pub fn exec_subtract(&self, rhs: &Value) -> Value {
        (Numeric::from(self) - Numeric::from(rhs)).into()
    }
    pub fn exec_multiply(&self, rhs: &Value) -> Value {
        (Numeric::from(self) * Numeric::from(rhs)).into()
    }
    pub fn exec_divide(&self, rhs: &Value) -> Value {
        (Numeric::from(self) / Numeric::from(rhs)).into()
    }
    pub fn exec_bit_and(&self, rhs: &Value) -> Value {
        (NullableInteger::from(self) & NullableInteger::from(rhs)).into()
    }
    pub fn exec_bit_or(&self, rhs: &Value) -> Value {
        (NullableInteger::from(self) | NullableInteger::from(rhs)).into()
    }
    pub fn exec_remainder(&self, rhs: &Value) -> Value {
        let convert_to_float = matches!(Numeric::from(self), Numeric::Float(_))
            || matches!(Numeric::from(rhs), Numeric::Float(_));
        match NullableInteger::from(self) % NullableInteger::from(rhs) {
            NullableInteger::Null => Value::Null,
            NullableInteger::Integer(v) => {
                if convert_to_float {
                    Value::Float(v as f64)
                } else {
                    Value::Integer(v)
                }
            }
        }
    }
    pub fn exec_bit_not(&self) -> Value {
        (!NullableInteger::from(self)).into()
    }
    pub fn exec_shift_left(&self, rhs: &Value) -> Value {
        (NullableInteger::from(self) << NullableInteger::from(rhs)).into()
    }
    pub fn exec_shift_right(&self, rhs: &Value) -> Value {
        (NullableInteger::from(self) >> NullableInteger::from(rhs)).into()
    }
    pub fn exec_boolean_not(&self) -> Value {
        match Numeric::from(self).try_into_bool() {
            None => Value::Null,
            Some(v) => Value::Integer(!v as i64),
        }
    }
    pub fn exec_concat(&self, rhs: &Value) -> Value {
        match (self, rhs) {
            (Value::Text(lhs_text), Value::Text(rhs_text)) => {
                Value::build_text(&(lhs_text.as_str().to_string() + rhs_text.as_str()))
            }
            (Value::Text(lhs_text), Value::Integer(rhs_int)) => {
                Value::build_text(&(lhs_text.as_str().to_string() + &rhs_int.to_string()))
            }
            (Value::Text(lhs_text), Value::Float(rhs_float)) => {
                Value::build_text(&(lhs_text.as_str().to_string() + &rhs_float.to_string()))
            }
            (Value::Integer(lhs_int), Value::Text(rhs_text)) => {
                Value::build_text(&(lhs_int.to_string() + rhs_text.as_str()))
            }
            (Value::Integer(lhs_int), Value::Integer(rhs_int)) => {
                Value::build_text(&(lhs_int.to_string() + &rhs_int.to_string()))
            }
            (Value::Integer(lhs_int), Value::Float(rhs_float)) => {
                Value::build_text(&(lhs_int.to_string() + &rhs_float.to_string()))
            }
            (Value::Float(lhs_float), Value::Text(rhs_text)) => {
                Value::build_text(&(lhs_float.to_string() + rhs_text.as_str()))
            }
            (Value::Float(lhs_float), Value::Integer(rhs_int)) => {
                Value::build_text(&(lhs_float.to_string() + &rhs_int.to_string()))
            }
            (Value::Float(lhs_float), Value::Float(rhs_float)) => {
                Value::build_text(&(lhs_float.to_string() + &rhs_float.to_string()))
            }
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (Value::Blob(_), _) | (_, Value::Blob(_)) => {
                todo!("TODO: Handle Blob conversion to String")
            }
        }
    }
    pub fn exec_and(&self, rhs: &Value) -> Value {
        match (
            Numeric::from(self).try_into_bool(),
            Numeric::from(rhs).try_into_bool(),
        ) {
            (Some(false), _) | (_, Some(false)) => Value::Integer(0),
            (None, _) | (_, None) => Value::Null,
            _ => Value::Integer(1),
        }
    }
    pub fn exec_or(&self, rhs: &Value) -> Value {
        match (
            Numeric::from(self).try_into_bool(),
            Numeric::from(rhs).try_into_bool(),
        ) {
            (Some(true), _) | (_, Some(true)) => Value::Integer(1),
            (None, _) | (_, None) => Value::Null,
            _ => Value::Integer(0),
        }
    }
    pub fn exec_like(
        regex_cache: Option<&mut HashMap<String, Regex>>,
        pattern: &str,
        text: &str,
    ) -> bool {
        if let Some(cache) = regex_cache {
            match cache.get(pattern) {
                Some(re) => re.is_match(text),
                None => {
                    let re = construct_like_regex(pattern);
                    let res = re.is_match(text);
                    cache.insert(pattern.to_string(), re);
                    res
                }
            }
        } else {
            let re = construct_like_regex(pattern);
            re.is_match(text)
        }
    }
    pub fn exec_min<'a, T: Iterator<Item = &'a Value>>(regs: T) -> Value {
        regs.min().map(|v| v.to_owned()).unwrap_or(Value::Null)
    }
    pub fn exec_max<'a, T: Iterator<Item = &'a Value>>(regs: T) -> Value {
        regs.max().map(|v| v.to_owned()).unwrap_or(Value::Null)
    }
}
pub(super) fn exec_concat_strings(registers: &[Register]) -> Value {
    let mut result = String::new();
    for reg in registers {
        match reg.get_owned_value() {
            Value::Null => continue,
            Value::Blob(_) => todo!("TODO concat blob"),
            v => result.push_str(&format!("{}", v)),
        }
    }
    Value::build_text(&result)
}
pub(super) fn exec_concat_ws(registers: &[Register]) -> Value {
    if registers.is_empty() {
        return Value::Null;
    }
    let separator = match &registers[0].get_owned_value() {
        Value::Null | Value::Blob(_) => return Value::Null,
        v => format!("{}", v),
    };
    let mut result = String::new();
    for (i, reg) in registers.iter().enumerate().skip(1) {
        if i > 1 {
            result.push_str(&separator);
        }
        match reg.get_owned_value() {
            v if matches!(v, Value::Text(_) | Value::Integer(_) | Value::Float(_)) => {
                result.push_str(&format!("{}", v))
            }
            _ => continue,
        }
    }
    Value::build_text(&result)
}
pub(super) fn exec_char(values: &[Register]) -> Value {
    let result: String = values
        .iter()
        .filter_map(|x| {
            if let Value::Integer(i) = x.get_owned_value() {
                Some(*i as u8 as char)
            } else {
                None
            }
        })
        .collect();
    Value::build_text(&result)
}
fn construct_like_regex(pattern: &str) -> Regex {
    let mut regex_pattern = String::with_capacity(pattern.len() * 2);
    regex_pattern.push('^');
    for c in pattern.chars() {
        match c {
            '\\' => regex_pattern.push_str("\\\\"),
            '%' => regex_pattern.push_str(".*"),
            '_' => regex_pattern.push('.'),
            ch => {
                if regex_syntax::is_meta_character(c) {
                    regex_pattern.push('\\');
                }
                regex_pattern.push(ch);
            }
        }
    }
    regex_pattern.push('$');
    RegexBuilder::new(&regex_pattern)
        .case_insensitive(true)
        .dot_matches_new_line(true)
        .build()
        .unwrap()
}
