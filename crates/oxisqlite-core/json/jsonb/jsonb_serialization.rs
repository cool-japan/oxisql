//! # `Jsonb` - serialization Methods
//!
//! This module contains method implementations for `Jsonb`.
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::json::error::{Error as PError, Result as PResult};
use crate::{bail_parse_error, LimboError, Result};
use std::{borrow::Cow, fmt::Write, str::from_utf8};

use super::constants::{INFINITY_CHAR_COUNT, MAX_JSON_DEPTH};
use super::functions::{is_hex_digit, is_json_ok, skip_whitespace};
use super::types::{ElementType, JsonIndentation, JsonbHeader};

use super::jsonb_type::Jsonb;

impl Jsonb {
    pub fn new(capacity: usize, data: Option<&[u8]>) -> Self {
        if let Some(data) = data {
            return Self {
                data: data.to_vec(),
            };
        }
        Self {
            data: Vec::with_capacity(capacity),
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn make_empty_array(size: usize) -> Self {
        let mut jsonb = Self {
            data: Vec::with_capacity(size),
        };
        jsonb
            .write_element_header(0, ElementType::ARRAY, 0, false)
            .unwrap();
        jsonb
    }

    pub fn make_empty_obj(size: usize) -> Self {
        let mut jsonb = Self {
            data: Vec::with_capacity(size),
        };
        jsonb
            .write_element_header(0, ElementType::OBJECT, 0, false)
            .unwrap();
        jsonb
    }

    pub fn append_to_array_unsafe(&mut self, data: &[u8]) {
        self.data.extend_from_slice(data);
    }

    pub fn append_jsonb_to_end(&mut self, mut data: Vec<u8>) {
        self.data.append(&mut data);
    }

    pub fn finalize_unsafe(&mut self, element_type: ElementType) -> Result<()> {
        self.write_element_header(0, element_type, self.len() - 1, false)?;
        Ok(())
    }

    pub(super) fn read_header(&self, cursor: usize) -> Result<(JsonbHeader, usize)> {
        let (header, offset) = JsonbHeader::from_slice(cursor, &self.data)?;

        Ok((header, offset))
    }

    pub fn is_valid(&self) -> Result<ElementType> {
        match self.read_header(0) {
            Ok((header, offset)) => {
                if self.data.get(offset..offset + header.1).is_some() {
                    Ok(header.0)
                } else {
                    bail_parse_error!("malformed JSON")
                }
            }
            Err(_) => bail_parse_error!("malformed JSON"),
        }
    }

    pub fn to_string(&self) -> Result<String> {
        let mut result = String::with_capacity(self.data.len() * 2);

        self.write_to_string(&mut result, JsonIndentation::None)?;

        Ok(result)
    }

    pub fn to_string_pretty(&self, indentation: Option<&str>) -> Result<String> {
        let mut result = String::with_capacity(self.data.len() * 2);
        let ind = if let Some(ind) = indentation {
            JsonIndentation::Indentation(Cow::Borrowed(ind))
        } else {
            JsonIndentation::Indentation(Cow::Borrowed("    "))
        };
        self.write_to_string(&mut result, ind)?;

        Ok(result)
    }

    fn write_to_string(&self, string: &mut String, indentation: JsonIndentation) -> Result<()> {
        let cursor = 0;
        let ind = indentation;
        let _ = self.serialize_value(string, cursor, 0, &ind);
        Ok(())
    }

    fn serialize_value(
        &self,
        string: &mut String,
        cursor: usize,
        depth: usize,
        delimiter: &JsonIndentation,
    ) -> Result<usize> {
        let (header, skip_header) = self.read_header(cursor)?;

        let cursor = cursor + skip_header;
        let current_cursor = match header {
            JsonbHeader(ElementType::OBJECT, len) => {
                self.serialize_object(string, cursor, len, depth, delimiter)?
            }
            JsonbHeader(ElementType::ARRAY, len) => {
                self.serialize_array(string, cursor, len, depth, delimiter)?
            }
            JsonbHeader(ElementType::TEXT, len)
            | JsonbHeader(ElementType::TEXTRAW, len)
            | JsonbHeader(ElementType::TEXTJ, len)
            | JsonbHeader(ElementType::TEXT5, len) => {
                self.serialize_string(string, cursor, len, &header.0, true)?
            }
            JsonbHeader(ElementType::INT, len)
            | JsonbHeader(ElementType::INT5, len)
            | JsonbHeader(ElementType::FLOAT, len)
            | JsonbHeader(ElementType::FLOAT5, len) => {
                self.serialize_number(string, cursor, len, &header.0)?
            }

            JsonbHeader(ElementType::TRUE, _) => self.serialize_boolean(string, cursor, true),
            JsonbHeader(ElementType::FALSE, _) => self.serialize_boolean(string, cursor, false),
            JsonbHeader(ElementType::NULL, _) => self.serialize_null(string, cursor),
            JsonbHeader(_, _) => {
                unreachable!();
            }
        };
        Ok(current_cursor)
    }

    fn serialize_object(
        &self,
        string: &mut String,
        cursor: usize,
        len: usize,
        mut depth: usize,
        indent: &JsonIndentation,
    ) -> Result<usize> {
        let end_cursor = cursor + len;
        let mut current_cursor = cursor;
        depth += 1;
        string.push('{');
        if indent.is_pretty() {
            string.push('\n');
        };
        while current_cursor < end_cursor {
            let (key_header, key_header_offset) = self.read_header(current_cursor)?;
            current_cursor += key_header_offset;
            let JsonbHeader(element_type, len) = key_header;
            if let JsonIndentation::Indentation(value) = indent {
                for _ in 0..depth {
                    string.push_str(value);
                }
            };
            match element_type {
                ElementType::TEXT
                | ElementType::TEXTRAW
                | ElementType::TEXTJ
                | ElementType::TEXT5 => {
                    current_cursor =
                        self.serialize_string(string, current_cursor, len, &element_type, true)?;
                }
                _ => bail_parse_error!("malformed JSON"),
            }

            string.push(':');
            if indent.is_pretty() {
                string.push(' ');
            }
            current_cursor = self.serialize_value(string, current_cursor, depth, indent)?;
            if current_cursor < end_cursor {
                string.push(',');
            }

            if indent.is_pretty() {
                string.push('\n');
            };
        }
        if let JsonIndentation::Indentation(value) = indent {
            for _ in 0..depth - 1 {
                string.push_str(value);
            }
        };
        string.push('}');

        Ok(current_cursor)
    }

    fn serialize_array(
        &self,
        string: &mut String,
        cursor: usize,
        len: usize,
        mut depth: usize,
        indent: &JsonIndentation,
    ) -> Result<usize> {
        let end_cursor = cursor + len;
        let mut current_cursor = cursor;
        depth += 1;
        string.push('[');
        if indent.is_pretty() {
            string.push('\n');
        };
        while current_cursor < end_cursor {
            if let JsonIndentation::Indentation(value) = indent {
                for _ in 0..depth {
                    string.push_str(value);
                }
            };
            current_cursor = self.serialize_value(string, current_cursor, depth, indent)?;
            if current_cursor < end_cursor {
                string.push(',');
            }
            if indent.is_pretty() {
                string.push('\n');
            };
        }
        if let JsonIndentation::Indentation(value) = indent {
            for _ in 0..depth - 1 {
                string.push_str(value);
            }
        };
        string.push(']');

        Ok(current_cursor)
    }

    fn serialize_string(
        &self,
        string: &mut String,
        cursor: usize,
        len: usize,
        kind: &ElementType,
        quote: bool,
    ) -> Result<usize> {
        let word_slice = &self.data[cursor..cursor + len];
        if quote {
            string.push('"');
        }

        match kind {
            // Can be serialized as is. Do not need escaping
            ElementType::TEXT => {
                let word = from_utf8(word_slice).map_err(|_| {
                    LimboError::ParseError("Failed to serialize string!".to_string())
                })?;
                string.push_str(word);
            }

            // Contain standard json escapes
            ElementType::TEXTJ => {
                let word = from_utf8(word_slice).map_err(|_| {
                    LimboError::ParseError("Failed to serialize string!".to_string())
                })?;
                string.push_str(word);
            }

            // We have to escape some JSON5 escape sequences
            ElementType::TEXT5 => {
                let mut i = 0;
                while i < word_slice.len() {
                    let ch = word_slice[i];

                    // Handle normal characters that don't need escaping
                    if is_json_ok(ch) || ch == b'\'' {
                        string.push(ch as char);
                        i += 1;
                        continue;
                    }

                    // Handle special cases
                    match ch {
                        // Double quotes need escaping
                        b'"' => {
                            string.push_str("\\\"");
                            i += 1;
                        }

                        // Control characters (0x00-0x1F)
                        ch if ch <= 0x1F => {
                            match ch {
                                // \b
                                0x08 => string.push_str("\\b"),
                                b'\t' => string.push_str("\\t"),
                                b'\n' => string.push_str("\\n"),
                                // \f
                                0x0C => string.push_str("\\f"),
                                b'\r' => string.push_str("\\r"),
                                _ => {
                                    // Format as \u00XX
                                    let hex = format!("\\u{:04x}", ch);
                                    string.push_str(&hex);
                                }
                            }
                            i += 1;
                        }

                        // Handle escape sequences
                        b'\\' if i + 1 < word_slice.len() => {
                            let next_ch = word_slice[i + 1];
                            match next_ch {
                                // Single quote
                                b'\'' => {
                                    string.push('\'');
                                    i += 2;
                                }

                                // Vertical tab
                                b'v' => {
                                    string.push_str("\\u0009");
                                    i += 2;
                                }

                                // Hex escapes like \x27
                                b'x' if i + 3 < word_slice.len() => {
                                    string.push_str("\\u00");
                                    string.push(word_slice[i + 2] as char);
                                    string.push(word_slice[i + 3] as char);
                                    i += 4;
                                }

                                // Null character
                                b'0' => {
                                    string.push_str("\\u0000");
                                    i += 2;
                                }

                                // CR line continuation
                                b'\r' => {
                                    if i + 2 < word_slice.len() && word_slice[i + 2] == b'\n' {
                                        i += 3; // Skip CRLF
                                    } else {
                                        i += 2; // Skip CR
                                    }
                                }

                                // LF line continuation
                                b'\n' => {
                                    i += 2;
                                }

                                // Unicode line separators (U+2028 and U+2029)
                                0xe2 if i + 3 < word_slice.len()
                                    && word_slice[i + 2] == 0x80
                                    && (word_slice[i + 3] == 0xa8 || word_slice[i + 3] == 0xa9) =>
                                {
                                    i += 4;
                                }

                                // All other escapes pass through
                                _ => {
                                    string.push('\\');
                                    string.push(next_ch as char);
                                    i += 2;
                                }
                            }
                        }

                        // Default case - just push the character
                        _ => {
                            string.push(ch as char);
                            i += 1;
                        }
                    }
                }
            }

            ElementType::TEXTRAW => {
                let word = from_utf8(word_slice).map_err(|_| {
                    LimboError::ParseError("Failed to serialize string!".to_string())
                })?;

                for ch in word.chars() {
                    match ch {
                        '"' => string.push_str("\\\""),
                        '\\' => string.push_str("\\\\"),
                        '\x08' => string.push_str("\\b"),
                        '\x0C' => string.push_str("\\f"),
                        '\n' => string.push_str("\\n"),
                        '\r' => string.push_str("\\r"),
                        '\t' => string.push_str("\\t"),
                        c if c <= '\u{001F}' => {
                            string.push_str(&format!("\\u{:04x}", c as u32));
                        }
                        _ => string.push(ch),
                    }
                }
            }

            _ => {
                unreachable!()
            }
        }
        if quote {
            string.push('"');
        }

        Ok(cursor + len)
    }

    fn serialize_number(
        &self,
        string: &mut String,
        cursor: usize,
        len: usize,
        kind: &ElementType,
    ) -> Result<usize> {
        let current_cursor = cursor + len;
        let num_slice = from_utf8(&self.data[cursor..current_cursor])
            .map_err(|_| LimboError::ParseError("Failed to parse integer".to_string()))?;

        match kind {
            ElementType::INT | ElementType::FLOAT => {
                string.push_str(num_slice);
            }
            ElementType::INT5 => {
                self.serialize_int5(string, num_slice)?;
            }
            ElementType::FLOAT5 => {
                self.serialize_float5(string, num_slice)?;
            }
            _ => unreachable!(),
        }
        Ok(current_cursor)
    }

    fn serialize_int5(&self, string: &mut String, hex_str: &str) -> Result<()> {
        // Check if number is hex
        if hex_str.len() > 2
            && (hex_str[..2].eq_ignore_ascii_case("0x")
                || (hex_str.starts_with("-") || hex_str.starts_with("+"))
                    && hex_str[1..3].eq_ignore_ascii_case("0x"))
        {
            let (sign, hex_part) = if hex_str.starts_with("-0x") || hex_str.starts_with("-0X") {
                ("-", &hex_str[3..])
            } else if hex_str.starts_with("+0x") || hex_str.starts_with("+0X") {
                ("", &hex_str[3..])
            } else {
                ("", &hex_str[2..])
            };

            // Add sign
            string.push_str(sign);

            let mut value = 0u64;

            for ch in hex_part.chars() {
                if !ch.is_ascii_hexdigit() {
                    bail_parse_error!("Failed to parse hex digit: {}", hex_part);
                }

                if (value >> 60) != 0 {
                    string.push_str("9.0e999");
                    return Ok(());
                }

                value = value * 16 + ch.to_digit(16).unwrap_or(0) as u64;
            }
            write!(string, "{}", value)
                .map_err(|_| LimboError::ParseError("Error writing string to json!".to_string()))?;
        } else {
            string.push_str(hex_str);
        }

        Ok(())
    }

    fn serialize_float5(&self, string: &mut String, float_str: &str) -> Result<()> {
        if float_str.len() < 2 {
            bail_parse_error!("Integer is less then 2 chars: {}", float_str);
        }
        match float_str {
            "9e999" | "-9e999" => {
                string.push_str(float_str);
            }
            val if val.starts_with("-.") => {
                string.push_str("-0.");
                string.push_str(&val[2..]);
            }
            val if val.starts_with("+.") => {
                string.push_str("0.");
                string.push_str(&val[2..]);
            }
            val if val.starts_with(".") => {
                string.push_str("0.");
                string.push_str(&val[1..]);
            }
            val if val
                .chars()
                .next()
                .map_or(false, |c| c.is_ascii_alphanumeric() || c == '+' || c == '-') =>
            {
                string.push_str(val);
                string.push('0');
            }
            _ => bail_parse_error!("Unable to serialize float5: {}", float_str),
        }

        Ok(())
    }

    fn serialize_boolean(&self, string: &mut String, cursor: usize, val: bool) -> usize {
        if val {
            string.push_str("true");
        } else {
            string.push_str("false");
        }

        cursor
    }

    fn serialize_null(&self, string: &mut String, cursor: usize) -> usize {
        string.push_str("null");
        cursor
    }

    pub(super) fn deserialize_value(
        &mut self,
        input: &[u8],
        mut pos: usize,
        depth: usize,
    ) -> PResult<usize> {
        if depth > MAX_JSON_DEPTH {
            return Err(PError::Message {
                msg: "Too deep".to_string(),
                location: Some(pos),
            });
        }

        pos = skip_whitespace(input, pos);
        if pos >= input.len() {
            return Err(PError::Message {
                msg: "Unexpected end of input".to_string(),
                location: Some(pos),
            });
        }

        match input[pos] {
            b'{' => {
                pos += 1; // consume '{'
                pos = self.deserialize_obj(input, pos, depth + 1)?;
            }
            b'[' => {
                pos += 1; // consume '['
                pos = self.deserialize_array(input, pos, depth + 1)?;
            }
            b't' => {
                pos = self.deserialize_true(input, pos)?;
            }
            b'f' => {
                pos = self.deserialize_false(input, pos)?;
            }
            b'n' | b'N' => {
                pos = self.deserialize_null_or_nan(input, pos)?;
            }
            b'"' | b'\'' => {
                pos = self.deserialize_string(input, pos)?;
            }
            c if c.is_ascii_digit()
                || c == b'-'
                || c == b'+'
                || c == b'.'
                || c.to_ascii_lowercase() == b'i' =>
            {
                pos = self.deserialize_number(input, pos)?;
            }
            _ => {
                return Err(PError::Message {
                    msg: "Unexpected character".to_string(),
                    location: Some(pos),
                })
            }
        }

        Ok(pos)
    }

    pub(super) fn deserialize_obj(
        &mut self,
        input: &[u8],
        mut pos: usize,
        depth: usize,
    ) -> PResult<usize> {
        if depth > MAX_JSON_DEPTH {
            return Err(PError::Message {
                msg: "Too deep".to_string(),
                location: Some(pos),
            });
        }
        if self.data.capacity() - self.data.len() < 50 {
            self.data.reserve(self.data.capacity());
        }
        if pos >= input.len() {
            return Err(PError::Message {
                msg: "Unexpected end of input".to_string(),
                location: Some(pos),
            });
        }

        let header_pos = self.len();
        self.write_element_header(header_pos, ElementType::OBJECT, 0, false)
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;
        let obj_start = self.len();
        let mut first = true;

        loop {
            pos = skip_whitespace(input, pos);
            if pos >= input.len() {
                return Err(PError::Message {
                    msg: "Unexpected end of input".to_string(),
                    location: Some(pos),
                });
            }

            match input[pos] {
                b'}' => {
                    pos += 1; // consume '}'
                    if first {
                        return Ok(pos);
                    } else {
                        let obj_size = self.len() - obj_start;
                        self.write_element_header(header_pos, ElementType::OBJECT, obj_size, false)
                            .map_err(|_| PError::Message {
                                msg: "Failed to write header".to_string(),
                                location: Some(pos),
                            })?;
                        return Ok(pos);
                    }
                }
                b',' if !first => {
                    pos += 1; // consume ','
                    pos = skip_whitespace(input, pos);
                    if input[pos] == b',' || input[pos] == b'{' {
                        return Err(PError::Message {
                            msg: "Two commas in a row".to_string(),
                            location: Some(pos),
                        });
                    }
                }
                _ => {
                    // Parse key (must be string)
                    pos = self.deserialize_string(input, pos)?;

                    pos = skip_whitespace(input, pos);
                    if pos >= input.len() || input[pos] != b':' {
                        return Err(PError::Message {
                            msg: "Expected : after object key".to_string(),
                            location: Some(pos),
                        });
                    }
                    pos += 1; // consume ':'

                    pos = skip_whitespace(input, pos);

                    // Parse value - can be any JSON value including another object
                    pos = self.deserialize_value(input, pos, depth + 1)?;
                    pos = skip_whitespace(input, pos);
                    if pos < input.len() && !matches!(input[pos], b',' | b'}') {
                        return Err(PError::Message {
                            msg: "Should be , or }}".to_string(),
                            location: Some(pos),
                        });
                    }
                    first = false;
                }
            }
        }
    }

    pub(super) fn deserialize_array(
        &mut self,
        input: &[u8],
        mut pos: usize,
        depth: usize,
    ) -> PResult<usize> {
        if depth > MAX_JSON_DEPTH {
            return Err(PError::Message {
                msg: "Too deep".to_string(),
                location: Some(pos),
            });
        }

        let header_pos = self.len();
        self.write_element_header(header_pos, ElementType::ARRAY, 0, false)
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;
        let arr_start = self.len();
        let mut first = true;

        loop {
            pos = skip_whitespace(input, pos);
            if pos >= input.len() {
                return Err(PError::Message {
                    msg: "Unexpected end of input".to_string(),
                    location: Some(pos),
                });
            }

            match input[pos] {
                b']' => {
                    pos += 1; // consume ']'
                    if first {
                        return Ok(pos);
                    } else {
                        let arr_len = self.len() - arr_start;
                        self.write_element_header(header_pos, ElementType::ARRAY, arr_len, false)
                            .map_err(|_| PError::Message {
                                msg: "Failed to write header".to_string(),
                                location: Some(pos),
                            })?;
                        return Ok(pos);
                    }
                }
                b',' if !first => {
                    pos += 1; // consume ','
                    pos = skip_whitespace(input, pos);
                    if input[pos] == b',' {
                        return Err(PError::Message {
                            msg: "Two commas in a row".to_string(),
                            location: Some(pos),
                        });
                    }
                }
                _ => {
                    pos = skip_whitespace(input, pos);

                    // Parse array element
                    pos = self.deserialize_value(input, pos, depth + 1)?;

                    first = false;
                }
            }
        }
    }

    pub(super) fn deserialize_string(&mut self, input: &[u8], mut pos: usize) -> PResult<usize> {
        if pos >= input.len() {
            return Err(PError::Message {
                msg: "Unexpected end of input".to_string(),
                location: Some(pos),
            });
        }

        let string_start = self.len();
        let quote = input[pos];
        pos += 1; // consume quote

        let quoted = quote == b'"' || quote == b'\'';
        let mut len = 0;

        if quoted {
            // Try to find the closing quote and check for simple string
            let mut end_pos = pos;
            let is_simple = true;

            while end_pos < input.len() {
                let c = input[end_pos];
                if c == quote {
                    // Found end of string - check if it's simple
                    if is_simple {
                        let len = end_pos - pos;
                        let header_pos = self.data.len();

                        // Write header and content
                        if len <= 11 {
                            self.data
                                .push((ElementType::TEXT as u8) | ((len as u8) << 4));
                        } else {
                            self.write_element_header(header_pos, ElementType::TEXT, len, false)
                                .map_err(|_| PError::Message {
                                    msg: "Failed to write header".to_string(),
                                    location: Some(pos),
                                })?;
                        }

                        self.data.extend_from_slice(&input[pos..end_pos]);
                        return Ok(end_pos + 1); // Skip past closing quote
                    }
                    break;
                } else if c == b'\\' || c < 32 {
                    // Not a simple string
                    break;
                }
                end_pos += 1;
            }
        }

        // Write placeholder header to be updated later
        self.write_element_header(string_start, ElementType::TEXT, 0, false)
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;

        if pos >= input.len() {
            return Err(PError::Message {
                msg: "Unexpected end of input".to_string(),
                location: Some(pos),
            });
        }

        let mut element_type = ElementType::TEXT;

        // Special case for unquoted JSON5 keys (identifiers)
        if !quoted {
            self.data.push(quote);
            len += 1;

            if pos < input.len() && input[pos] == b':' {
                self.write_element_header(string_start, element_type, len, false)
                    .map_err(|_| PError::Message {
                        msg: "Failed to write header".to_string(),
                        location: Some(pos),
                    })?;
                return Ok(pos);
            }
        }

        let mut escape_buffer = [0u8; 6]; // Buffer for escape sequences

        while pos < input.len() {
            let c = input[pos];
            pos += 1;

            if quoted && c == quote {
                break; // End of string
            } else if !quoted && (c == b'"' || c == b'\'') {
                return Err(PError::Message {
                    msg: "Unexpected input".to_string(),
                    location: Some(pos),
                });
            } else if c == b'\\' {
                // Handle escape sequences
                if pos >= input.len() {
                    return Err(PError::Message {
                        msg: "Unexpected end of input".to_string(),
                        location: Some(pos),
                    });
                }

                let esc = input[pos];
                pos += 1;

                match esc {
                    b'b' => {
                        self.data.extend_from_slice(b"\\b");
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b'f' => {
                        self.data.extend_from_slice(b"\\f");
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b'n' => {
                        self.data.extend_from_slice(b"\\n");
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b'r' => {
                        self.data.extend_from_slice(b"\\r");
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b't' => {
                        self.data.extend_from_slice(b"\\t");
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b'\\' | b'"' | b'/' => {
                        self.data.push(b'\\');
                        self.data.push(esc);
                        len += 2;
                        element_type = ElementType::TEXTJ;
                    }
                    b'u' => {
                        // Unicode escape sequence
                        if pos + 4 > input.len() {
                            return Err(PError::Message {
                                msg: "Incomplete unicode escape sequence".to_string(),
                                location: Some(pos),
                            });
                        }

                        escape_buffer[0] = b'\\';
                        escape_buffer[1] = b'u';

                        for i in 0..4 {
                            let h = input[pos + i];
                            if !is_hex_digit(h) {
                                return Err(PError::Message {
                                    msg: "Invalid unicode escape sequence".to_string(),
                                    location: Some(pos),
                                });
                            }
                            escape_buffer[2 + i] = h;
                        }

                        self.data.extend_from_slice(&escape_buffer[0..6]);
                        len += 6;
                        pos += 4;
                        element_type = ElementType::TEXTJ;
                    }
                    // JSON5 extensions
                    b'\n' => {
                        self.data.extend_from_slice(b"\\\n");
                        len += 2;
                        element_type = ElementType::TEXT5;
                    }
                    b'\'' => {
                        self.data.extend_from_slice(b"\\\'");
                        len += 2;
                        element_type = ElementType::TEXT5;
                    }
                    b'0' => {
                        self.data.extend_from_slice(b"\\0");
                        len += 2;
                        element_type = ElementType::TEXT5;
                    }
                    b'v' => {
                        self.data.extend_from_slice(b"\\v");
                        len += 2;
                        element_type = ElementType::TEXT5;
                    }
                    b'x' => {
                        // Hex escape sequence (JSON5)
                        if pos + 2 > input.len() {
                            return Err(PError::Message {
                                msg: "Incopmlete hex escape sequence".to_string(),
                                location: Some(pos),
                            });
                        }

                        escape_buffer[0] = b'\\';
                        escape_buffer[1] = b'x';

                        for i in 0..2 {
                            let h = input[pos + i];
                            if !is_hex_digit(h) {
                                return Err(PError::Message {
                                    msg: "Invalid hex escape sequence".to_string(),
                                    location: Some(pos),
                                });
                            }
                            escape_buffer[2 + i] = h;
                        }

                        self.data.extend_from_slice(&escape_buffer[0..4]);
                        len += 4;
                        pos += 2;
                        element_type = ElementType::TEXT5;
                    }

                    _ => {
                        return Err(PError::Message {
                            msg: "Invalid escape sequence".to_string(),
                            location: Some(pos),
                        });
                    }
                }
            } else if !quoted && (c == b':' || c.is_ascii_whitespace()) {
                // End of unquoted identifier
                pos -= 1; // Put back the terminating character
                break;
            } else if c <= 0x1F {
                // Control character
                element_type = ElementType::TEXT5;
                self.data.push(c);
                len += 1;
            } else {
                // Normal character
                self.data.push(c);
                len += 1;
            }
        }

        // Write final header with correct type and size
        self.write_element_header(string_start, element_type, len, false)
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;

        Ok(pos)
    }

    pub(super) fn deserialize_number(&mut self, input: &[u8], mut pos: usize) -> PResult<usize> {
        let num_start = self.len();
        let mut len = 0;
        let mut is_float = false;
        let mut is_json5 = false;

        // Write placeholder header
        self.write_element_header(num_start, ElementType::INT, 0, false)
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;

        // Handle sign
        if pos < input.len() && (input[pos] == b'-' || input[pos] == b'+') {
            if input[pos] == b'+' {
                is_json5 = true;
                pos += 1;
            } else {
                self.data.push(input[pos]);
                pos += 1;
                len += 1;
            }
        }

        // Handle JSON5 float starting with dot
        if pos < input.len() && input[pos] == b'.' {
            is_json5 = true;
            is_float = true;
        }

        // Check for hex (JSON5)
        if pos < input.len() && input[pos] == b'0' && pos + 1 < input.len() {
            self.data.push(input[pos]);
            pos += 1;
            len += 1;

            if pos < input.len() && (input[pos] == b'x' || input[pos] == b'X') {
                // Hexadecimal number
                self.data.push(input[pos]);
                pos += 1;
                len += 1;

                let mut has_digit = false;
                while pos < input.len() && is_hex_digit(input[pos]) {
                    self.data.push(input[pos]);
                    pos += 1;
                    len += 1;
                    has_digit = true;
                }

                if !has_digit {
                    return Err(PError::Message {
                        msg: "Invalid hex digit".to_string(),
                        location: Some(pos),
                    });
                }

                self.write_element_header(num_start, ElementType::INT5, len, false)
                    .map_err(|_| PError::Message {
                        msg: "Unexpected input after json".to_string(),
                        location: Some(pos),
                    })?;
                return Ok(pos);
            } else if pos < input.len() && input[pos].is_ascii_digit() {
                // Leading zero followed by digit is not allowed in standard JSON
                return Err(PError::Message {
                    msg: "Leading zero is not allowed".to_string(),
                    location: Some(pos),
                });
            }
        }

        // Check for Infinity
        if pos < input.len() && (input[pos] == b'I' || input[pos] == b'i') {
            // Try to match "Infinity"
            let infinity = b"infinity";
            let mut i = 0;

            while i < infinity.len() && pos + i < input.len() {
                if input[pos + i].to_ascii_lowercase() != infinity[i] {
                    return Err(PError::Message {
                        msg: "Invalid number".to_string(),
                        location: Some(pos),
                    });
                }
                i += 1;
            }

            if i < infinity.len() {
                return Err(PError::Message {
                    msg: "incomplete infinity".to_string(),
                    location: Some(pos),
                });
            }

            pos += infinity.len();

            // Write Infinity as 9e999
            self.data.extend_from_slice(b"9e999");
            self.write_element_header(
                num_start,
                ElementType::FLOAT5,
                len + INFINITY_CHAR_COUNT as usize,
                false,
            )
            .map_err(|_| PError::Message {
                msg: "Failed to write header".to_string(),
                location: Some(pos),
            })?;

            return Ok(pos);
        }

        // Regular number parsing
        while pos < input.len() {
            match input[pos] {
                b'0'..=b'9' => {
                    self.data.push(input[pos]);
                    pos += 1;
                    len += 1;
                }
                b'.' => {
                    is_float = true;
                    self.data.push(input[pos]);
                    pos += 1;
                    len += 1;

                    // Check for trailing dot
                    if pos >= input.len() || !input[pos].is_ascii_digit() {
                        is_json5 = true;
                    }
                }
                b'e' | b'E' => {
                    is_float = true;
                    self.data.push(input[pos]);
                    pos += 1;
                    len += 1;

                    // Optional sign after exponent
                    if pos < input.len() && (input[pos] == b'+' || input[pos] == b'-') {
                        self.data.push(input[pos]);
                        pos += 1;
                        len += 1;
                    }
                }
                _ => break,
            }
        }

        // No digits found
        if len == 0 && (!is_json5 || !is_float) {
            return Err(PError::Message {
                msg: "Not a digigt".to_string(),
                location: Some(pos),
            });
        }

        // Determine the appropriate element type
        let element_type = if is_float {
            if is_json5 {
                ElementType::FLOAT5
            } else {
                ElementType::FLOAT
            }
        } else if is_json5 {
            ElementType::INT5
        } else {
            ElementType::INT
        };

        self.write_element_header(num_start, element_type, len, false)
            .map_err(|_| PError::Message {
                msg: "Unexpected input after json".to_string(),
                location: Some(pos),
            })?;

        Ok(pos)
    }

    pub(super) fn deserialize_true(&mut self, input: &[u8], mut pos: usize) -> PResult<usize> {
        let true_lit = b"true";
        for i in 0..true_lit.len() {
            if pos + i >= input.len() || input[pos + i] != true_lit[i] {
                return Err(PError::Message {
                    msg: "Expected true".to_string(),
                    location: Some(pos),
                });
            }
        }

        pos += true_lit.len();
        self.data.push(ElementType::TRUE as u8);

        Ok(pos)
    }

    pub(super) fn deserialize_false(&mut self, input: &[u8], mut pos: usize) -> PResult<usize> {
        let false_lit = b"false";
        for i in 0..false_lit.len() {
            if pos + i >= input.len() || input[pos + i] != false_lit[i] {
                return Err(PError::Message {
                    msg: "Expected false".to_string(),
                    location: Some(pos),
                });
            }
        }

        pos += false_lit.len();
        self.data.push(ElementType::FALSE as u8);

        Ok(pos)
    }

    pub fn deserialize_null_or_nan(&mut self, input: &[u8], mut pos: usize) -> PResult<usize> {
        // First check if we have enough bytes remaining
        if pos + 3 >= input.len() {
            return Err(PError::Message {
                msg: "Unexpected end of input".to_string(),
                location: Some(pos),
            });
        }

        // Fast path for "null"
        if pos + 4 <= input.len()
            && input[pos] == b'n'
            && input[pos + 1] == b'u'
            && input[pos + 2] == b'l'
            && input[pos + 3] == b'l'
        {
            pos += 4;
            self.data.push(ElementType::NULL as u8);
            return Ok(pos);
        }

        // Fast path for "nan"
        if pos + 3 <= input.len()
            && (input[pos] == b'n' || input[pos] == b'N')
            && (input[pos + 1] == b'a' || input[pos + 1] == b'A')
            && (input[pos + 2] == b'n' || input[pos + 2] == b'N')
        {
            pos += 3;
            self.data.push(ElementType::NULL as u8);
            return Ok(pos);
        }

        return Err(PError::Message {
            msg: "Expected null or nan".to_string(),
            location: Some(pos),
        });
    }

    pub(super) fn write_element_header(
        &mut self,
        cursor: usize,
        element_type: ElementType,
        payload_size: usize,
        size_might_change: bool,
    ) -> Result<usize> {
        if payload_size <= 11 && !size_might_change {
            let header_byte = (element_type as u8) | ((payload_size as u8) << 4);
            if cursor == self.len() {
                self.data.push(header_byte);
            } else {
                self.data[cursor] = header_byte;
            }
            return Ok(1);
        }

        let header = JsonbHeader::new(element_type, payload_size).into_bytes();
        let header_bytes = header.as_bytes();
        let header_len = header_bytes.len();

        if cursor == self.len() {
            self.data.extend_from_slice(header_bytes);
            return Ok(header_len);
        }

        let old_len = if size_might_change {
            let (_, offset) = self.read_header(cursor)?;
            offset
        } else {
            1
        };

        let new_len = header_bytes.len();

        if new_len > old_len {
            self.data.splice(
                cursor + old_len..cursor + old_len,
                std::iter::repeat(0).take(new_len - old_len),
            );
        } else if new_len < old_len {
            self.data.drain(cursor + new_len..cursor + old_len);
        }

        for (i, &byte) in header_bytes.iter().enumerate() {
            self.data[cursor + i] = byte;
        }

        Ok(new_len)
    }
}
