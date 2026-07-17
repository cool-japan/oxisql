//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::Result;

use super::constants::{CHARACTER_TYPE, CHARACTER_TYPE_OK, WS_TABLE};
use super::jsonb_type::Jsonb;
use super::types::{ElementType, JsonTraversalResult, PathOperationMode};

pub(super) const fn make_whitespace_table() -> [u8; 256] {
    let mut table = [0u8; 256];

    // Mark whitespace characters
    table[0x09] = 1; // Tab
    table[0x0A] = 1; // Line feed
    table[0x0D] = 1; // Carriage return
    table[0x20] = 1; // Space

    table
}

pub(super) const fn make_character_type_table() -> [u8; 256] {
    let mut table = [0u8; 256];

    // Mark whitespace characters
    table[0x09] = 1; // Tab
    table[0x0A] = 1; // Line feed
    table[0x0D] = 1; // Carriage return
    table[0x20] = 1; // Space

    // Mark numeric digits
    table[0x30] = 2; // 0
    table[0x31] = 2; // 1
    table[0x32] = 2; // 2
    table[0x33] = 2; // 3
    table[0x34] = 2; // 4
    table[0x35] = 2; // 5
    table[0x36] = 2; // 6
    table[0x37] = 2; // 7
    table[0x38] = 2; // 8
    table[0x39] = 2; // 9

    // Mark hex digits (a-f, A-F)
    table[0x41] = 3; // A
    table[0x42] = 3; // B
    table[0x43] = 3; // C
    table[0x44] = 3; // D
    table[0x45] = 3; // E
    table[0x46] = 3; // F
    table[0x61] = 3; // a
    table[0x62] = 3; // b
    table[0x63] = 3; // c
    table[0x64] = 3; // d
    table[0x65] = 3; // e
    table[0x66] = 3; // f

    table
}

pub(super) const fn make_character_type_ok_table() -> [u8; 256] {
    let mut table = [0u8; 256];

    table[0x20] |= 4; // Space
    table[0x21] |= 4; // !
                      // Skipping 0x22 (") as it needs escaping
    table[0x23] |= 4; // #
    table[0x24] |= 4; // $
    table[0x25] |= 4; // %
    table[0x26] |= 4; // &
    table[0x27] |= 4; // '
    table[0x28] |= 4; // (
    table[0x29] |= 4; // )
    table[0x2A] |= 4; // *
    table[0x2B] |= 4; // +
    table[0x2C] |= 4; // ,
    table[0x2D] |= 4; // -
    table[0x2E] |= 4; // .
    table[0x2F] |= 4; // /
    table[0x30] |= 4; // 0
    table[0x31] |= 4; // 1
    table[0x32] |= 4; // 2
    table[0x33] |= 4; // 3
    table[0x34] |= 4; // 4
    table[0x35] |= 4; // 5
    table[0x36] |= 4; // 6
    table[0x37] |= 4; // 7
    table[0x38] |= 4; // 8
    table[0x39] |= 4; // 9
    table[0x3A] |= 4; // :
    table[0x3B] |= 4; // ;
    table[0x3C] |= 4; //
    table[0x3D] |= 4; // =
    table[0x3E] |= 4; // >
    table[0x3F] |= 4; // ?
    table[0x40] |= 4; // @
    table[0x41] |= 4; // A
    table[0x42] |= 4; // B
    table[0x43] |= 4; // C
    table[0x44] |= 4; // D
    table[0x45] |= 4; // E
    table[0x46] |= 4; // F
    table[0x47] |= 4; // G
    table[0x48] |= 4; // H
    table[0x49] |= 4; // I
    table[0x4A] |= 4; // J
    table[0x4B] |= 4; // K
    table[0x4C] |= 4; // L
    table[0x4D] |= 4; // M
    table[0x4E] |= 4; // N
    table[0x4F] |= 4; // O
    table[0x50] |= 4; // P
    table[0x51] |= 4; // Q
    table[0x52] |= 4; // R
    table[0x53] |= 4; // S
    table[0x54] |= 4; // T
    table[0x55] |= 4; // U
    table[0x56] |= 4; // V
    table[0x57] |= 4; // W
    table[0x58] |= 4; // X
    table[0x59] |= 4; // Y
    table[0x5A] |= 4; // Z
    table[0x5B] |= 4; // [
                      // Skipping 0x5C (\) as it needs escaping
    table[0x5D] |= 4; // ]
    table[0x5E] |= 4; // ^
    table[0x5F] |= 4; // _
    table[0x60] |= 4; // `
    table[0x61] |= 4; // a
    table[0x62] |= 4; // b
    table[0x63] |= 4; // c
    table[0x64] |= 4; // d
    table[0x65] |= 4; // e
    table[0x66] |= 4; // f
    table[0x67] |= 4; // g
    table[0x68] |= 4; // h
    table[0x69] |= 4; // i
    table[0x6A] |= 4; // j
    table[0x6B] |= 4; // k
    table[0x6C] |= 4; // l
    table[0x6D] |= 4; // m
    table[0x6E] |= 4; // n
    table[0x6F] |= 4; // o
    table[0x70] |= 4; // p
    table[0x71] |= 4; // q
    table[0x72] |= 4; // r
    table[0x73] |= 4; // s
    table[0x74] |= 4; // t
    table[0x75] |= 4; // u
    table[0x76] |= 4; // v
    table[0x77] |= 4; // w
    table[0x78] |= 4; // x
    table[0x79] |= 4; // y
    table[0x7A] |= 4; // z
    table[0x7B] |= 4; // {
    table[0x7C] |= 4; // |
    table[0x7D] |= 4; // }
    table[0x7E] |= 4; // ~

    table
}

