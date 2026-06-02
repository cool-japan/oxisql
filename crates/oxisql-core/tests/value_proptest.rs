//! Property-based tests for `Value` round-trip: `ToSqlValue -> Value -> FromValue`.
//!
//! Verifies that for every supported Rust type, converting to a `Value` and
//! back produces the original value.

use oxisql_core::{FromValue, Value};
use proptest::prelude::*;

proptest! {
    #[test]
    fn prop_i64_round_trip(n: i64) {
        let v = Value::I64(n);
        let extracted: i64 = i64::from_value(&v).unwrap();
        prop_assert_eq!(extracted, n);
    }

    #[test]
    fn prop_f64_round_trip(
        n in any::<f64>().prop_filter("not nan or inf", |f| f.is_finite()),
    ) {
        let v = Value::F64(n);
        let extracted: f64 = f64::from_value(&v).unwrap();
        // Allow a small relative tolerance for floating-point representation.
        let tolerance = 1e-10 * n.abs().max(1.0);
        prop_assert!(
            (extracted - n).abs() < tolerance,
            "round-trip drift too large: {extracted} != {n} (tol={tolerance})"
        );
    }

    #[test]
    fn prop_bool_round_trip(b: bool) {
        let v = Value::Bool(b);
        let extracted: bool = bool::from_value(&v).unwrap();
        prop_assert_eq!(extracted, b);
    }

    #[test]
    fn prop_string_round_trip(s in ".*") {
        let v = Value::Text(s.clone());
        let extracted: String = String::from_value(&v).unwrap();
        prop_assert_eq!(extracted, s);
    }

    #[test]
    fn prop_null_option_round_trip(n: Option<i64>) {
        match n {
            None => {
                let v = Value::Null;
                let extracted: Option<i64> = Option::<i64>::from_value(&v).unwrap();
                prop_assert_eq!(extracted, None);
            }
            Some(x) => {
                let v = Value::I64(x);
                let extracted: Option<i64> = Option::<i64>::from_value(&v).unwrap();
                prop_assert_eq!(extracted, Some(x));
            }
        }
    }

    #[test]
    fn prop_bytes_round_trip(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let v = Value::Blob(bytes.clone());
        let extracted: Vec<u8> = Vec::<u8>::from_value(&v).unwrap();
        prop_assert_eq!(extracted, bytes);
    }

    /// Verify that `ToSqlValue::to_value()` and `FromValue::from_value()` compose
    /// correctly for i64 (smoke-test the trait round-trip, not just the enum).
    #[test]
    fn prop_to_sql_value_i64_round_trip(n: i64) {
        use oxisql_core::ToSqlValue;
        let v = n.to_value();
        let extracted: i64 = i64::from_value(&v).unwrap();
        prop_assert_eq!(extracted, n);
    }

    /// `i32` goes through `Value::I64` — verify it survives in-range values.
    #[test]
    fn prop_i32_round_trip(n: i32) {
        use oxisql_core::ToSqlValue;
        let v = n.to_value(); // produces Value::I64(n as i64)
        let extracted: i32 = i32::from_value(&v).unwrap();
        prop_assert_eq!(extracted, n);
    }
}
