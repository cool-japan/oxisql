//! Auto-generated test module (consolidated from inline `#[cfg(test)] mod` blocks)

use super::constants::{MAX_JSON_DEPTH, SIZE_MARKER_16BIT, SIZE_MARKER_32BIT, SIZE_MARKER_8BIT};
use super::*;

#[cfg(test)]
mod tests_2 {
    use super::*;

    #[test]
    fn test_null_serialization() {
        // Create JSONB with null value
        let mut jsonb = Jsonb::new(10, None);
        jsonb.data.push(ElementType::NULL as u8);

        // Test serialization
        let json_str = jsonb.to_string().unwrap();
        assert_eq!(json_str, "null");

        // Test round-trip
        let reparsed = Jsonb::from_str("null").unwrap();
        assert_eq!(reparsed.data[0] as u8, ElementType::NULL as u8);
    }

    #[test]
    fn test_boolean_serialization() {
        // True
        let mut jsonb_true = Jsonb::new(10, None);
        jsonb_true.data.push(ElementType::TRUE as u8);
        assert_eq!(jsonb_true.to_string().unwrap(), "true");

        // False
        let mut jsonb_false = Jsonb::new(10, None);
        jsonb_false.data.push(ElementType::FALSE as u8);
        assert_eq!(jsonb_false.to_string().unwrap(), "false");

        // Round-trip
        let true_parsed = Jsonb::from_str("true").unwrap();
        assert_eq!(true_parsed.data[0] as u8, ElementType::TRUE as u8);

        let false_parsed = Jsonb::from_str("false").unwrap();
        assert_eq!(false_parsed.data[0] as u8, ElementType::FALSE as u8);
    }

    #[test]
    fn test_integer_serialization() {
        // Standard integer
        let parsed = Jsonb::from_str("42").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "42");