impl From<ElementType> for String {
    fn from(element_type: ElementType) -> String {
        match element_type {
            ElementType::ARRAY => "array".to_string(),
            ElementType::OBJECT => "object".to_string(),
            ElementType::NULL => "null".to_string(),
            ElementType::TRUE => "true".to_string(),
            ElementType::FALSE => "false".to_string(),
            ElementType::FLOAT | ElementType::FLOAT5 => "real".to_string(),
            ElementType::INT | ElementType::INT5 => "integer".to_string(),
            ElementType::TEXT | ElementType::TEXT5 | ElementType::TEXTJ | ElementType::TEXTRAW => {
                "text".to_string()
            }
            _ => unreachable!(),
        }
    }
}

pub trait PathOperation {
    // Get the operation mode for this operation
    fn operation_mode(&self) -> PathOperationMode;

    // Execute the actual operation
    fn execute(&mut self, json: &mut Jsonb, stack: Vec<JsonTraversalResult>) -> Result<()>;

    // Name of the operation for logging/debugging
}

#[inline]
pub(super) fn compare(key: (&str, ElementType), path_key: (&str, bool)) -> bool {
    let (key, element_type) = key;
    let (path_key, is_raw) = path_key;
    if !is_raw && element_type == ElementType::TEXT {
        if key.len() == path_key.len() {
            return key == path_key;
        } else {
            return false;
        }
    }
    if !is_raw {
        return unescape_string(key) == path_key;
    }
    match element_type {
        ElementType::TEXTJ | ElementType::TEXT5 | ElementType::TEXTRAW | ElementType::TEXT => {
            return unescape_string(key) == unescape_string(path_key);
        }
        _ => {}
    };

    false
}

