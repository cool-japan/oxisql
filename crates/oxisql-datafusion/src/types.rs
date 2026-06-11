//! Mapping from `oxisql_core::Value` / `oxisql_core::Row` to Apache Arrow types.
//!
//! Supports both basic types (Bool, I64, F64, Text, Blob) and extended
//! OxiSQL types (Timestamp, Date, Uuid, Json, Decimal, Array).

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, Date64Builder, Decimal128Builder,
    Float64Builder, Int64Builder, ListBuilder, StringBuilder, TimestampMicrosecondBuilder,
    TimestampMillisecondBuilder,
};
use arrow::datatypes::{DataType, Field, SchemaRef, TimeUnit};
use arrow::record_batch::RecordBatch;
use oxisql_core::{Row, Value};

use crate::error::OxiSqlFusionError;

/// Map an `oxisql_core::Value` to the equivalent Arrow [`DataType`].
///
/// Returns `None` for [`Value::Null`] since null values do not determine a
/// column type.
pub fn value_to_arrow_type(value: &Value) -> Option<DataType> {
    match value {
        Value::Null => None,
        Value::Bool(_) => Some(DataType::Boolean),
        Value::I64(_) => Some(DataType::Int64),
        Value::F64(_) => Some(DataType::Float64),
        Value::Text(_) => Some(DataType::Utf8),
        Value::Blob(_) => Some(DataType::LargeBinary),
        Value::Timestamp(_) => Some(DataType::Timestamp(TimeUnit::Microsecond, None)),
        Value::Date(_) => Some(DataType::Date32),
        Value::Time(_) => Some(DataType::Int64), // microseconds since midnight
        Value::Uuid(_) => Some(DataType::Utf8),  // UUID as string
        Value::Json(_) => Some(DataType::Utf8),  // JSON as string
        Value::Decimal(_) => Some(DataType::Utf8), // Decimal as string
        Value::Array(_) => Some(DataType::List(Arc::new(Field::new(
            "item",
            DataType::Utf8,
            true,
        )))),
        // TypedArray carries an element-type hint from the backend but is stored
        // the same way as an untyped Array at the Arrow layer.
        Value::TypedArray { .. } => Some(DataType::List(Arc::new(Field::new(
            "item",
            DataType::Utf8,
            true,
        )))),
    }
}

/// Parse a decimal string (e.g., `"123.45"`) into an `i128` scaled by
/// `10^scale`.
///
/// Returns `None` if the string cannot be parsed as a valid decimal number.
fn parse_decimal_to_i128(s: &str, scale: i8) -> Option<i128> {
    let s = s.trim();
    let scale_u32 = scale.unsigned_abs() as u32;
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let frac_part = &s[dot + 1..];
        let frac_len = frac_part.len();
        let scale_usize = scale_u32 as usize;
        // Pad or truncate fraction to exactly `scale` digits.
        let frac_scaled: String = if frac_len < scale_usize {
            format!("{frac_part}{:0>pad$}", "", pad = scale_usize - frac_len)
        } else {
            frac_part[..scale_usize].to_string()
        };
        let combined = format!("{int_part}{frac_scaled}");
        combined.parse::<i128>().ok()
    } else {
        // No decimal point — multiply by 10^scale.
        let n: i128 = s.parse().ok()?;
        Some(n * 10i128.pow(scale_u32))
    }
}