        // Negative integer
        let parsed = Jsonb::from_str("-123").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "-123");

        // Zero
        let parsed = Jsonb::from_str("0").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "0");

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::INT));
    }

    #[test]
    fn test_json5_integer_serialization() {
        // Hexadecimal notation
        let parsed = Jsonb::from_str("0x1A").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "26"); // Should convert to decimal

        // Positive sign (JSON5)
        let parsed = Jsonb::from_str("+42").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "42");

        // Negative hexadecimal
        let parsed = Jsonb::from_str("-0xFF").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "-255");

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::INT5));
    }

    #[test]
    fn test_float_serialization() {
        // Standard float
        let parsed = Jsonb::from_str("3.14159").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "3.14159");

        // Negative float
        let parsed = Jsonb::from_str("-2.718").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "-2.718");

        // Scientific notation
        let parsed = Jsonb::from_str("6.022e23").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "6.022e23");

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::FLOAT));
    }

    #[test]
    fn test_json5_float_serialization() {
        // Leading decimal point
        let parsed = Jsonb::from_str(".123").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "0.123");

        // Trailing decimal point
        let parsed = Jsonb::from_str("42.").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "42.0");

        // Plus sign in exponent
        let parsed = Jsonb::from_str("1.5e+10").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "1.5e+10");

        // Infinity
        let parsed = Jsonb::from_str("Infinity").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "9e999");

        // Negative Infinity
        let parsed = Jsonb::from_str("-Infinity").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "-9e999");

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::FLOAT5));
    }

    #[test]
    fn test_string_serialization() {
        // Simple string
        let parsed = Jsonb::from_str(r#""hello world""#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""hello world""#);

        // String with escaped characters
        let parsed = Jsonb::from_str(r#""hello\nworld""#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""hello\nworld""#);

        // Unicode escape
        let parsed = Jsonb::from_str(r#""hello\u0020world""#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""hello\u0020world""#);

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::TEXTJ));
    }

    #[test]
    fn test_json5_string_serialization() {
        // Single quotes
        let parsed = Jsonb::from_str("'hello world'").unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""hello world""#);

        // Hex escape
        let parsed = Jsonb::from_str(r#"'\x41\x42\x43'"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""\u0041\u0042\u0043""#);

        // Multiline string with line continuation
        let parsed = Jsonb::from_str(
            r#""hello \
world""#,
        )
        .unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""hello world""#);

        // Escaped single quote
        let parsed = Jsonb::from_str(r#"'Don\'t worry'"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#""Don't worry""#);

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::TEXT5));
    }

    #[test]
    fn test_array_serialization() {
        // Empty array
        let parsed = Jsonb::from_str("[]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[]");

        // Simple array
        let parsed = Jsonb::from_str("[1,2,3]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[1,2,3]");

        // Nested array
        let parsed = Jsonb::from_str("[[1,2],[3,4]]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[[1,2],[3,4]]");

        // Mixed types array
        let parsed = Jsonb::from_str(r#"[1,"text",true,null,{"key":"value"}]"#).unwrap();
        assert_eq!(
            parsed.to_string().unwrap(),
            r#"[1,"text",true,null,{"key":"value"}]"#
        );

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::ARRAY));
    }

    #[test]
    fn test_json5_array_serialization() {
        // Trailing comma
        let parsed = Jsonb::from_str("[1,2,3,]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[1,2,3]");

        // Comments in array
        let parsed = Jsonb::from_str("[1,/* comment */2,3]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[1,2,3]");

        // Line comment in array
        let parsed = Jsonb::from_str("[1,// line comment\n2,3]").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[1,2,3]");
    }

    #[test]
    fn test_object_serialization() {
        // Empty object
        let parsed = Jsonb::from_str("{}").unwrap();
        assert_eq!(parsed.to_string().unwrap(), "{}");

        // Simple object
        let parsed = Jsonb::from_str(r#"{"key":"value"}"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"key":"value"}"#);

        // Multiple properties
        let parsed = Jsonb::from_str(r#"{"a":1,"b":2,"c":3}"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"a":1,"b":2,"c":3}"#);

        // Nested object
        let parsed = Jsonb::from_str(r#"{"outer":{"inner":"value"}}"#).unwrap();
        assert_eq!(
            parsed.to_string().unwrap(),
            r#"{"outer":{"inner":"value"}}"#
        );

        // Mixed values
        let parsed =
            Jsonb::from_str(r#"{"str":"text","num":42,"bool":true,"null":null,"arr":[1,2]}"#)
                .unwrap();
        assert_eq!(
            parsed.to_string().unwrap(),
            r#"{"str":"text","num":42,"bool":true,"null":null,"arr":[1,2]}"#
        );

        // Verify correct type
        let header = JsonbHeader::from_slice(0, &parsed.data).unwrap().0;
        assert!(matches!(header.0, ElementType::OBJECT));
    }

    #[test]
    fn test_json5_object_serialization() {
        // Unquoted keys
        let parsed = Jsonb::from_str("{key:\"value\"}").unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"key":"value"}"#);

        // Trailing comma
        let parsed = Jsonb::from_str(r#"{"a":1,"b":2,}"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"a":1,"b":2}"#);

        // Comments in object
        let parsed = Jsonb::from_str(r#"{"a":1,/*comment*/"b":2}"#).unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"a":1,"b":2}"#);

        // Single quotes for keys and values
        let parsed = Jsonb::from_str("{'a':'value'}").unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"a":"value"}"#);
    }

    #[test]
    fn test_complex_json() {
        let complex_json = r#"{
            "string": "Hello, world!",
            "number": 42,
            "float": 3.14159,
            "boolean": true,
            "null": null,
            "array": [1, 2, 3, "text", {"nested": "object"}],
            "object": {
                "key1": "value1",
                "key2": [4, 5, 6],
                "key3": {
                    "nested": true
                }
            }
        }"#;

        let parsed = Jsonb::from_str(complex_json).unwrap();
        // Round-trip test
        let reparsed = Jsonb::from_str(&parsed.to_string().unwrap()).unwrap();
        assert_eq!(parsed.to_string().unwrap(), reparsed.to_string().unwrap());
    }

    #[test]
    fn test_error_handling() {
        // Invalid JSON syntax
        assert!(Jsonb::from_str("{").is_err());
        assert!(Jsonb::from_str("[").is_err());
        assert!(Jsonb::from_str("}").is_err());
        assert!(Jsonb::from_str("]").is_err());

        assert!(Jsonb::from_str(r#"{"a":"55,"b":72}"#).is_err());

        assert!(Jsonb::from_str(r#"{"a":"55",,"b":72}"#).is_err());

        // Unclosed string
        assert!(Jsonb::from_str(r#"{"key":"value"#).is_err());

        // Invalid number format
        assert!(Jsonb::from_str("01234").is_err()); // Leading zero not allowed in JSON

        // Invalid escape sequence
        assert!(Jsonb::from_str(r#""\z""#).is_err());

        // Missing colon in object
        assert!(Jsonb::from_str(r#"{"key" "value"}"#).is_err());

        // Trailing characters
        assert!(Jsonb::from_str(r#"{"key":"value"} extra"#).is_err());
    }

    #[test]
    fn test_depth_limit() {
        // Create a JSON string that exceeds MAX_JSON_DEPTH
        let mut deep_json = String::from("[");
        for _ in 0..MAX_JSON_DEPTH + 1 {
            deep_json.push('[');
        }
        for _ in 0..MAX_JSON_DEPTH + 1 {
            deep_json.push(']');
        }
        deep_json.push(']');

        // Should fail due to exceeding depth limit
        assert!(Jsonb::from_str(&deep_json).is_err());
    }

    #[test]
    fn test_header_encoding() {
        // Small payload (fits in 4 bits)
        let header = JsonbHeader::new(ElementType::TEXT, 5);
        let bytes = header.into_bytes().as_bytes().to_vec();
        assert_eq!(bytes[0], (5 << 4) | (ElementType::TEXT as u8));

        // Medium payload (8-bit)
        let header = JsonbHeader::new(ElementType::TEXT, 200);
        let bytes = header.into_bytes().as_bytes().to_vec();
        assert_eq!(
            bytes[0],
            (SIZE_MARKER_8BIT << 4) | (ElementType::TEXT as u8)
        );
        assert_eq!(bytes[1], 200);

        // Large payload (16-bit)
        let header = JsonbHeader::new(ElementType::TEXT, 40000);
        let bytes = header.into_bytes().as_bytes().to_vec();
        assert_eq!(
            bytes[0],
            (SIZE_MARKER_16BIT << 4) | (ElementType::TEXT as u8)
        );
        assert_eq!(bytes[1], (40000 >> 8) as u8);
        assert_eq!(bytes[2], (40000 & 0xFF) as u8);

        // Extra large payload (32-bit)
        let header = JsonbHeader::new(ElementType::TEXT, 70000);
        let bytes = header.into_bytes().as_bytes().to_vec();
        assert_eq!(
            bytes[0],
            (SIZE_MARKER_32BIT << 4) | (ElementType::TEXT as u8)
        );
        assert_eq!(bytes[1], (70000 >> 24) as u8);
        assert_eq!(bytes[2], ((70000 >> 16) & 0xFF) as u8);
        assert_eq!(bytes[3], ((70000 >> 8) & 0xFF) as u8);
        assert_eq!(bytes[4], (70000 & 0xFF) as u8);
    }

    #[test]
    fn test_header_decoding() {
        // Create sample data with various headers
        let data = vec![
            (5 << 4) | (ElementType::TEXT as u8),
            (SIZE_MARKER_8BIT << 4) | (ElementType::ARRAY as u8),
            150,
            (SIZE_MARKER_16BIT << 4) | (ElementType::OBJECT as u8),
            0x98,
            0x68,
        ];

        // Parse and verify each header
        let (header1, offset1) = JsonbHeader::from_slice(0, &data).unwrap();
        assert_eq!(offset1, 1);
        assert_eq!(header1.0, ElementType::TEXT);
        assert_eq!(header1.1, 5);

        let (header2, offset2) = JsonbHeader::from_slice(1, &data).unwrap();
        assert_eq!(offset2, 2);
        assert_eq!(header2.0, ElementType::ARRAY);
        assert_eq!(header2.1, 150);

        let (header3, offset3) = JsonbHeader::from_slice(3, &data).unwrap();
        assert_eq!(offset3, 3);
        assert_eq!(header3.0, ElementType::OBJECT);
        assert_eq!(header3.1, 0x9868); // 39000
    }

    #[test]
    fn test_unicode_escapes() {
        // Basic unicode escape
        let parsed = Jsonb::from_str(r#""\u00A9""#).unwrap(); // Copyright symbol
        assert_eq!(parsed.to_string().unwrap(), r#""\u00A9""#);

        // Non-BMP character (surrogate pair)
        let parsed = Jsonb::from_str(r#""\uD83D\uDE00""#).unwrap(); // Smiley emoji
        assert_eq!(parsed.to_string().unwrap(), r#""\uD83D\uDE00""#);
    }

    #[test]
    fn test_json5_comments() {
        // Line comments
        let parsed = Jsonb::from_str(
            r#"{
            // This is a line comment
            "key": "value"
        }"#,
        )
        .unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"key":"value"}"#);

        // Block comments
        let parsed = Jsonb::from_str(
            r#"{
            /* This is a
               block comment */
            "key": "value"
        }"#,
        )
        .unwrap();
        assert_eq!(parsed.to_string().unwrap(), r#"{"key":"value"}"#);

        // Comments inside array
        let parsed = Jsonb::from_str(
            r#"[1, // Comment
                                       2, /* Another comment */ 3]"#,
        )
        .unwrap();
        assert_eq!(parsed.to_string().unwrap(), "[1,2,3]");
    }

    #[test]
    fn test_whitespace_handling() {
        // Various whitespace patterns
        let json_with_whitespace = r#"
        {
            "key1"    :    "value1"   ,
             "key2": [   1,    2,    3   ]  ,
            "key3":   {
                "nested"   :   true
            }
        }
        "#;

        let parsed = Jsonb::from_str(json_with_whitespace).unwrap();
        assert_eq!(
            parsed.to_string().unwrap(),
            r#"{"key1":"value1","key2":[1,2,3],"key3":{"nested":true}}"#
        );
    }

    #[test]
    fn test_binary_roundtrip() {
        // Test that binary data can be round-tripped through the JSONB format
        let original = r#"{"test":"value","array":[1,2,3]}"#;
        let parsed = Jsonb::from_str(original).unwrap();
        let binary_data = parsed.data.clone();

        // Create a new Jsonb from the binary data
        let from_binary = Jsonb::new(0, Some(&binary_data));
        assert_eq!(from_binary.to_string().unwrap(), original);
    }

    #[test]
    fn test_large_json() {
        // Generate a large JSON with many elements
        let mut large_array = String::from("[");
        for i in 0..1000 {
            large_array.push_str(&format!("{}", i));
            if i < 999 {
                large_array.push(',');
            }
        }
        large_array.push(']');

        let parsed = Jsonb::from_str(&large_array).unwrap();
        assert!(parsed.to_string().unwrap().starts_with("[0,1,2,"));
        assert!(parsed.to_string().unwrap().ends_with("998,999]"));
    }

    #[test]
    fn test_jsonb_is_valid() {
        // Valid JSONB
        let jsonb = Jsonb::from_str(r#"{"test":"value"}"#).unwrap();
        assert!(jsonb.is_valid().is_ok());

        // Invalid JSONB (manually corrupted)
        let mut invalid = jsonb.data.clone();
        if !invalid.is_empty() {
            invalid[0] = 0xFF; // Invalid element type
            let jsonb = Jsonb::new(0, Some(&invalid));
            assert!(jsonb.is_valid().is_err());
        }
    }

    #[test]
    fn test_special_characters_in_strings() {
        // Test handling of various special characters
        let json = r#"{
            "escaped_quotes": "He said \"Hello\"",
            "backslashes": "C:\\Windows\\System32",
            "control_chars": "\b\f\n\r\t",
            "unicode": "\u00A9 2023"
        }"#;

        let parsed = Jsonb::from_str(json).unwrap();
        let result = parsed.to_string().unwrap();

        assert!(result.contains(r#""escaped_quotes":"He said \"Hello\"""#));
        assert!(result.contains(r#""backslashes":"C:\\Windows\\System32""#));
        assert!(result.contains(r#""control_chars":"\b\f\n\r\t""#));
        assert!(result.contains(r#""unicode":"\u00A9 2023""#));
    }
}
#[cfg(test)]
mod path_operations_tests {
    use super::*;
    use crate::json::path::{JsonPath, PathElement};
    use std::borrow::Cow;

    // Helper function to create a simple JsonPath
    fn create_path(elements: Vec<PathElement>) -> JsonPath {
        JsonPath { elements }
    }

    #[test]
    fn test_navigate_root_path() {
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a path to the root
        let path = create_path(vec![PathElement::Root()]);

        // Navigate to the path
        let result = jsonb.navigate_path(&path, PathOperationMode::ReplaceExisting);

        // Verify navigation succeeds
        assert!(result.is_ok());
        let stack = result.unwrap();
        assert_eq!(stack.len(), 1);
        assert_eq!(stack[0].field_value_index, 0);
        assert_eq!(stack[0].field_key_index, JsonLocationKind::DocumentRoot);
    }

    #[test]
    fn test_navigate_object_property() {
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a path to the "name" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("name"), false),
        ]);

        // Navigate to the path
        let result = jsonb.navigate_path(&path, PathOperationMode::ReplaceExisting);

        // Verify navigation succeeds and points to the correct value
        assert!(result.is_ok());
        let stack = result.unwrap();
        assert_eq!(stack.len(), 2);

        // Verify we can get the value at this position
        let name_index = stack[1].field_value_index;
        let (header, header_size) = jsonb.read_header(name_index).unwrap();
        assert_eq!(header.0, ElementType::TEXT);

        // Extract the actual string value to verify
        let text_bytes = &jsonb.data[name_index + header_size..name_index + header_size + header.1];
        let text = std::str::from_utf8(text_bytes).unwrap();
        assert_eq!(text, "John");
    }

    #[test]
    fn test_navigate_nested_object_property() {
        let json_str = r#"{"person": {"name": "John", "age": 30}}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a path to the nested "name" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("person"), false),
            PathElement::Key(Cow::Borrowed("name"), false),
        ]);

        // Navigate to the path
        let result = jsonb.navigate_path(&path, PathOperationMode::ReplaceExisting);

        // Verify navigation succeeds
        assert!(result.is_ok());
        let stack = result.unwrap();
        assert_eq!(stack.len(), 3);
    }

    #[test]
    fn test_navigate_array_element() {
        let json_str = r#"{"items": [10, 20, 30]}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a path to the second array element (index 1)
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("items"), false),
            PathElement::ArrayLocator(Some(1)),
        ]);

        // Navigate to the path
        let result = jsonb.navigate_path(&path, PathOperationMode::ReplaceExisting);

        // Verify navigation succeeds
        assert!(result.is_ok());
        let stack = result.unwrap();
        assert_eq!(stack.len(), 2);

        // Verify we can get the value at the array position
        assert!(stack[1].has_specific_index());
        let array_element_index = stack[1].get_array_index().unwrap();
        let (header, header_size) = jsonb.read_header(array_element_index).unwrap();
        assert_eq!(header.0, ElementType::INT);

        // Extract the actual integer value to verify
        let int_bytes = &jsonb.data
            [array_element_index + header_size..array_element_index + header_size + header.1];
        let int_str = std::str::from_utf8(int_bytes).unwrap();
        assert_eq!(int_str, "20");
    }

    #[test]
    fn test_navigate_negative_array_index() {
        let json_str = r#"{"items": [10, 20, 30]}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a path to the last array element (index -1)
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("items"), false),
            PathElement::ArrayLocator(Some(-1)),
        ]);

        // Navigate to the path
        let result = jsonb.navigate_path(&path, PathOperationMode::ReplaceExisting);

        // Verify navigation succeeds
        assert!(result.is_ok());
        let stack = result.unwrap();
        assert_eq!(stack.len(), 2);

        // Verify we can get the value at the array position
        assert!(stack[1].has_specific_index());
    }

    #[test]
    fn test_set_operation() {
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a new value to set
        let new_value = Jsonb::from_str("\"Jane\"").unwrap();
        let mut operation = SetOperation::new(new_value);

        // Create a path to the "name" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("name"), false),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Verify the value was updated
        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"name":"Jane","age":30}"#);
    }

    #[test]
    fn test_insert_operation() {
        let json_str = r#"{"name": "John"}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a new value to insert
        let new_value = Jsonb::from_str("30").unwrap();
        let mut operation = InsertOperation::new(new_value);

        // Create a path to a new "age" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("age"), false),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Verify the value was inserted
        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"name":"John","age":30}"#);
    }

    #[test]
    fn test_delete_operation() {
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a delete operation
        let mut operation = DeleteOperation::new();

        // Create a path to the "age" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("age"), false),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Verify the property was deleted
        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"name":"John"}"#);
    }

    #[test]
    fn test_replace_operation() {
        let json_str = r#"{"items": [10, 20, 30]}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a new value to replace with
        let new_value = Jsonb::from_str("50").unwrap();
        let mut operation = ReplaceOperation::new(new_value);

        // Create a path to the second array element (index 1)
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("items"), false),
            PathElement::ArrayLocator(Some(1)),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Verify the value was replaced
        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"items":[10,50,30]}"#);
    }

    #[test]
    fn test_search_operation() {
        let json_str = r#"{"person": {"name": "John", "age": 30}}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a search operation
        let mut operation = SearchOperation::new(100);

        // Create a path to the "person" property
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("person"), false),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Get the search result
        let search_result = operation.result();
        let result_str = search_result.to_string().unwrap();

        // Verify the search found the correct value
        assert_eq!(result_str, r#"{"name":"John","age":30}"#);
    }

    #[test]
    fn test_error_for_nonexistent_path() {
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a new value to set
        let new_value = Jsonb::from_str("\"Doe\"").unwrap();
        let mut operation = ReplaceOperation::new(new_value);

        // Create a path to a non-existent property with ReplaceExisting mode
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("surname"), false),
        ]);

        // Execute the operation - should fail because path doesn't exist
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_err());
    }

    #[test]
    fn test_deep_nested_path() {
        let json_str = r#"{"level1": {"level2": {"level3": {"value": 42}}}}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Create a new value to set
        let new_value = Jsonb::from_str("100").unwrap();
        let mut operation = SetOperation::new(new_value);

        // Create a deeply nested path
        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("level1"), false),
            PathElement::Key(Cow::Borrowed("level2"), false),
            PathElement::Key(Cow::Borrowed("level3"), false),
            PathElement::Key(Cow::Borrowed("value"), false),
        ]);

        // Execute the operation
        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Verify the deep value was updated
        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(
            updated_json,
            r#"{"level1":{"level2":{"level3":{"value":100}}}}"#
        );
    }

    #[test]
    fn test_path_modes() {
        // Test the different path operation modes

        // 1. ReplaceExisting mode - should fail when path doesn't exist
        let json_str = r#"{"name": "John"}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        let mut operation = SetOperation::new(Jsonb::from_str("30").unwrap());
        operation.mode = PathOperationMode::ReplaceExisting;

        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("age"), false),
        ]);

        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_err());

        // 2. InsertNew mode - should succeed for new paths
        let json_str = r#"{"name": "John"}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        let mut operation = InsertOperation::new(Jsonb::from_str("30").unwrap());
        operation.mode = PathOperationMode::InsertNew;

        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("age"), false),
        ]);

        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"name":"John","age":30}"#);

        // 3. InsertNew mode - should fail when path already exists
        let mut operation = InsertOperation::new(Jsonb::from_str("31").unwrap());
        operation.mode = PathOperationMode::InsertNew;

        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_err());

        // 4. Upsert mode - should work for both existing and new paths
        let json_str = r#"{"name": "John", "age": 30}"#;
        let mut jsonb = Jsonb::from_str(json_str).unwrap();

        // Update existing value with Upsert
        let mut operation = SetOperation::new(Jsonb::from_str("31").unwrap());
        operation.mode = PathOperationMode::Upsert;

        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("age"), false),
        ]);

        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        // Insert new value with Upsert
        let mut operation = SetOperation::new(Jsonb::from_str("\"Doe\"").unwrap());
        operation.mode = PathOperationMode::Upsert;

        let path = create_path(vec![
            PathElement::Root(),
            PathElement::Key(Cow::Borrowed("surname"), false),
        ]);

        let result = jsonb.operate_on_path(&path, &mut operation);
        assert!(result.is_ok());

        let updated_json = jsonb.to_string().unwrap();
        assert_eq!(updated_json, r#"{"name":"John","age":31,"surname":"Doe"}"#);
    }
}