#[inline]
pub fn unescape_string(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut code_point = String::with_capacity(5);

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('t') => result.push('\t'),
                Some('\\') => result.push('\\'),
                Some('/') => result.push('/'),
                Some('"') => result.push('"'),
                Some('b') => result.push('\u{0008}'),
                Some('f') => result.push('\u{000C}'),
                Some('x') => {
                    code_point.clear();
                    for _ in 0..2 {
                        if let Some(hex_char) = chars.next() {
                            code_point.push(hex_char);
                        } else {
                            break;
                        }
                    }
                    if let Ok(code) = u16::from_str_radix(&code_point, 16) {
                        if let Some(ch) = char::from_u32(code as u32) {
                            result.push(ch)
                        }
                    }
                }
                // Handle \uXXXX format (JSON style)
                Some('u') => {
                    code_point.clear();
                    for _ in 0..4 {
                        if let Some(hex_char) = chars.next() {
                            code_point.push(hex_char);
                        } else {
                            break;
                        }
                    }

                    if let Ok(code) = u16::from_str_radix(&code_point, 16) {
                        // Check if this is a high surrogate
                        if matches!(code, 0xD800..=0xDBFF) {
                            if chars.next() == Some('\\') && chars.next() == Some('u') {
                                code_point.clear();
                                for _ in 0..4 {
                                    if let Some(hex_char) = chars.next() {
                                        code_point.push(hex_char);
                                    } else {
                                        break;
                                    }
                                }

                                if let Ok(low_code) = u16::from_str_radix(&code_point, 16) {
                                    if (0xDC00..=0xDFFF).contains(&low_code) {
                                        let high_ten_bits = (code - 0xD800) as u32;
                                        let low_ten_bits = (low_code - 0xDC00) as u32;
                                        let code_point = (high_ten_bits << 10) | low_ten_bits;
                                        let unicode_value = code_point + 0x10000;

                                        if let Some(unicode_char) = char::from_u32(unicode_value) {
                                            result.push(unicode_char);
                                        }
                                    } else {
                                        // If low surrogate is invalid, just push both as separate chars
                                        if let Some(c1) = char::from_u32(code as u32) {
                                            result.push(c1);
                                        }
                                        if let Some(c2) = char::from_u32(low_code as u32) {
                                            result.push(c2);
                                        }
                                    }
                                }
                            } else {
                                // No low surrogate, just push the high surrogate as is
                                if let Some(unicode_char) = char::from_u32(code as u32) {
                                    result.push(unicode_char);
                                }
                            }
                        } else {
                            // Not a surrogate pair, just a regular Unicode character
                            if let Some(unicode_char) = char::from_u32(code as u32) {
                                result.push(unicode_char);
                            }
                        }
                    }
                }

                Some(c) => {
                    // For any other escape sequence we don't recognize,
                    // just output the backslash and the character
                    result.push('\\');
                    result.push(c);
                }
                None => {
                    // Handle trailing backslash
                    result.push('\\');
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

#[inline]
pub fn skip_whitespace(input: &[u8], mut pos: usize) -> usize {
    let len = input.len();
    if pos >= len {
        return pos;
    }

    // Fast path for non-whitespace, non-comment
    if (WS_TABLE[input[pos] as usize] & 1) == 0 && input[pos] != b'/' {
        return pos;
    }

    // Process whitespace and comments
    while pos < len {
        let ch = input[pos];
        if (WS_TABLE[ch as usize] & 1) != 0 {
            // Skip whitespace
            pos += 1;
        } else if ch == b'/' && pos + 1 < len {
            // Handle JSON5 comments
            match input[pos + 1] {
                b'/' => {
                    // Line comment - skip until newline
                    pos += 2;
                    while pos < len && input[pos] != b'\n' {
                        pos += 1;
                    }
                    if pos < len {
                        pos += 1; // Skip the newline
                    }
                }
                b'*' => {
                    // Block comment - skip until "*/"
                    pos += 2;
                    while pos + 1 < len {
                        if input[pos] == b'*' && input[pos + 1] == b'/' {
                            pos += 2;
                            break;
                        }
                        pos += 1;
                    }
                }
                _ => {
                    // Not a comment
                    break;
                }
            }
        } else {
            // Not whitespace or comment
            break;
        }
    }

    pos
}

#[inline]
pub(super) fn is_hex_digit(ch: u8) -> bool {
    (CHARACTER_TYPE[ch as usize] & 3) == 2 || (CHARACTER_TYPE[ch as usize] & 3) == 3
}

#[inline]
pub(super) fn is_json_ok(ch: u8) -> bool {
    (CHARACTER_TYPE_OK[ch as usize] & 4) != 0
}
