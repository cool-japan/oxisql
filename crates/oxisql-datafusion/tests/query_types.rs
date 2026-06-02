//! Type-specific integration tests for `oxisql-datafusion`:
//! - `Decimal128` column building
//! - `List(Utf8)` array columns
//! - Schema mismatch and null handling
//! - Wave 9 NULL / schema tests

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use oxisql_core::{Row, Value};
use oxisql_datafusion::OxiSqlTableProvider;

/// `Decimal128` column builder parses decimal strings into scaled `i128`.
#[tokio::test]
async fn test_decimal128_column_builder() {
    use arrow::datatypes::Schema;
    use oxisql_core::{Row, Value};
    use oxisql_datafusion::types::rows_to_record_batch;

    let schema = Arc::new(Schema::new(vec![Field::new(
        "price",
        DataType::Decimal128(18, 2),
        true,
    )]));

    let rows = vec![
        Row::new(
            vec!["price".to_string()],
            vec![Value::Decimal("123.45".to_string())],
        ),
        Row::new(
            vec!["price".to_string()],
            vec![Value::Decimal("0.99".to_string())],
        ),
        Row::new(vec!["price".to_string()], vec![Value::Null]),
    ];

    let batch = rows_to_record_batch(rows, schema).expect("should succeed");
    assert_eq!(batch.num_rows(), 3);
    assert!(batch.column(0).is_null(2), "third row should be NULL");
}

/// `List(Utf8)` column builder encodes `Value::Array` into Arrow list arrays.
#[tokio::test]
async fn test_array_list_column() {
    use arrow::datatypes::Schema;
    use oxisql_core::{Row, Value};
    use oxisql_datafusion::types::rows_to_record_batch;

    let item_field = Arc::new(Field::new("item", DataType::Utf8, true));
    let schema = Arc::new(Schema::new(vec![Field::new(
        "tags",
        DataType::List(item_field),
        true,
    )]));

    let rows = vec![
        Row::new(
            vec!["tags".to_string()],
            vec![Value::Array(vec![
                Value::Text("a".to_string()),
                Value::Text("b".to_string()),
            ])],
        ),
        Row::new(vec!["tags".to_string()], vec![Value::Null]),
    ];

    let batch = rows_to_record_batch(rows, schema).expect("should succeed");
    assert_eq!(batch.num_rows(), 2);
    assert!(!batch.column(0).is_null(0), "first row should not be null");
    assert!(batch.column(0).is_null(1), "second row should be NULL");
}

/// Nullable column: NULL values propagate through Arrow arrays.
///
/// A table with a nullable `Int64` column is queried with `IS NOT NULL` to
/// verify that only the non-null row is returned.
#[tokio::test]
async fn test_null_handling() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("val", DataType::Int64, true),
    ]));

    let cols: Vec<String> = vec!["id".into(), "val".into()];
    let rows = vec![
        Row::new(cols.clone(), vec![Value::I64(1), Value::I64(100)]),
        Row::new(cols, vec![Value::I64(2), Value::Null]),
    ];

    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("null_test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id FROM null_test WHERE val IS NOT NULL")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "IS NOT NULL should return exactly 1 row");
}

/// Schema mismatch: a row with fewer values than schema fields gracefully
/// produces NULLs in the missing column positions.
///
/// `build_column` uses `get_by_index` which returns `None` for out-of-range
/// indices, and every builder arm maps `None` to `append_null()`.  The schema
/// fields must be declared nullable (`true`) so Arrow accepts the null values.
#[test]
fn test_schema_mismatch_detection() {
    use oxisql_datafusion::types::rows_to_record_batch;

    // Both columns must be nullable=true — `rows_to_record_batch` fills
    // missing positions with NULL, which Arrow rejects for non-nullable fields.
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, true),
        Field::new("name", DataType::Utf8, true),
    ]));

    // Row has only 1 value but schema has 2 fields — missing "name" → NULL.
    let rows = vec![Row::new(vec!["id".into()], vec![Value::I64(1)])];
    let batch = rows_to_record_batch(rows, schema).expect("should succeed with nulls");
    assert_eq!(batch.num_rows(), 1);
    assert_eq!(batch.num_columns(), 2);
    // The "name" column (index 1) should be NULL for the only row.
    assert!(
        batch.column(1).is_null(0),
        "missing value should produce a null in the name column"
    );
}

/// `Decimal128` column built from `Value::Decimal` is correctly queried
/// through a DataFusion `SessionContext`.
#[tokio::test]
async fn test_extended_types_date_decimal() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "amount",
        DataType::Decimal128(18, 4),
        true,
    )]));

    let rows = vec![Row::new(
        vec!["amount".into()],
        vec![Value::Decimal("123.4567".into())],
    )];

    let provider = OxiSqlTableProvider::from_rows(rows, schema.clone());
    let ctx = SessionContext::new();
    ctx.register_table("decimals", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT amount FROM decimals")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    assert!(!batches.is_empty(), "should return at least one batch");
}
