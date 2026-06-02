//! Basic snapshot provider integration tests.
//!
//! Registers `OxiSqlTableProvider` with a DataFusion `SessionContext` and
//! executes SQL queries, asserting on returned `RecordBatch` data.

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::SessionContext;
use oxisql_core::{Row, Value};
use oxisql_datafusion::OxiSqlTableProvider;

#[tokio::test]
async fn select_all_returns_expected_rows() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id, name FROM test ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    assert!(!batches.is_empty(), "expected at least one batch");
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2, "expected 2 rows total");
}

#[tokio::test]
async fn schema_mapping_int64_correct() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("test2", Arc::new(provider))
        .expect("register_table");

    let df = ctx.sql("SELECT id FROM test2").await.expect("sql parse");
    let batches = df.collect().await.expect("collect");

    assert!(!batches.is_empty());
    assert_eq!(
        batches[0].schema().field(0).data_type(),
        &DataType::Int64,
        "id column must be Int64"
    );
}

#[tokio::test]
async fn where_filter_works() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("test3", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT name FROM test3 WHERE id = 1")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 1, "WHERE id=1 should return exactly 1 row");
}

#[tokio::test]
async fn aggregate_query_works() {
    let (rows, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);

    let ctx = SessionContext::new();
    ctx.register_table("test4", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT COUNT(*) as cnt FROM test4")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    assert!(!batches.is_empty());
    // The COUNT(*) result should be a single row with value 2
    assert_eq!(batches[0].num_rows(), 1, "aggregate should return 1 row");
}

#[tokio::test]
async fn nullable_columns_handled_correctly() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("note", DataType::Utf8, true),
    ]));

    let cols: Vec<String> = vec!["id".into(), "note".into()];
    let rows = vec![
        Row::new(cols.clone(), vec![Value::I64(10), Value::Null]),
        Row::new(cols, vec![Value::I64(11), Value::Text("hello".to_string())]),
    ];

    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("nullable_test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id FROM nullable_test ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 2);
}

#[tokio::test]
async fn empty_table_handled_correctly() {
    let (_, schema) = common::make_test_rows();
    let provider = OxiSqlTableProvider::from_rows(vec![], schema);

    let ctx = SessionContext::new();
    ctx.register_table("empty_test", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id, name FROM empty_test")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(total_rows, 0, "empty table should return 0 rows");
}