/// Build an `ArrayRef` for column `col_idx` across all `rows`, using the
/// Arrow `DataType` declared in `schema` for that column.
///
/// All column arrays are built as *nullable* so that `Value::Null` can always
/// be represented without loss of information.
fn build_column(
    rows: &[Row],
    col_idx: usize,
    dtype: &DataType,
) -> Result<ArrayRef, OxiSqlFusionError> {
    match dtype {
        DataType::Boolean => {
            let mut builder = BooleanBuilder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Bool(b)) => builder.append_value(*b),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Int64 => {
            let mut builder = Int64Builder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::I64(v)) => builder.append_value(*v),
                    Some(Value::Time(v)) => builder.append_value(*v), // Time as i64 micros
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Float64 => {
            let mut builder = Float64Builder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::F64(v)) => builder.append_value(*v),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Utf8 => {
            let mut builder = StringBuilder::with_capacity(rows.len(), rows.len() * 16);
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Text(s)) => builder.append_value(s.as_str()),
                    Some(Value::Json(s)) => builder.append_value(s.as_str()),
                    Some(Value::Decimal(s)) => builder.append_value(s.as_str()),
                    Some(Value::Uuid(u)) => {
                        builder.append_value(format!("{}", Value::Uuid(*u)).as_str());
                    }
                    Some(Value::Array(vals)) => {
                        builder.append_value(format!("{}", Value::Array(vals.clone())).as_str());
                    }
                    Some(Value::TypedArray { values, .. }) => {
                        builder.append_value(format!("{}", Value::Array(values.clone())).as_str());
                    }
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::LargeBinary => {
            let mut builder = BinaryBuilder::with_capacity(rows.len(), rows.len() * 32);
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Blob(b)) => builder.append_value(b.as_slice()),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let mut builder = TimestampMicrosecondBuilder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Timestamp(us)) => builder.append_value(*us),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Date32 => {
            let mut builder = Date32Builder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Date(d)) => builder.append_value(*d),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Date64 => {
            // Arrow `Date64` stores milliseconds since Unix epoch (must be
            // divisible by 86_400_000).  `Value::Date` stores days since
            // Unix epoch as `i32`, so we multiply by 86_400_000.
            let mut builder = Date64Builder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Date(d)) => {
                        builder.append_value(i64::from(*d) * 86_400_000);
                    }
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            // `Value::Timestamp` stores microseconds; divide by 1_000 to get
            // milliseconds for the `TimestampMillisecond` Arrow type.
            let mut builder = TimestampMillisecondBuilder::with_capacity(rows.len());
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Timestamp(us)) => builder.append_value(*us / 1_000),
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::Decimal128(precision, scale) => {
            let mut builder = Decimal128Builder::with_capacity(rows.len())
                .with_precision_and_scale(*precision, *scale)
                .map_err(OxiSqlFusionError::Arrow)?;
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Decimal(s)) | Some(Value::Text(s)) => {
                        match parse_decimal_to_i128(s, *scale) {
                            Some(v) => builder.append_value(v),
                            None => builder.append_null(),
                        }
                    }
                    Some(Value::I64(v)) => {
                        let scale_u32 = (*scale).unsigned_abs() as u32;
                        builder.append_value(i128::from(*v) * 10i128.pow(scale_u32));
                    }
                    Some(Value::Null) | None => builder.append_null(),
                    _ => builder.append_null(),
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        DataType::List(inner_field) if matches!(inner_field.data_type(), DataType::Utf8) => {
            let inner = StringBuilder::new();
            let mut builder = ListBuilder::new(inner);
            for row in rows {
                match row.get_by_index(col_idx) {
                    Some(Value::Array(vals)) | Some(Value::TypedArray { values: vals, .. }) => {
                        let values_builder = builder.values();
                        for v in vals {
                            if v.is_null() {
                                values_builder.append_null();
                            } else {
                                values_builder.append_value(format!("{v}"));
                            }
                        }
                        builder.append(true);
                    }
                    Some(Value::Null) | None => {
                        builder.append(false);
                    }
                    _ => {
                        builder.append(false);
                    }
                }
            }
            Ok(Arc::new(builder.finish()) as ArrayRef)
        }
        other => Err(OxiSqlFusionError::UnsupportedType(format!(
            "column {col_idx}: {other}"
        ))),
    }
}

