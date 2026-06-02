#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-core` — core traits and types for the OxiSQL Pure-Rust SQL facade.
//!
//! This crate defines the public API surface (`Connection`, `Transaction`,
//! `Row`, `Value`, `OxiSqlError`) that every OxiSQL backend must implement.
//! It has no storage logic of its own; concrete backends live in
//! `oxisql-embedded`, `oxisql-postgres`, etc.
//!
//! # Extended Type System
//!
//! Beyond the basic scalar types (`Null`, `Bool`, `I64`, `F64`, `Text`,
//! `Blob`), `Value` supports rich database types:
//!
//! - [`Value::Timestamp`] — microseconds since Unix epoch (UTC)
//! - [`Value::Date`] — days since Unix epoch
//! - [`Value::Time`] — microseconds since midnight
//! - [`Value::Uuid`] — 128-bit UUID stored as `u128`
//! - [`Value::Json`] — JSON/JSONB stored as a `String`
//! - [`Value::Decimal`] — exact decimal as a string representation
//! - [`Value::Array`] — ordered collection of `Value`s (Postgres arrays)
//!
//! # Type-Safe Extraction
//!
//! Use [`Row::try_get`] with the [`FromValue`] trait for ergonomic,
//! type-safe value extraction:
//!
//! ```rust
//! # use oxisql_core::{Row, Value};
//! let row = Row::new(
//!     vec!["id".into(), "name".into()],
//!     vec![Value::I64(42), Value::Text("Alice".into())],
//! );
//! let id: i64 = row.try_get("id").unwrap();
//! let name: String = row.try_get("name").unwrap();
//! ```

mod cursor;
mod error;
pub mod middleware;
mod migrator;
pub mod params;
mod pool;
mod prepare;
pub mod query;
pub mod registry;
mod row;
pub mod schema;
mod traits;
mod value;

