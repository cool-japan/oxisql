#![allow(unused_variables)]
use super::numeric::{apply_numeric_affinity, execute_sqlite_version};
use super::values::{exec_char, exec_concat_strings};

#[cfg(test)]
mod tests_2 {
    use super::*;
    use crate::types::{Text, Value};
    #[test]
    fn test_apply_numeric_affinity_partial_numbers() {
        let mut reg = Register::Value(Value::Text(Text::from_str("123abc")));
        assert!(!apply_numeric_affinity(&mut reg, false));
        assert!(matches!(reg, Register::Value(Value::Text(_))));
        let mut reg = Register::Value(Value::Text(Text::from_str("-53093015420544-15062897")));
        assert!(!apply_numeric_affinity(&mut reg, false));
        assert!(matches!(reg, Register::Value(Value::Text(_))));
        let mut reg = Register::Value(Value::Text(Text::from_str("123.45xyz")));
        assert!(!apply_numeric_affinity(&mut reg, false));
        assert!(matches!(reg, Register::Value(Value::Text(_))));
    }
    #[test]
    fn test_apply_numeric_affinity_complete_numbers() {
        let mut reg = Register::Value(Value::Text(Text::from_str("123")));
        assert!(apply_numeric_affinity(&mut reg, false));
        assert_eq!(*reg.get_owned_value(), Value::Integer(123));
        let mut reg = Register::Value(Value::Text(Text::from_str("123.45")));
        assert!(apply_numeric_affinity(&mut reg, false));
        assert_eq!(*reg.get_owned_value(), Value::Float(123.45));
        let mut reg = Register::Value(Value::Text(Text::from_str("  -456  ")));
        assert!(apply_numeric_affinity(&mut reg, false));
        assert_eq!(*reg.get_owned_value(), Value::Integer(-456));
        let mut reg = Register::Value(Value::Text(Text::from_str("0")));
        assert!(apply_numeric_affinity(&mut reg, false));
        assert_eq!(*reg.get_owned_value(), Value::Integer(0));
    }
    #[test]
    fn test_exec_add() {
        let inputs = vec![
            (Value::Integer(3), Value::Integer(1)),
            (Value::Float(3.0), Value::Float(1.0)),
            (Value::Float(3.0), Value::Integer(1)),
            (Value::Integer(3), Value::Float(1.0)),
            (Value::Null, Value::Null),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Float(1.0)),
            (Value::Null, Value::Text(Text::from_str("2"))),
            (Value::Integer(1), Value::Null),
            (Value::Float(1.0), Value::Null),
            (Value::Text(Text::from_str("1")), Value::Null),
            (
                Value::Text(Text::from_str("1")),
                Value::Text(Text::from_str("3")),
            ),
            (
                Value::Text(Text::from_str("1.0")),
                Value::Text(Text::from_str("3.0")),
            ),
            (Value::Text(Text::from_str("1.0")), Value::Float(3.0)),
            (Value::Text(Text::from_str("1.0")), Value::Integer(3)),
            (Value::Float(1.0), Value::Text(Text::from_str("3.0"))),
            (Value::Integer(1), Value::Text(Text::from_str("3"))),
        ];
        let outputs = [
            Value::Integer(4),
            Value::Float(4.0),
            Value::Float(4.0),
            Value::Float(4.0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Integer(4),
            Value::Float(4.0),
            Value::Float(4.0),
            Value::Float(4.0),
            Value::Float(4.0),
            Value::Float(4.0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_add(rhs),
                outputs[i],
                "Wrong ADD for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_subtract() {
        let inputs = vec![
            (Value::Integer(3), Value::Integer(1)),
            (Value::Float(3.0), Value::Float(1.0)),
            (Value::Float(3.0), Value::Integer(1)),
            (Value::Integer(3), Value::Float(1.0)),
            (Value::Null, Value::Null),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Float(1.0)),
            (Value::Null, Value::Text(Text::from_str("1"))),
            (Value::Integer(1), Value::Null),
            (Value::Float(1.0), Value::Null),
            (Value::Text(Text::from_str("4")), Value::Null),
            (
                Value::Text(Text::from_str("1")),
                Value::Text(Text::from_str("3")),
            ),
            (
                Value::Text(Text::from_str("1.0")),
                Value::Text(Text::from_str("3.0")),
            ),
            (Value::Text(Text::from_str("1.0")), Value::Float(3.0)),
            (Value::Text(Text::from_str("1.0")), Value::Integer(3)),
            (Value::Float(1.0), Value::Text(Text::from_str("3.0"))),
            (Value::Integer(1), Value::Text(Text::from_str("3"))),
        ];
        let outputs = [
            Value::Integer(2),
            Value::Float(2.0),
            Value::Float(2.0),
            Value::Float(2.0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Integer(-2),
            Value::Float(-2.0),
            Value::Float(-2.0),
            Value::Float(-2.0),
            Value::Float(-2.0),
            Value::Float(-2.0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_subtract(rhs),
                outputs[i],
                "Wrong subtract for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_multiply() {
        let inputs = vec![
            (Value::Integer(3), Value::Integer(2)),
            (Value::Float(3.0), Value::Float(2.0)),
            (Value::Float(3.0), Value::Integer(2)),
            (Value::Integer(3), Value::Float(2.0)),
            (Value::Null, Value::Null),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Float(1.0)),
            (Value::Null, Value::Text(Text::from_str("1"))),
            (Value::Integer(1), Value::Null),
            (Value::Float(1.0), Value::Null),
            (Value::Text(Text::from_str("4")), Value::Null),
            (
                Value::Text(Text::from_str("2")),
                Value::Text(Text::from_str("3")),
            ),
            (
                Value::Text(Text::from_str("2.0")),
                Value::Text(Text::from_str("3.0")),
            ),
            (Value::Text(Text::from_str("2.0")), Value::Float(3.0)),
            (Value::Text(Text::from_str("2.0")), Value::Integer(3)),
            (Value::Float(2.0), Value::Text(Text::from_str("3.0"))),
            (Value::Integer(2), Value::Text(Text::from_str("3.0"))),
        ];
        let outputs = [
            Value::Integer(6),
            Value::Float(6.0),
            Value::Float(6.0),
            Value::Float(6.0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Integer(6),
            Value::Float(6.0),
            Value::Float(6.0),
            Value::Float(6.0),
            Value::Float(6.0),
            Value::Float(6.0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_multiply(rhs),
                outputs[i],
                "Wrong multiply for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_divide() {
        let inputs = vec![
            (Value::Integer(1), Value::Integer(0)),
            (Value::Float(1.0), Value::Float(0.0)),
            (Value::Integer(i64::MIN), Value::Integer(-1)),
            (Value::Float(6.0), Value::Float(2.0)),
            (Value::Float(6.0), Value::Integer(2)),
            (Value::Integer(6), Value::Integer(2)),
            (Value::Null, Value::Integer(2)),
            (Value::Integer(2), Value::Null),
            (Value::Null, Value::Null),
            (
                Value::Text(Text::from_str("6")),
                Value::Text(Text::from_str("2")),
            ),
            (Value::Text(Text::from_str("6")), Value::Integer(2)),
        ];
        let outputs = [
            Value::Null,
            Value::Null,
            Value::Float(9.223372036854776e18),
            Value::Float(3.0),
            Value::Float(3.0),
            Value::Float(3.0),
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Float(3.0),
            Value::Float(3.0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_divide(rhs),
                outputs[i],
                "Wrong divide for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_remainder() {
        let inputs = vec![
            (Value::Null, Value::Null),
            (Value::Null, Value::Float(1.0)),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Text(Text::from_str("1"))),
            (Value::Float(1.0), Value::Null),
            (Value::Integer(1), Value::Null),
            (Value::Integer(12), Value::Integer(0)),
            (Value::Float(12.0), Value::Float(0.0)),
            (Value::Float(12.0), Value::Integer(0)),
            (Value::Integer(12), Value::Float(0.0)),
            (Value::Integer(i64::MIN), Value::Integer(-1)),
            (Value::Integer(12), Value::Integer(3)),
            (Value::Float(12.0), Value::Float(3.0)),
            (Value::Float(12.0), Value::Integer(3)),
            (Value::Integer(12), Value::Float(3.0)),
            (Value::Integer(12), Value::Integer(-3)),
            (Value::Float(12.0), Value::Float(-3.0)),
            (Value::Float(12.0), Value::Integer(-3)),
            (Value::Integer(12), Value::Float(-3.0)),
            (
                Value::Text(Text::from_str("12.0")),
                Value::Text(Text::from_str("3.0")),
            ),
            (Value::Text(Text::from_str("12.0")), Value::Float(3.0)),
            (Value::Float(12.0), Value::Text(Text::from_str("3.0"))),
        ];
        let outputs = vec![
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Float(0.0),
            Value::Integer(0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Integer(0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
            Value::Float(0.0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_remainder(rhs),
                outputs[i],
                "Wrong remainder for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_and() {
        let inputs = [
            (Value::Integer(0), Value::Null),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Null),
            (Value::Float(0.0), Value::Null),
            (Value::Integer(1), Value::Float(2.2)),
            (Value::Integer(0), Value::Text(Text::from_str("string"))),
            (Value::Integer(0), Value::Text(Text::from_str("1"))),
            (Value::Integer(1), Value::Text(Text::from_str("1"))),
        ];
        let outputs = [
            Value::Integer(0),
            Value::Null,
            Value::Null,
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(1),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_and(rhs),
                outputs[i],
                "Wrong AND for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    #[test]
    fn test_exec_or() {
        let inputs = vec![
            (Value::Integer(0), Value::Null),
            (Value::Null, Value::Integer(1)),
            (Value::Null, Value::Null),
            (Value::Float(0.0), Value::Null),
            (Value::Integer(1), Value::Float(2.2)),
            (Value::Float(0.0), Value::Integer(0)),
            (Value::Integer(0), Value::Text(Text::from_str("string"))),
            (Value::Integer(0), Value::Text(Text::from_str("1"))),
            (Value::Integer(0), Value::Text(Text::from_str(""))),
        ];
        let outputs = [
            Value::Null,
            Value::Integer(1),
            Value::Null,
            Value::Null,
            Value::Integer(1),
            Value::Integer(0),
            Value::Integer(0),
            Value::Integer(1),
            Value::Integer(0),
        ];
        assert_eq!(
            inputs.len(),
            outputs.len(),
            "Inputs and Outputs should have same size"
        );
        for (i, (lhs, rhs)) in inputs.iter().enumerate() {
            assert_eq!(
                lhs.exec_or(rhs),
                outputs[i],
                "Wrong OR for lhs: {}, rhs: {}",
                lhs,
                rhs
            );
        }
    }
    use super::{exec_char, exec_concat_strings, execute_sqlite_version};
    use crate::vdbe::{Bitfield, Register};
    use std::collections::HashMap;
    #[test]
    fn test_length() {
        let input_str = Value::build_text("bob");
        let expected_len = Value::Integer(3);
        assert_eq!(input_str.exec_length(), expected_len);
        let input_integer = Value::Integer(123);
        let expected_len = Value::Integer(3);
        assert_eq!(input_integer.exec_length(), expected_len);
        let input_float = Value::Float(123.456);
        let expected_len = Value::Integer(7);
        assert_eq!(input_float.exec_length(), expected_len);
        let expected_blob = Value::Blob("example".as_bytes().to_vec());
        let expected_len = Value::Integer(7);
        assert_eq!(expected_blob.exec_length(), expected_len);
    }
    #[test]
    fn test_quote() {
        let input = Value::build_text("abc\0edf");
        let expected = Value::build_text("'abc'");
        assert_eq!(input.exec_quote(), expected);
        let input = Value::Integer(123);
        let expected = Value::Integer(123);
        assert_eq!(input.exec_quote(), expected);
        let input = Value::build_text("hello''world");
        let expected = Value::build_text("'hello''''world'");
        assert_eq!(input.exec_quote(), expected);
        // QUOTE() renders a BLOB as an uppercase hex literal: X'...'.
        let input = Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]);
        let expected = Value::build_text("X'DEADBEEF'");
        assert_eq!(input.exec_quote(), expected);
        let input = Value::Blob(vec![]);
        let expected = Value::build_text("X''");
        assert_eq!(input.exec_quote(), expected);
    }
    #[test]
    fn test_exec_concat_blob_rules() {
        // Blob || Blob stays a Blob: raw byte concatenation, no text involved.
        let lhs = Value::Blob(vec![0x01, 0x02]);
        let rhs = Value::Blob(vec![0x03, 0x04]);
        assert_eq!(
            lhs.exec_concat(&rhs),
            Value::Blob(vec![0x01, 0x02, 0x03, 0x04])
        );

        // Blob || Text coerces the Blob operand to Text (byte-buffer reuse).
        let lhs = Value::Blob(vec![0x41, 0x42]); // "AB"
        let rhs = Value::build_text("CD");
        assert_eq!(lhs.exec_concat(&rhs), Value::build_text("ABCD"));

        // Text || Blob coerces the Blob operand to Text (byte-buffer reuse),
        // in the opposite operand order.
        let lhs = Value::build_text("AB");
        let rhs = Value::Blob(vec![0x43, 0x44]); // "CD"
        assert_eq!(lhs.exec_concat(&rhs), Value::build_text("ABCD"));

        // Blob || Integer / Float and their reverses also coerce to Text.
        let lhs = Value::Blob(vec![0x41]); // "A"
        let rhs = Value::Integer(1);
        assert_eq!(lhs.exec_concat(&rhs), Value::build_text("A1"));
        let lhs = Value::Integer(1);
        let rhs = Value::Blob(vec![0x41]); // "A"
        assert_eq!(lhs.exec_concat(&rhs), Value::build_text("1A"));

        // NULL still short-circuits to NULL even when the other side is a Blob.
        assert_eq!(
            Value::Null.exec_concat(&Value::Blob(vec![1, 2])),
            Value::Null
        );
        assert_eq!(
            Value::Blob(vec![1, 2]).exec_concat(&Value::Null),
            Value::Null
        );

        // The Blob||Text result must actually be a Text variant (not a Blob),
        // matching SQLite's `typeof(x'4142' || 'CD')` = 'text'.
        assert!(matches!(
            Value::Blob(vec![0x41, 0x42]).exec_concat(&Value::build_text("CD")),
            Value::Text(_)
        ));
        // The Blob||Blob result must actually be a Blob variant (not Text),
        // matching SQLite's `typeof(x'0102' || x'0304')` = 'blob'.
        assert!(matches!(
            Value::Blob(vec![1, 2]).exec_concat(&Value::Blob(vec![3, 4])),
            Value::Blob(_)
        ));
    }
    #[test]
    fn test_exec_concat_strings_blob_rules() {
        // concat() (SQLite 3.44+) ALWAYS returns a string -- unlike `||`,
        // Blob+Blob does NOT stay a Blob; every Blob argument coerces to Text.
        let result = exec_concat_strings(&[
            Register::Value(Value::Blob(vec![0x41, 0x42])), // "AB"
            Register::Value(Value::Blob(vec![0x43, 0x44])), // "CD"
        ]);
        assert_eq!(result, Value::build_text("ABCD"));
        assert!(matches!(result, Value::Text(_)));

        // Mixed Blob/Text arguments all coerce to Text and concatenate in order.
        let result = exec_concat_strings(&[
            Register::Value(Value::Blob(vec![0x41])), // "A"
            Register::Value(Value::build_text("B")),
            Register::Value(Value::Blob(vec![0x43])), // "C"
        ]);
        assert_eq!(result, Value::build_text("ABC"));

        // NULL arguments are skipped (treated as empty), consistent with the
        // pre-existing non-Blob behavior of concat().
        let result = exec_concat_strings(&[
            Register::Value(Value::Null),
            Register::Value(Value::Blob(vec![0x41])), // "A"
        ]);
        assert_eq!(result, Value::build_text("A"));

        // A lone Blob argument also coerces to Text, not Blob.
        let result = exec_concat_strings(&[Register::Value(Value::Blob(vec![0x41, 0x42]))]);
        assert_eq!(result, Value::build_text("AB"));
        assert!(matches!(result, Value::Text(_)));
    }
    #[test]
    fn test_typeof() {
        let input = Value::Null;
        let expected: Value = Value::build_text("null");
        assert_eq!(input.exec_typeof(), expected);
        let input = Value::Integer(123);
        let expected: Value = Value::build_text("integer");
        assert_eq!(input.exec_typeof(), expected);
        let input = Value::Float(123.456);
        let expected: Value = Value::build_text("real");
        assert_eq!(input.exec_typeof(), expected);
        let input = Value::build_text("hello");
        let expected: Value = Value::build_text("text");
        assert_eq!(input.exec_typeof(), expected);
        let input = Value::Blob("limbo".as_bytes().to_vec());
        let expected: Value = Value::build_text("blob");
        assert_eq!(input.exec_typeof(), expected);
    }
    #[test]
    fn test_unicode() {
        assert_eq!(Value::build_text("a").exec_unicode(), Value::Integer(97));
        assert_eq!(
            Value::build_text("😊").exec_unicode(),
            Value::Integer(128522)
        );
        assert_eq!(Value::build_text("").exec_unicode(), Value::Null);
        assert_eq!(Value::Integer(23).exec_unicode(), Value::Integer(50));
        assert_eq!(Value::Integer(0).exec_unicode(), Value::Integer(48));
        assert_eq!(Value::Float(0.0).exec_unicode(), Value::Integer(48));
        assert_eq!(Value::Float(23.45).exec_unicode(), Value::Integer(50));
        assert_eq!(Value::Null.exec_unicode(), Value::Null);
        assert_eq!(
            Value::Blob("example".as_bytes().to_vec()).exec_unicode(),
            Value::Integer(101)
        );
    }
    #[test]
    fn test_min_max() {
        let input_int_vec = [
            Register::Value(Value::Integer(-1)),
            Register::Value(Value::Integer(10)),
        ];
        assert_eq!(
            Value::exec_min(input_int_vec.iter().map(|v| v.get_owned_value())),
            Value::Integer(-1)
        );
        assert_eq!(
            Value::exec_max(input_int_vec.iter().map(|v| v.get_owned_value())),
            Value::Integer(10)
        );
        let str1 = Register::Value(Value::build_text("A"));
        let str2 = Register::Value(Value::build_text("z"));
        let input_str_vec = [str2, str1.clone()];
        assert_eq!(
            Value::exec_min(input_str_vec.iter().map(|v| v.get_owned_value())),
            Value::build_text("A")
        );
        assert_eq!(
            Value::exec_max(input_str_vec.iter().map(|v| v.get_owned_value())),
            Value::build_text("z")
        );
        let input_null_vec = [Register::Value(Value::Null), Register::Value(Value::Null)];
        assert_eq!(
            Value::exec_min(input_null_vec.iter().map(|v| v.get_owned_value())),
            Value::Null
        );
        assert_eq!(
            Value::exec_max(input_null_vec.iter().map(|v| v.get_owned_value())),
            Value::Null
        );
        let input_mixed_vec = [Register::Value(Value::Integer(10)), str1];
        assert_eq!(
            Value::exec_min(input_mixed_vec.iter().map(|v| v.get_owned_value())),
            Value::Integer(10)
        );
        assert_eq!(
            Value::exec_max(input_mixed_vec.iter().map(|v| v.get_owned_value())),
            Value::build_text("A")
        );
    }
    #[test]
    fn test_trim() {
        let input_str = Value::build_text("     Bob and Alice     ");
        let expected_str = Value::build_text("Bob and Alice");
        assert_eq!(input_str.exec_trim(None), expected_str);
        let input_str = Value::build_text("     Bob and Alice     ");
        let pattern_str = Value::build_text("Bob and");
        let expected_str = Value::build_text("Alice");
        assert_eq!(input_str.exec_trim(Some(&pattern_str)), expected_str);
    }
    #[test]
    fn test_ltrim() {
        let input_str = Value::build_text("     Bob and Alice     ");
        let expected_str = Value::build_text("Bob and Alice     ");
        assert_eq!(input_str.exec_ltrim(None), expected_str);
        let input_str = Value::build_text("     Bob and Alice     ");
        let pattern_str = Value::build_text("Bob and");
        let expected_str = Value::build_text("Alice     ");
        assert_eq!(input_str.exec_ltrim(Some(&pattern_str)), expected_str);
    }
    #[test]
    fn test_rtrim() {
        let input_str = Value::build_text("     Bob and Alice     ");
        let expected_str = Value::build_text("     Bob and Alice");
        assert_eq!(input_str.exec_rtrim(None), expected_str);
        let input_str = Value::build_text("     Bob and Alice     ");
        let pattern_str = Value::build_text("Bob and");
        let expected_str = Value::build_text("     Bob and Alice");
        assert_eq!(input_str.exec_rtrim(Some(&pattern_str)), expected_str);
        let input_str = Value::build_text("     Bob and Alice     ");
        let pattern_str = Value::build_text("and Alice");
        let expected_str = Value::build_text("     Bob");
        assert_eq!(input_str.exec_rtrim(Some(&pattern_str)), expected_str);
    }
    #[test]
    fn test_soundex() {
        let input_str = Value::build_text("Pfister");
        let expected_str = Value::build_text("P236");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("husobee");
        let expected_str = Value::build_text("H210");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Tymczak");
        let expected_str = Value::build_text("T522");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Ashcraft");
        let expected_str = Value::build_text("A261");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Robert");
        let expected_str = Value::build_text("R163");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Rupert");
        let expected_str = Value::build_text("R163");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Rubin");
        let expected_str = Value::build_text("R150");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Kant");
        let expected_str = Value::build_text("K530");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("Knuth");
        let expected_str = Value::build_text("K530");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("x");
        let expected_str = Value::build_text("X000");
        assert_eq!(input_str.exec_soundex(), expected_str);
        let input_str = Value::build_text("闪电五连鞭");
        let expected_str = Value::build_text("?000");
        assert_eq!(input_str.exec_soundex(), expected_str);
    }
    #[test]
    fn test_upper_case() {
        let input_str = Value::build_text("Limbo");
        let expected_str = Value::build_text("LIMBO");
        assert_eq!(input_str.exec_upper().unwrap(), expected_str);
        let input_int = Value::Integer(10);
        assert_eq!(input_int.exec_upper().unwrap(), input_int);
        assert_eq!(Value::Null.exec_upper().unwrap(), Value::Null)
    }
    #[test]
    fn test_lower_case() {
        let input_str = Value::build_text("Limbo");
        let expected_str = Value::build_text("limbo");
        assert_eq!(input_str.exec_lower().unwrap(), expected_str);
        let input_int = Value::Integer(10);
        assert_eq!(input_int.exec_lower().unwrap(), input_int);
        assert_eq!(Value::Null.exec_lower().unwrap(), Value::Null)
    }
    #[test]
    fn test_hex() {
        let input_str = Value::build_text("limbo");
        let expected_val = Value::build_text("6C696D626F");
        assert_eq!(input_str.exec_hex(), expected_val);
        let input_int = Value::Integer(100);
        let expected_val = Value::build_text("313030");
        assert_eq!(input_int.exec_hex(), expected_val);
        let input_float = Value::Float(12.34);
        let expected_val = Value::build_text("31322E3334");
        assert_eq!(input_float.exec_hex(), expected_val);
        let input_blob = Value::Blob(vec![0xff]);
        let expected_val = Value::build_text("FF");
        assert_eq!(input_blob.exec_hex(), expected_val);
    }
    #[test]
    fn test_unhex() {
        let input = Value::build_text("6f");
        let expected = Value::Blob(vec![0x6f]);
        assert_eq!(input.exec_unhex(None), expected);
        let input = Value::build_text("6f");
        let expected = Value::Blob(vec![0x6f]);
        assert_eq!(input.exec_unhex(None), expected);
        let input = Value::build_text("611");
        let expected = Value::Null;
        assert_eq!(input.exec_unhex(None), expected);
        let input = Value::build_text("");
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_unhex(None), expected);
        let input = Value::build_text("61x");
        let expected = Value::Null;
        assert_eq!(input.exec_unhex(None), expected);
        let input = Value::Null;
        let expected = Value::Null;
        assert_eq!(input.exec_unhex(None), expected);
    }
    #[test]
    fn test_abs() {
        let int_positive_reg = Value::Integer(10);
        let int_negative_reg = Value::Integer(-10);
        assert_eq!(int_positive_reg.exec_abs().unwrap(), int_positive_reg);
        assert_eq!(int_negative_reg.exec_abs().unwrap(), int_positive_reg);
        let float_positive_reg = Value::Integer(10);
        let float_negative_reg = Value::Integer(-10);
        assert_eq!(float_positive_reg.exec_abs().unwrap(), float_positive_reg);
        assert_eq!(float_negative_reg.exec_abs().unwrap(), float_positive_reg);
        assert_eq!(
            Value::build_text("a").exec_abs().unwrap(),
            Value::Float(0.0)
        );
        assert_eq!(Value::Null.exec_abs().unwrap(), Value::Null);
        assert!(Value::Integer(i64::MIN).exec_abs().is_err());
    }
    #[test]
    fn test_char() {
        assert_eq!(
            exec_char(&[
                Register::Value(Value::Integer(108)),
                Register::Value(Value::Integer(105))
            ]),
            Value::build_text("li")
        );
        assert_eq!(exec_char(&[]), Value::build_text(""));
        assert_eq!(
            exec_char(&[Register::Value(Value::Null)]),
            Value::build_text("")
        );
        assert_eq!(
            exec_char(&[Register::Value(Value::build_text("a"))]),
            Value::build_text("")
        );
    }
    #[test]
    fn test_like_with_escape_or_regexmeta_chars() {
        assert!(Value::exec_like(None, r#"\%A"#, r#"\A"#));
        assert!(Value::exec_like(None, "%a%a", "aaaa"));
    }
    #[test]
    fn test_like_no_cache() {
        assert!(Value::exec_like(None, "a%", "aaaa"));
        assert!(Value::exec_like(None, "%a%a", "aaaa"));
        assert!(!Value::exec_like(None, "%a.a", "aaaa"));
        assert!(!Value::exec_like(None, "a.a%", "aaaa"));
        assert!(!Value::exec_like(None, "%a.ab", "aaaa"));
    }
    #[test]
    fn test_like_with_cache() {
        let mut cache = HashMap::new();
        assert!(Value::exec_like(Some(&mut cache), "a%", "aaaa"));
        assert!(Value::exec_like(Some(&mut cache), "%a%a", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "%a.a", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "a.a%", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "%a.ab", "aaaa"));
        assert!(Value::exec_like(Some(&mut cache), "a%", "aaaa"));
        assert!(Value::exec_like(Some(&mut cache), "%a%a", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "%a.a", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "a.a%", "aaaa"));
        assert!(!Value::exec_like(Some(&mut cache), "%a.ab", "aaaa"));
    }
    #[test]
    fn test_random() {
        match Value::exec_random() {
            Value::Integer(value) => {
                assert!(
                    (i64::MIN..=i64::MAX).contains(&value),
                    "Random number out of range"
                );
            }
            _ => panic!("exec_random did not return an Integer variant"),
        }
    }
    #[test]
    fn test_exec_randomblob() {
        struct TestCase {
            input: Value,
            expected_len: usize,
        }
        let test_cases = vec![
            TestCase {
                input: Value::Integer(5),
                expected_len: 5,
            },
            TestCase {
                input: Value::Integer(0),
                expected_len: 1,
            },
            TestCase {
                input: Value::Integer(-1),
                expected_len: 1,
            },
            TestCase {
                input: Value::build_text(""),
                expected_len: 1,
            },
            TestCase {
                input: Value::build_text("5"),
                expected_len: 5,
            },
            TestCase {
                input: Value::build_text("0"),
                expected_len: 1,
            },
            TestCase {
                input: Value::build_text("-1"),
                expected_len: 1,
            },
            TestCase {
                input: Value::Float(2.9),
                expected_len: 2,
            },
            TestCase {
                input: Value::Float(-3.15),
                expected_len: 1,
            },
            TestCase {
                input: Value::Null,
                expected_len: 1,
            },
        ];
        for test_case in &test_cases {
            let result = test_case.input.exec_randomblob();
            match result {
                Value::Blob(blob) => {
                    assert_eq!(blob.len(), test_case.expected_len);
                }
                _ => panic!("exec_randomblob did not return a Blob variant"),
            }
        }
    }
    #[test]
    fn test_exec_round() {
        let input_val = Value::Float(123.456);
        let expected_val = Value::Float(123.0);
        assert_eq!(input_val.exec_round(None), expected_val);
        let input_val = Value::Float(123.456);
        let precision_val = Value::Integer(2);
        let expected_val = Value::Float(123.46);
        assert_eq!(input_val.exec_round(Some(&precision_val)), expected_val);
        let input_val = Value::Float(123.456);
        let precision_val = Value::build_text("1");
        let expected_val = Value::Float(123.5);
        assert_eq!(input_val.exec_round(Some(&precision_val)), expected_val);
        let input_val = Value::build_text("123.456");
        let precision_val = Value::Integer(2);
        let expected_val = Value::Float(123.46);
        assert_eq!(input_val.exec_round(Some(&precision_val)), expected_val);
        let input_val = Value::Integer(123);
        let precision_val = Value::Integer(1);
        let expected_val = Value::Float(123.0);
        assert_eq!(input_val.exec_round(Some(&precision_val)), expected_val);
        let input_val = Value::Float(100.123);
        let expected_val = Value::Float(100.0);
        assert_eq!(input_val.exec_round(None), expected_val);
        let input_val = Value::Float(100.123);
        let expected_val = Value::Null;
        assert_eq!(input_val.exec_round(Some(&Value::Null)), expected_val);
    }
    #[test]
    fn test_exec_if() {
        let reg = Value::Integer(0);
        assert!(!reg.exec_if(false, false));
        assert!(reg.exec_if(false, true));
        let reg = Value::Integer(1);
        assert!(reg.exec_if(false, false));
        assert!(!reg.exec_if(false, true));
        let reg = Value::Null;
        assert!(!reg.exec_if(false, false));
        assert!(!reg.exec_if(false, true));
        let reg = Value::Null;
        assert!(reg.exec_if(true, false));
        assert!(reg.exec_if(true, true));
        let reg = Value::Null;
        assert!(!reg.exec_if(false, false));
        assert!(!reg.exec_if(false, true));
    }
    #[test]
    fn test_nullif() {
        assert_eq!(
            Value::Integer(1).exec_nullif(&Value::Integer(1)),
            Value::Null
        );
        assert_eq!(
            Value::Float(1.1).exec_nullif(&Value::Float(1.1)),
            Value::Null
        );
        assert_eq!(
            Value::build_text("limbo").exec_nullif(&Value::build_text("limbo")),
            Value::Null
        );
        assert_eq!(
            Value::Integer(1).exec_nullif(&Value::Integer(2)),
            Value::Integer(1)
        );
        assert_eq!(
            Value::Float(1.1).exec_nullif(&Value::Float(1.2)),
            Value::Float(1.1)
        );
        assert_eq!(
            Value::build_text("limbo").exec_nullif(&Value::build_text("limb")),
            Value::build_text("limbo")
        );
    }
    #[test]
    fn test_substring() {
        let str_value = Value::build_text("limbo");
        let start_value = Value::Integer(1);
        let length_value = Value::Integer(3);
        let expected_val = Value::build_text("lim");
        assert_eq!(
            Value::exec_substring(&str_value, &start_value, Some(&length_value)),
            expected_val
        );
        let str_value = Value::build_text("limbo");
        let start_value = Value::Integer(1);
        let length_value = Value::Integer(10);
        let expected_val = Value::build_text("limbo");
        assert_eq!(
            Value::exec_substring(&str_value, &start_value, Some(&length_value)),
            expected_val
        );
        let str_value = Value::build_text("limbo");
        let start_value = Value::Integer(10);
        let length_value = Value::Integer(3);
        let expected_val = Value::build_text("");
        assert_eq!(
            Value::exec_substring(&str_value, &start_value, Some(&length_value)),
            expected_val
        );
        let str_value = Value::build_text("limbo");
        let start_value = Value::Integer(3);
        let length_value = Value::Null;
        let expected_val = Value::build_text("mbo");
        assert_eq!(
            Value::exec_substring(&str_value, &start_value, Some(&length_value)),
            expected_val
        );
        let str_value = Value::build_text("limbo");
        let start_value = Value::Integer(10);
        let length_value = Value::Null;
        let expected_val = Value::build_text("");
        assert_eq!(
            Value::exec_substring(&str_value, &start_value, Some(&length_value)),
            expected_val
        );
    }
    #[test]
    fn test_exec_instr() {
        let input = Value::build_text("limbo");
        let pattern = Value::build_text("im");
        let expected = Value::Integer(2);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("limbo");
        let pattern = Value::build_text("limbo");
        let expected = Value::Integer(1);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("limbo");
        let pattern = Value::build_text("o");
        let expected = Value::Integer(5);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("liiiiimbo");
        let pattern = Value::build_text("ii");
        let expected = Value::Integer(2);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("limbo");
        let pattern = Value::build_text("limboX");
        let expected = Value::Integer(0);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("limbo");
        let pattern = Value::build_text("");
        let expected = Value::Integer(1);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("");
        let pattern = Value::build_text("limbo");
        let expected = Value::Integer(0);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("");
        let pattern = Value::build_text("");
        let expected = Value::Integer(1);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Null;
        let pattern = Value::Null;
        let expected = Value::Null;
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("limbo");
        let pattern = Value::Null;
        let expected = Value::Null;
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Null;
        let pattern = Value::build_text("limbo");
        let expected = Value::Null;
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Integer(123);
        let pattern = Value::Integer(2);
        let expected = Value::Integer(2);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Integer(123);
        let pattern = Value::Integer(5);
        let expected = Value::Integer(0);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Float(12.34);
        let pattern = Value::Float(2.3);
        let expected = Value::Integer(2);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Float(12.34);
        let pattern = Value::Float(5.6);
        let expected = Value::Integer(0);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Float(12.34);
        let pattern = Value::build_text(".");
        let expected = Value::Integer(3);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Blob(vec![1, 2, 3, 4, 5]);
        let pattern = Value::Blob(vec![3, 4]);
        let expected = Value::Integer(3);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Blob(vec![1, 2, 3, 4, 5]);
        let pattern = Value::Blob(vec![3, 2]);
        let expected = Value::Integer(0);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::Blob(vec![0x61, 0x62, 0x63, 0x64, 0x65]);
        let pattern = Value::build_text("cd");
        let expected = Value::Integer(3);
        assert_eq!(input.exec_instr(&pattern), expected);
        let input = Value::build_text("abcde");
        let pattern = Value::Blob(vec![0x63, 0x64]);
        let expected = Value::Integer(3);
        assert_eq!(input.exec_instr(&pattern), expected);
    }
    #[test]
    fn test_exec_sign() {
        let input = Value::Integer(42);
        let expected = Some(Value::Integer(1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Integer(-42);
        let expected = Some(Value::Integer(-1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Integer(0);
        let expected = Some(Value::Integer(0));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Float(0.0);
        let expected = Some(Value::Integer(0));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Float(0.1);
        let expected = Some(Value::Integer(1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Float(42.0);
        let expected = Some(Value::Integer(1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Float(-42.0);
        let expected = Some(Value::Integer(-1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::build_text("abc");
        let expected = Some(Value::Null);
        assert_eq!(input.exec_sign(), expected);
        let input = Value::build_text("42");
        let expected = Some(Value::Integer(1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::build_text("-42");
        let expected = Some(Value::Integer(-1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::build_text("0");
        let expected = Some(Value::Integer(0));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Blob(b"abc".to_vec());
        let expected = Some(Value::Null);
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Blob(b"42".to_vec());
        let expected = Some(Value::Integer(1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Blob(b"-42".to_vec());
        let expected = Some(Value::Integer(-1));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Blob(b"0".to_vec());
        let expected = Some(Value::Integer(0));
        assert_eq!(input.exec_sign(), expected);
        let input = Value::Null;
        let expected = Some(Value::Null);
        assert_eq!(input.exec_sign(), expected);
    }
    #[test]
    fn test_exec_zeroblob() {
        let input = Value::Integer(0);
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::Null;
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::Integer(4);
        let expected = Value::Blob(vec![0; 4]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::Integer(-1);
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::build_text("5");
        let expected = Value::Blob(vec![0; 5]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::build_text("-5");
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::build_text("text");
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::Float(2.6);
        let expected = Value::Blob(vec![0; 2]);
        assert_eq!(input.exec_zeroblob(), expected);
        let input = Value::Blob(vec![1]);
        let expected = Value::Blob(vec![]);
        assert_eq!(input.exec_zeroblob(), expected);
    }
    #[test]
    fn test_execute_sqlite_version() {
        let version_integer = 3046001;
        let expected = "3.46.1";
        assert_eq!(execute_sqlite_version(version_integer), expected);
    }
    #[test]
    fn test_replace() {
        let input_str = Value::build_text("bob");
        let pattern_str = Value::build_text("b");
        let replace_str = Value::build_text("a");
        let expected_str = Value::build_text("aoa");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bob");
        let pattern_str = Value::build_text("b");
        let replace_str = Value::build_text("");
        let expected_str = Value::build_text("o");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bob");
        let pattern_str = Value::build_text("b");
        let replace_str = Value::build_text("abc");
        let expected_str = Value::build_text("abcoabc");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bob");
        let pattern_str = Value::build_text("a");
        let replace_str = Value::build_text("b");
        let expected_str = Value::build_text("bob");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bob");
        let pattern_str = Value::build_text("");
        let replace_str = Value::build_text("a");
        let expected_str = Value::build_text("bob");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bob");
        let pattern_str = Value::Null;
        let replace_str = Value::build_text("a");
        let expected_str = Value::Null;
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bo5");
        let pattern_str = Value::Integer(5);
        let replace_str = Value::build_text("a");
        let expected_str = Value::build_text("boa");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bo5.0");
        let pattern_str = Value::Float(5.0);
        let replace_str = Value::build_text("a");
        let expected_str = Value::build_text("boa");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bo5");
        let pattern_str = Value::Float(5.0);
        let replace_str = Value::build_text("a");
        let expected_str = Value::build_text("bo5");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("bo5.0");
        let pattern_str = Value::Float(5.0);
        let replace_str = Value::Float(6.0);
        let expected_str = Value::build_text("bo6.0");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
        let input_str = Value::build_text("tes3");
        let pattern_str = Value::Integer(3);
        let replace_str = Value::Float(0.3);
        let expected_str = Value::build_text("tes0.3");
        assert_eq!(
            Value::exec_replace(&input_str, &pattern_str, &replace_str),
            expected_str
        );
    }
    #[test]
    fn test_likely() {
        let input = Value::build_text("limbo");
        let expected = Value::build_text("limbo");
        assert_eq!(input.exec_likely(), expected);
        let input = Value::Integer(100);
        let expected = Value::Integer(100);
        assert_eq!(input.exec_likely(), expected);
        let input = Value::Float(12.34);
        let expected = Value::Float(12.34);
        assert_eq!(input.exec_likely(), expected);
        let input = Value::Null;
        let expected = Value::Null;
        assert_eq!(input.exec_likely(), expected);
        let input = Value::Blob(vec![1, 2, 3, 4]);
        let expected = Value::Blob(vec![1, 2, 3, 4]);
        assert_eq!(input.exec_likely(), expected);
    }
    #[test]
    fn test_likelihood() {
        let value = Value::build_text("limbo");
        let prob = Value::Float(0.5);
        assert_eq!(value.exec_likelihood(&prob), value);
        let value = Value::build_text("database");
        let prob = Value::Float(0.9375);
        assert_eq!(value.exec_likelihood(&prob), value);
        let value = Value::Integer(100);
        let prob = Value::Float(1.0);
        assert_eq!(value.exec_likelihood(&prob), value);
        let value = Value::Float(12.34);
        let prob = Value::Float(0.5);
        assert_eq!(value.exec_likelihood(&prob), value);
        let value = Value::Null;
        let prob = Value::Float(0.5);
        assert_eq!(value.exec_likelihood(&prob), value);
        let value = Value::Blob(vec![1, 2, 3, 4]);
        let prob = Value::Float(0.5);
        assert_eq!(value.exec_likelihood(&prob), value);
        let prob = Value::build_text("0.5");
        assert_eq!(value.exec_likelihood(&prob), value);
        let prob = Value::Null;
        assert_eq!(value.exec_likelihood(&prob), value);
    }
    #[test]
    fn test_bitfield() {
        let mut bitfield = Bitfield::<4>::new();
        for i in 0..256 {
            bitfield.set(i);
            assert!(bitfield.get(i));
            for j in 0..i {
                assert!(bitfield.get(j));
            }
            for j in i + 1..256 {
                assert!(!bitfield.get(j));
            }
        }
        for i in 0..256 {
            bitfield.unset(i);
            assert!(!bitfield.get(i));
        }
    }
}
