//! Shared test helpers for oxisql-datafusion integration tests.
//!
//! Each test binary (`tests/*.rs`) includes this module via `mod common;`.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use oxisql_core::{Row, Value};

/// Build a small test dataset: two rows with id / name / score columns.
#[allow(dead_code)]
pub fn make_test_rows() -> (Vec<Row>, Arc<Schema>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Float64, false),
    ]));

    let cols: Vec<String> = vec!["id".into(), "name".into(), "score".into()];
    let rows = vec![
        Row::new(
            cols.clone(),
            vec![
                Value::I64(1),
                Value::Text("Alice".to_string()),
                Value::F64(95.5),
            ],
        ),
        Row::new(
            cols,
            vec![
                Value::I64(2),
                Value::Text("Bob".to_string()),
                Value::F64(87.0),
            ],
        ),
    ];

    (rows, schema)
}