pub use cursor::Cursor;
pub use error::OxiSqlError;
pub use middleware::{
    ConnectionMetrics, LoggingConnection, MetricsConnection, MetricsSnapshot, RetryConnection,
    RetryPolicy, RetryPredicate,
};
pub use migrator::{MigrationInfo, MigrationStatus, Migrator};
pub use params::{bind_named_params, rewrite_named_params};
pub use pool::ConnectionPool;
pub use prepare::PreparedStatement;
pub use query::{
    BuiltQuery, DeleteBuilder, InsertBuilder, SelectBuilder, SortDirection, UpdateBuilder,
};
pub use registry::{SqlType, TypeRegistry};
pub use row::{FromValue, Row, RowSet};
pub use schema::{ColumnInfo, ForeignKeyInfo, IndexInfo, TableInfo, TableType};
pub use traits::{Connection, ToSqlValue, Transaction};
pub use value::Value;

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_display() {
        assert_eq!(format!("{}", Value::Null), "NULL");
        assert_eq!(format!("{}", Value::Bool(true)), "true");
        assert_eq!(format!("{}", Value::I64(42)), "42");
        assert_eq!(format!("{}", Value::F64(2.71)), "2.71");
        assert_eq!(format!("{}", Value::Text("hello".into())), "hello");
        assert_eq!(format!("{}", Value::Blob(vec![1, 2, 3])), "<blob:3 bytes>");
        assert_eq!(format!("{}", Value::Decimal("123.456".into())), "123.456");
        assert_eq!(
            format!("{}", Value::Json(r#"{"key":"val"}"#.into())),
            r#"{"key":"val"}"#
        );
        assert_eq!(
            format!("{}", Value::Array(vec![Value::I64(1), Value::I64(2)])),
            "[1, 2]"
        );
    }

    #[test]
    fn value_type_name() {
        assert_eq!(Value::Null.type_name(), "Null");
        assert_eq!(Value::Bool(true).type_name(), "Bool");
        assert_eq!(Value::I64(1).type_name(), "I64");
        assert_eq!(Value::F64(1.0).type_name(), "F64");
        assert_eq!(Value::Text("x".into()).type_name(), "Text");
        assert_eq!(Value::Blob(vec![]).type_name(), "Blob");
        assert_eq!(Value::Timestamp(0).type_name(), "Timestamp");
        assert_eq!(Value::Date(0).type_name(), "Date");
        assert_eq!(Value::Time(0).type_name(), "Time");
        assert_eq!(Value::Uuid(0).type_name(), "Uuid");
        assert_eq!(Value::Json("{}".into()).type_name(), "Json");
        assert_eq!(Value::Decimal("0".into()).type_name(), "Decimal");
        assert_eq!(Value::Array(vec![]).type_name(), "Array");
    }

    #[test]
    fn value_from_impls() {
        assert_eq!(Value::from(true), Value::Bool(true));
        assert_eq!(Value::from(42i32), Value::I64(42));
        assert_eq!(Value::from(42i64), Value::I64(42));
        assert_eq!(Value::from(2.71f64), Value::F64(2.71));
        assert_eq!(Value::from("hello"), Value::Text("hello".into()));
        assert_eq!(
            Value::from("hello".to_string()),
            Value::Text("hello".into())
        );
        assert_eq!(Value::from(vec![1u8, 2, 3]), Value::Blob(vec![1, 2, 3]));
        assert_eq!(Value::from(None::<i64>), Value::Null);
        assert_eq!(Value::from(Some(42i64)), Value::I64(42));
    }

    #[test]
    fn value_partial_ord() {
        assert!(Value::Null < Value::I64(0));
        assert!(Value::I64(1) < Value::I64(2));
        assert!(Value::Text("a".into()) < Value::Text("b".into()));
        assert!(Value::Bool(false) < Value::Bool(true));
        // Cross-type comparison returns None
        assert_eq!(Value::I64(1).partial_cmp(&Value::Text("1".into())), None);
    }

    #[test]
    fn from_value_basic() {
        assert_eq!(bool::from_value(&Value::Bool(true)).ok(), Some(true));
        assert_eq!(i64::from_value(&Value::I64(42)).ok(), Some(42i64));
        assert_eq!(i32::from_value(&Value::I64(42)).ok(), Some(42i32));
        assert_eq!(f64::from_value(&Value::F64(2.71)).ok(), Some(2.71));
        assert_eq!(
            String::from_value(&Value::Text("hi".into())).ok(),
            Some("hi".to_string())
        );
        assert_eq!(
            Vec::<u8>::from_value(&Value::Blob(vec![1])).ok(),
            Some(vec![1u8])
        );
    }

    #[test]
    fn from_value_option() {
        assert_eq!(Option::<i64>::from_value(&Value::Null).ok(), Some(None));
        assert_eq!(
            Option::<i64>::from_value(&Value::I64(42)).ok(),
            Some(Some(42))
        );
    }

    #[test]
    fn from_value_type_mismatch() {
        assert!(i64::from_value(&Value::Text("x".into())).is_err());
        assert!(bool::from_value(&Value::I64(1)).is_err());
        assert!(String::from_value(&Value::I64(1)).is_err());
    }

    #[test]
    fn row_try_get() {
        let row = Row::new(
            vec!["id".into(), "name".into(), "empty".into()],
            vec![Value::I64(42), Value::Text("Alice".into()), Value::Null],
        );
        assert_eq!(row.try_get::<i64>("id").ok(), Some(42));
        assert_eq!(
            row.try_get::<String>("name").ok(),
            Some("Alice".to_string())
        );
        assert_eq!(row.try_get::<Option<i64>>("empty").ok(), Some(None));
        assert!(row.try_get::<i64>("nonexistent").is_err());
    }

    #[test]
    fn row_column_count_and_is_null() {
        let row = Row::new(
            vec!["a".into(), "b".into()],
            vec![Value::I64(1), Value::Null],
        );
        assert_eq!(row.column_count(), 2);
        assert!(!row.is_null("a"));
        assert!(row.is_null("b"));
        assert!(!row.is_null("nonexistent"));
    }

    #[test]
    fn row_display() {
        let row = Row::new(
            vec!["id".into(), "name".into()],
            vec![Value::I64(1), Value::Text("Alice".into())],
        );
        assert_eq!(format!("{row}"), "{id: 1, name: Alice}");
    }

    #[test]
    fn row_into_values() {
        let row = Row::new(vec!["x".into()], vec![Value::I64(99)]);
        let vals = row.into_values();
        assert_eq!(vals, vec![Value::I64(99)]);
    }

    #[test]
    fn row_o1_lookup() {
        let cols: Vec<String> = (0..100).map(|i| format!("col{i}")).collect();
        let vals: Vec<Value> = (0..100).map(Value::I64).collect();
        let row = Row::new(cols, vals);
        assert_eq!(row.try_get::<i64>("col0").ok(), Some(0));
        assert_eq!(row.try_get::<i64>("col99").ok(), Some(99));
        assert_eq!(row.try_get::<i64>("col50").ok(), Some(50));
        assert!(row.try_get::<i64>("nonexistent").is_err());
    }

    #[test]
    fn rowset_basic() {
        let rows = vec![
            Row::new(vec!["a".into()], vec![Value::I64(1)]),
            Row::new(vec!["a".into()], vec![Value::I64(2)]),
        ];
        let rs = RowSet::from_rows(rows);
        assert_eq!(rs.len(), 2);
        assert_eq!(rs.column_count(), 1);
        assert!(!rs.is_empty());
        assert_eq!(rs.columns(), &["a".to_string()]);
    }

    #[test]
    fn rowset_empty() {
        let rs = RowSet::from_rows(vec![]);
        assert!(rs.is_empty());
        assert_eq!(rs.column_count(), 0);
    }

    #[test]
    fn error_display() {
        assert_eq!(
            format!("{}", OxiSqlError::Parse("bad sql".into())),
            "SQL parse error: bad sql"
        );
        assert_eq!(
            format!("{}", OxiSqlError::ConstraintViolation("unique key".into())),
            "constraint violation: unique key"
        );
        assert_eq!(
            format!("{}", OxiSqlError::Timeout("5s".into())),
            "timeout: 5s"
        );
        assert_eq!(
            format!("{}", OxiSqlError::ConnectionPool("exhausted".into())),
            "connection pool error: exhausted"
        );
        assert_eq!(
            format!("{}", OxiSqlError::Migration("failed".into())),
            "migration error: failed"
        );
    }

    #[test]
    fn uuid_display() {
        // Zero UUID
        assert_eq!(
            format!("{}", Value::Uuid(0)),
            "00000000-0000-0000-0000-000000000000"
        );
    }

    #[test]
    fn time_display() {
        // 01:02:03
        let us = (3600 + 2 * 60 + 3) * 1_000_000i64;
        assert_eq!(format!("{}", Value::Time(us)), "01:02:03");
        // 01:02:03.000042
        let us2 = us + 42;
        assert_eq!(format!("{}", Value::Time(us2)), "01:02:03.000042");
    }

    #[test]
    fn f64_from_i64_coercion() {
        // FromValue for f64 should accept I64 values
        assert_eq!(f64::from_value(&Value::I64(42)).ok(), Some(42.0));
    }

    #[test]
    fn string_from_json_and_decimal() {
        // FromValue for String should accept Json and Decimal
        assert_eq!(
            String::from_value(&Value::Json(r#"{"a":1}"#.into())).ok(),
            Some(r#"{"a":1}"#.to_string())
        );
        assert_eq!(
            String::from_value(&Value::Decimal("1.23".into())).ok(),
            Some("1.23".to_string())
        );
    }

    // ── ToSqlValue tests ─────────────────────────────────────────────────────

    #[test]
    fn to_sql_value_i64() {
        assert_eq!(42i64.to_value(), Value::I64(42));
        assert_eq!((-1i64).to_value(), Value::I64(-1));
        assert_eq!(i64::MAX.to_value(), Value::I64(i64::MAX));
    }

    #[test]
    fn to_sql_value_i32() {
        assert_eq!(100i32.to_value(), Value::I64(100));
        assert_eq!((-5i32).to_value(), Value::I64(-5));
    }

    #[test]
    fn to_sql_value_f64() {
        assert_eq!(1.5f64.to_value(), Value::F64(1.5));
        assert_eq!(0.0f64.to_value(), Value::F64(0.0));
    }

    #[test]
    fn to_sql_value_bool() {
        assert_eq!(true.to_value(), Value::Bool(true));
        assert_eq!(false.to_value(), Value::Bool(false));
    }

    #[test]
    fn to_sql_value_str() {
        assert_eq!("hello".to_value(), Value::Text("hello".into()));
        assert_eq!("".to_value(), Value::Text(String::new()));
    }

    #[test]
    fn to_sql_value_string() {
        assert_eq!("world".to_string().to_value(), Value::Text("world".into()));
    }

    #[test]
    fn to_sql_value_bytes() {
        assert_eq!(vec![1u8, 2, 3].to_value(), Value::Blob(vec![1, 2, 3]));
        assert_eq!(Vec::<u8>::new().to_value(), Value::Blob(vec![]));
    }

    #[test]
    fn to_sql_value_option_some_and_none() {
        assert_eq!(Some(99i64).to_value(), Value::I64(99));
        assert_eq!(None::<i64>.to_value(), Value::Null);
    }

    #[test]
    fn to_sql_value_ref_passthrough() {
        // &T also implements ToSqlValue via blanket impl
        let n: i64 = 7;
        assert_eq!(n.to_value(), Value::I64(7));
        assert_eq!((&"txt").to_value(), Value::Text("txt".into()));
    }

    // ── FromValue edge-case tests ────────────────────────────────────────────

    #[test]
    fn from_value_i32_range_check() {
        // i64 within i32 range succeeds
        assert_eq!(i32::from_value(&Value::I64(100)).ok(), Some(100i32));
        // i64 out of i32 range fails
        assert!(i32::from_value(&Value::I64(i64::MAX)).is_err());
        assert!(i32::from_value(&Value::I64(i64::MIN)).is_err());
    }

    #[test]
    fn from_value_uuid_as_string() {
        // UUID should format as hyphenated string via String::from_value
        let uuid_val = Value::Uuid(0u128);
        let s = String::from_value(&uuid_val).expect("uuid as string");
        assert!(s.contains('-'), "UUID string should contain hyphens: {s}");
        assert_eq!(s, "00000000-0000-0000-0000-000000000000");
    }

    #[test]
    fn from_value_uuid_nonzero_as_string() {
        // A known non-zero UUID value
        let uuid_val = Value::Uuid(0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128);
        let s = String::from_value(&uuid_val).expect("uuid as string");
        assert!(s.contains('-'), "UUID string should contain hyphens: {s}");
    }

    #[test]
    fn from_value_null_non_option_fails() {
        // Non-Option types should fail on Null
        assert!(i64::from_value(&Value::Null).is_err());
        assert!(bool::from_value(&Value::Null).is_err());
        assert!(String::from_value(&Value::Null).is_err());
        assert!(f64::from_value(&Value::Null).is_err());
    }

    #[test]
    fn from_value_u128_from_uuid() {
        let val = Value::Uuid(12345678u128);
        assert_eq!(u128::from_value(&val).ok(), Some(12345678u128));
    }

    #[test]
    fn from_value_u128_wrong_type_fails() {
        assert!(u128::from_value(&Value::I64(1)).is_err());
    }

    #[test]
    fn from_value_blob_wrong_type_fails() {
        assert!(Vec::<u8>::from_value(&Value::Text("x".into())).is_err());
    }

    #[test]
    fn from_value_f64_from_i64_boundary() {
        // Exact integer values round-trip through f64 correctly
        assert_eq!(f64::from_value(&Value::I64(0)).ok(), Some(0.0f64));
        assert_eq!(f64::from_value(&Value::I64(-1)).ok(), Some(-1.0f64));
    }

    // ── QueryBuilder additional tests ────────────────────────────────────────

    #[test]
    fn select_builder_basic() {
        let q = SelectBuilder::new()
            .columns(&["id", "name"])
            .from("users")
            .build();
        assert!(q.sql.contains("SELECT"));
        assert!(q.sql.contains("users"));
        assert!(q.sql.contains("id"));
        assert!(q.sql.contains("name"));
    }

    #[test]
    fn select_builder_with_where_eq() {
        let q = SelectBuilder::new()
            .from("users")
            .where_eq("id", &42i64)
            .build();
        assert!(q.sql.contains("WHERE"));
        assert!(!q.params.is_empty());
        assert_eq!(q.params[0], Value::I64(42));
    }

    #[test]
    fn insert_builder_basic() {
        let q = InsertBuilder::new()
            .into_table("users")
            .column("name", &"Alice")
            .column("age", &30i64)
            .build();
        assert!(q.sql.to_uppercase().contains("INSERT"));
        assert!(q.sql.contains("users"));
        assert_eq!(q.params.len(), 2);
    }

    #[test]
    fn update_builder_basic() {
        let q = UpdateBuilder::new()
            .table("users")
            .set("name", &"Bob")
            .where_eq("id", &1i64)
            .build();
        assert!(q.sql.to_uppercase().contains("UPDATE"));
        assert!(q.sql.contains("users"));
        assert_eq!(q.params.len(), 2);
    }

    #[test]
    fn delete_builder_basic() {
        let q = DeleteBuilder::new()
            .from("users")
            .where_raw("id = 1")
            .build();
        assert!(q.sql.to_uppercase().contains("DELETE"));
        assert!(q.sql.contains("users"));
    }

    // ── OxiSqlError Display — all variants ──────────────────────────────────

    #[test]
    fn oxisql_error_display_all_variants() {
        let cases: Vec<(OxiSqlError, &str)> = vec![
            (OxiSqlError::NotConnected, "not connected"),
            (OxiSqlError::Execution("oops".into()), "oops"),
            (OxiSqlError::Timeout("30s".into()), "30s"),
            (OxiSqlError::Parse("bad sql".into()), "bad sql"),
            (
                OxiSqlError::ConstraintViolation("unique key".into()),
                "unique key",
            ),
            (OxiSqlError::ConnectionPool("exhausted".into()), "exhausted"),
            (OxiSqlError::Migration("failed".into()), "failed"),
            (OxiSqlError::Other("something".into()), "something"),
            (
                OxiSqlError::TypeMismatch {
                    expected: "I64",
                    got: "Text",
                },
                "i64",
            ),
        ];
        for (err, fragment) in cases {
            let s = err.to_string().to_lowercase();
            assert!(
                !s.is_empty(),
                "error should display non-empty string for {err:?}"
            );
            assert!(
                s.contains(fragment),
                "expected fragment '{fragment}' not found in error display: '{s}'"
            );
        }
    }

    // ── TypeRegistry tests ───────────────────────────────────────────────────

    #[test]
    fn type_registry_lookup_standard_types() {
        let reg = TypeRegistry::new();
        assert!(reg.lookup("INTEGER").is_some());
        assert!(reg.lookup("TEXT").is_some());
        assert!(reg.lookup("BOOLEAN").is_some());
        assert!(reg.lookup("UUID").is_some());
        assert!(reg.lookup("TIMESTAMP").is_some());
        assert!(reg.lookup("DATE").is_some());
        assert!(reg.lookup("TIME").is_some());
        assert!(reg.lookup("JSON").is_some());
        assert!(reg.lookup("DECIMAL").is_some());
        assert!(reg.lookup("NONEXISTENT_TYPE").is_none());
    }

    #[test]
    fn type_registry_lookup_case_insensitive() {
        let reg = TypeRegistry::new();
        assert!(reg.lookup("integer").is_some());
        assert!(reg.lookup("Integer").is_some());
        assert!(reg.lookup("TEXT").is_some());
        assert!(reg.lookup("text").is_some());
    }

    #[test]
    fn type_registry_lookup_aliases() {
        let reg = TypeRegistry::new();
        // INT, INT4 → Integer
        assert_eq!(reg.lookup("INT"), Some(&SqlType::Integer));
        assert_eq!(reg.lookup("INT4"), Some(&SqlType::Integer));
        // INT8, BIGINT → BigInt
        assert_eq!(reg.lookup("BIGINT"), Some(&SqlType::BigInt));
        assert_eq!(reg.lookup("INT8"), Some(&SqlType::BigInt));
        // INT2, SMALLINT → SmallInt
        assert_eq!(reg.lookup("SMALLINT"), Some(&SqlType::SmallInt));
        assert_eq!(reg.lookup("INT2"), Some(&SqlType::SmallInt));
        // FLOAT, FLOAT4, REAL → Float
        assert_eq!(reg.lookup("FLOAT"), Some(&SqlType::Float));
        assert_eq!(reg.lookup("REAL"), Some(&SqlType::Float));
        // DOUBLE, FLOAT8 → Double
        assert_eq!(reg.lookup("DOUBLE"), Some(&SqlType::Double));
        assert_eq!(reg.lookup("FLOAT8"), Some(&SqlType::Double));
        // NUMERIC → Decimal
        assert_eq!(reg.lookup("NUMERIC"), Some(&SqlType::Decimal));
        // BOOL → Boolean
        assert_eq!(reg.lookup("BOOL"), Some(&SqlType::Boolean));
        // JSONB → Json
        assert_eq!(reg.lookup("JSONB"), Some(&SqlType::Json));
        // BYTEA, BLOB → Blob
        assert_eq!(reg.lookup("BYTEA"), Some(&SqlType::Blob));
        assert_eq!(reg.lookup("BLOB"), Some(&SqlType::Blob));
        // TIMESTAMPTZ → Timestamp
        assert_eq!(reg.lookup("TIMESTAMPTZ"), Some(&SqlType::Timestamp));
        // TIMETZ → Time
        assert_eq!(reg.lookup("TIMETZ"), Some(&SqlType::Time));
    }

    #[test]
    fn type_registry_default_values() {
        let reg = TypeRegistry::new();
        assert_eq!(reg.default_value_for("INTEGER"), Value::I64(0));
        assert_eq!(reg.default_value_for("BIGINT"), Value::I64(0));
        assert_eq!(reg.default_value_for("SMALLINT"), Value::I64(0));
        assert_eq!(reg.default_value_for("TEXT"), Value::Text(String::new()));
        assert_eq!(reg.default_value_for("VARCHAR"), Value::Text(String::new()));
        assert_eq!(reg.default_value_for("BOOLEAN"), Value::Bool(false));
        assert_eq!(reg.default_value_for("TIMESTAMP"), Value::Timestamp(0));
        assert_eq!(reg.default_value_for("DATE"), Value::Date(0));
        assert_eq!(reg.default_value_for("TIME"), Value::Time(0));
        assert_eq!(reg.default_value_for("UUID"), Value::Uuid(0));
        assert_eq!(reg.default_value_for("JSON"), Value::Json("{}".into()));
        assert_eq!(reg.default_value_for("DECIMAL"), Value::Decimal("0".into()));
        assert_eq!(reg.default_value_for("BYTEA"), Value::Blob(Vec::new()));
        // Unknown type → Null
        assert_eq!(reg.default_value_for("UNKNOWN_TYPE"), Value::Null);
    }

    #[test]
    fn type_registry_register_custom() {
        let mut reg = TypeRegistry::new();
        reg.register("MY_CUSTOM_TYPE", SqlType::Text);
        assert!(reg.lookup("MY_CUSTOM_TYPE").is_some());
        // Case-insensitive
        assert!(reg.lookup("my_custom_type").is_some());
    }

    #[test]
    fn type_registry_register_overrides_existing() {
        let mut reg = TypeRegistry::new();
        // Overwrite INTEGER to map to BigInt
        reg.register("INTEGER", SqlType::BigInt);
        assert_eq!(reg.lookup("INTEGER"), Some(&SqlType::BigInt));
    }

    #[test]
    fn type_registry_default() {
        // Default impl delegates to new()
        let reg = TypeRegistry::default();
        assert!(reg.lookup("TEXT").is_some());
    }

    #[test]
    fn value_matches_type_basic() {
        assert!(TypeRegistry::value_matches_type(
            &Value::I64(1),
            &SqlType::Integer
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::I64(1),
            &SqlType::BigInt
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::I64(1),
            &SqlType::SmallInt
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::F64(1.0),
            &SqlType::Float
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::F64(1.0),
            &SqlType::Double
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Text("x".into()),
            &SqlType::Text
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Text("x".into()),
            &SqlType::VarChar(Some(255))
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Bool(true),
            &SqlType::Boolean
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Uuid(0),
            &SqlType::Uuid
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Json("{}".into()),
            &SqlType::Json
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Decimal("1.5".into()),
            &SqlType::Decimal
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Blob(vec![]),
            &SqlType::Blob
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Timestamp(0),
            &SqlType::Timestamp
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Date(0),
            &SqlType::Date
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Time(0),
            &SqlType::Time
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Array(vec![]),
            &SqlType::Array(Box::new(SqlType::Integer))
        ));
    }

    #[test]
    fn value_matches_type_mismatches() {
        assert!(!TypeRegistry::value_matches_type(
            &Value::Text("x".into()),
            &SqlType::Integer
        ));
        assert!(!TypeRegistry::value_matches_type(
            &Value::I64(1),
            &SqlType::Text
        ));
        assert!(!TypeRegistry::value_matches_type(
            &Value::Bool(true),
            &SqlType::Integer
        ));
        assert!(!TypeRegistry::value_matches_type(
            &Value::F64(1.0),
            &SqlType::Integer
        ));
    }

    #[test]
    fn value_matches_type_null_valid_for_any() {
        // NULL is valid for any SQL type
        assert!(TypeRegistry::value_matches_type(
            &Value::Null,
            &SqlType::Integer
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Null,
            &SqlType::Text
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Null,
            &SqlType::Boolean
        ));
        assert!(TypeRegistry::value_matches_type(
            &Value::Null,
            &SqlType::Uuid
        ));
    }

    #[test]
    fn sql_type_as_sql_name() {
        assert_eq!(SqlType::Integer.as_sql_name(), "INTEGER");
        assert_eq!(SqlType::BigInt.as_sql_name(), "BIGINT");
        assert_eq!(SqlType::SmallInt.as_sql_name(), "SMALLINT");
        assert_eq!(SqlType::Float.as_sql_name(), "REAL");
        assert_eq!(SqlType::Double.as_sql_name(), "DOUBLE PRECISION");
        assert_eq!(SqlType::Decimal.as_sql_name(), "DECIMAL");
        assert_eq!(SqlType::Text.as_sql_name(), "TEXT");
        assert_eq!(SqlType::VarChar(None).as_sql_name(), "VARCHAR");
        assert_eq!(SqlType::VarChar(Some(255)).as_sql_name(), "VARCHAR(255)");
        assert_eq!(SqlType::Blob.as_sql_name(), "BYTEA");
        assert_eq!(SqlType::Boolean.as_sql_name(), "BOOLEAN");
        assert_eq!(SqlType::Timestamp.as_sql_name(), "TIMESTAMP");
        assert_eq!(SqlType::Date.as_sql_name(), "DATE");
        assert_eq!(SqlType::Time.as_sql_name(), "TIME");
        assert_eq!(SqlType::Uuid.as_sql_name(), "UUID");
        assert_eq!(SqlType::Json.as_sql_name(), "JSON");
        assert_eq!(
            SqlType::Array(Box::new(SqlType::Integer)).as_sql_name(),
            "INTEGER[]"
        );
        assert_eq!(SqlType::Unknown("MYTYPE".into()).as_sql_name(), "MYTYPE");
    }

    #[test]
    fn sql_type_default_value_array_and_unknown() {
        assert_eq!(
            SqlType::Array(Box::new(SqlType::Integer)).default_value(),
            Value::Array(Vec::new())
        );
        assert_eq!(
            SqlType::Unknown("CUSTOM".into()).default_value(),
            Value::Null
        );
    }

    // ── Cursor tests ─────────────────────────────────────────────────────────

    #[test]
    fn cursor_basic_traversal() {
        let rows = vec![
            Row::new(vec!["id".into()], vec![Value::I64(1)]),
            Row::new(vec!["id".into()], vec![Value::I64(2)]),
        ];
        let mut cursor = Cursor::new(rows);
        assert_eq!(cursor.len(), 2);
        assert_eq!(cursor.remaining(), 2);
        assert!(!cursor.is_empty());

        let r1 = cursor.advance().expect("first row");
        assert_eq!(r1.try_get::<i64>("id").unwrap(), 1);
        assert_eq!(cursor.position(), 1);
        assert_eq!(cursor.remaining(), 1);

        let r2 = cursor.advance().expect("second row");
        assert_eq!(r2.try_get::<i64>("id").unwrap(), 2);
        assert_eq!(cursor.position(), 2);
        assert_eq!(cursor.remaining(), 0);

        assert!(cursor.advance().is_none());
    }

    #[test]
    fn cursor_peek_does_not_advance() {
        let rows = vec![Row::new(vec!["x".into()], vec![Value::I64(42)])];
        let cursor = Cursor::new(rows);
        let peeked = cursor.peek().expect("peek first row");
        assert_eq!(peeked.try_get::<i64>("x").unwrap(), 42);
        // position must still be 0
        assert_eq!(cursor.position(), 0);
    }

    #[test]
    fn cursor_reset() {
        let rows = vec![Row::new(vec!["x".into()], vec![Value::I64(1)])];
        let mut cursor = Cursor::new(rows);
        cursor.advance();
        assert_eq!(cursor.position(), 1);
        cursor.reset();
        assert_eq!(cursor.position(), 0);
        assert_eq!(cursor.remaining(), 1);
        assert!(cursor.advance().is_some());
    }

    #[test]
    fn cursor_skip_by() {
        let rows: Vec<Row> = (0..5)
            .map(|i| Row::new(vec!["n".into()], vec![Value::I64(i)]))
            .collect();
        let mut cursor = Cursor::new(rows);
        cursor.skip_by(3);
        assert_eq!(cursor.position(), 3);
        assert_eq!(cursor.remaining(), 2);
        // skip_by past end is clamped
        cursor.skip_by(100);
        assert_eq!(cursor.position(), 5);
        assert_eq!(cursor.remaining(), 0);
        assert!(cursor.advance().is_none());
    }

    #[test]
    fn cursor_into_rows_recovers_all() {
        let rows = vec![
            Row::new(vec!["v".into()], vec![Value::I64(10)]),
            Row::new(vec!["v".into()], vec![Value::I64(20)]),
        ];
        let mut cursor = Cursor::new(rows.clone());
        cursor.advance(); // consume one
        let recovered = cursor.into_rows();
        // into_rows returns the full underlying vec, not just remaining
        assert_eq!(recovered.len(), 2);
    }

    #[test]
    fn cursor_empty() {
        let mut cursor = Cursor::new(vec![]);
        assert!(cursor.is_empty());
        assert_eq!(cursor.len(), 0);
        assert_eq!(cursor.remaining(), 0);
        assert!(cursor.peek().is_none());
        assert!(cursor.advance().is_none());
    }

    #[test]
    fn cursor_iterator_yields_owned_rows() {
        let rows: Vec<Row> = (1..=3)
            .map(|i| Row::new(vec!["i".into()], vec![Value::I64(i)]))
            .collect();
        let cursor = Cursor::new(rows);
        let collected: Vec<Row> = cursor.collect();
        assert_eq!(collected.len(), 3);
        assert_eq!(collected[0].try_get::<i64>("i").unwrap(), 1);
        assert_eq!(collected[2].try_get::<i64>("i").unwrap(), 3);
    }

    #[test]
    fn cursor_iterator_and_advance_independent() {
        // Iterator::next and advance() both advance the same position counter.
        let rows = vec![
            Row::new(vec!["k".into()], vec![Value::I64(10)]),
            Row::new(vec!["k".into()], vec![Value::I64(20)]),
            Row::new(vec!["k".into()], vec![Value::I64(30)]),
        ];
        let mut cursor = Cursor::new(rows);
        // Consume first via Iterator
        let owned = Iterator::next(&mut cursor).expect("first via Iterator");
        assert_eq!(owned.try_get::<i64>("k").unwrap(), 10);
        // Consume second via advance (borrow)
        let borrowed = cursor.advance().expect("second via advance");
        assert_eq!(borrowed.try_get::<i64>("k").unwrap(), 20);
        // Only the third should remain
        assert_eq!(cursor.remaining(), 1);
    }
}