/// Convert a `Vec<Row>` into a single [`RecordBatch`] using the provided
/// Arrow `schema`.
///
/// The schema must match the row layout: each field's `DataType` determines
/// how the corresponding column position is decoded from every [`Value`].
/// Rows with fewer values than schema fields will produce `null` in missing
/// positions.  Rows are **not** validated against the column names; callers
/// are expected to ensure the schema was constructed consistently with the
/// rows.
///
/// # Errors
///
/// Returns [`OxiSqlFusionError::Arrow`] if Arrow array construction or
/// `RecordBatch` assembly fails.
pub fn rows_to_record_batch(
    rows: Vec<Row>,
    schema: SchemaRef,
) -> Result<RecordBatch, OxiSqlFusionError> {
    let n_cols = schema.fields().len();
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(n_cols);

    for col_idx in 0..n_cols {
        let dtype = schema.field(col_idx).data_type();
        let array = build_column(&rows, col_idx, dtype)?;
        columns.push(array);
    }

    let batch = RecordBatch::try_new(schema, columns)?;
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{Field, Schema};

    fn make_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, true),
            Field::new("name", DataType::Utf8, true),
            Field::new("active", DataType::Boolean, true),
            Field::new("score", DataType::Float64, true),
        ]))
    }

    #[test]
    fn rows_to_batch_basic() {
        let schema = make_schema();
        let rows = vec![
            Row::new(
                vec![
                    "id".to_string(),
                    "name".to_string(),
                    "active".to_string(),
                    "score".to_string(),
                ],
                vec![
                    Value::I64(1),
                    Value::Text("Alice".to_string()),
                    Value::Bool(true),
                    Value::F64(95.5),
                ],
            ),
            Row::new(
                vec![
                    "id".to_string(),
                    "name".to_string(),
                    "active".to_string(),
                    "score".to_string(),
                ],
                vec![
                    Value::I64(2),
                    Value::Text("Bob".to_string()),
                    Value::Bool(false),
                    Value::Null,
                ],
            ),
        ];

        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 2);
        assert_eq!(batch.num_columns(), 4);
    }

    #[test]
    fn null_values_produce_null_array_entries() {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, true)]));
        let rows = vec![Row::new(vec!["x".to_string()], vec![Value::Null])];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        let col = batch.column(0);
        assert!(col.is_null(0));
    }

    #[test]
    fn empty_rows_produce_empty_batch() {
        let schema = make_schema();
        let batch = rows_to_record_batch(vec![], schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 0);
    }

    #[test]
    fn timestamp_column() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            true,
        )]));
        let rows = vec![Row::new(
            vec!["ts".to_string()],
            vec![Value::Timestamp(1_000_000)],
        )];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn date32_column() {
        let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::Date32, true)]));
        let rows = vec![Row::new(vec!["d".to_string()], vec![Value::Date(19000)])];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn date64_column() {
        let schema = Arc::new(Schema::new(vec![Field::new("d64", DataType::Date64, true)]));
        // Value::Date(1) == 1 day == 86_400_000 ms
        let rows = vec![Row::new(vec!["d64".to_string()], vec![Value::Date(1)])];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);

        use arrow::array::Date64Array;
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<Date64Array>()
            .expect("should be Date64Array");
        assert_eq!(col.value(0), 86_400_000_i64, "1 day = 86_400_000 ms");
    }

    #[test]
    fn timestamp_millisecond_column() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "ts_ms",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            true,
        )]));
        // Value::Timestamp stores microseconds; 2_000_000 us = 2_000 ms
        let rows = vec![Row::new(
            vec!["ts_ms".to_string()],
            vec![Value::Timestamp(2_000_000)],
        )];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);

        use arrow::array::TimestampMillisecondArray;
        let col = batch
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .expect("should be TimestampMillisecondArray");
        assert_eq!(col.value(0), 2_000_i64, "2_000_000 us = 2_000 ms");
    }

    #[test]
    fn json_as_utf8() {
        let schema = Arc::new(Schema::new(vec![Field::new("j", DataType::Utf8, true)]));
        let rows = vec![Row::new(
            vec!["j".to_string()],
            vec![Value::Json(r#"{"a":1}"#.to_string())],
        )];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn decimal_as_utf8() {
        let schema = Arc::new(Schema::new(vec![Field::new("d", DataType::Utf8, true)]));
        let rows = vec![Row::new(
            vec!["d".to_string()],
            vec![Value::Decimal("123.45".to_string())],
        )];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);
    }

    #[test]
    fn uuid_as_utf8() {
        let schema = Arc::new(Schema::new(vec![Field::new("u", DataType::Utf8, true)]));
        let rows = vec![Row::new(vec!["u".to_string()], vec![Value::Uuid(0)])];
        let batch = rows_to_record_batch(rows, schema).expect("should succeed");
        assert_eq!(batch.num_rows(), 1);
    }
}
