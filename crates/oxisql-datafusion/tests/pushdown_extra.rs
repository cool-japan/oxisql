//! Extra pushdown + hash-partition tests (Item 1 & 2, v0.1.2).
//!
//! Covers:
//! - `InList` / `Between` / `Like` pushdown in the streaming path
//!   (`can_push_filter` + `expr_to_sql`)
//! - `InList` / `Between` / `Like` + compound `AND`/`OR`/`NOT` evaluation in
//!   the snapshot path (`OxiSqlTableProvider`)
//! - `with_hash_partition` correctness (all rows, no duplicates, bucket count)

mod common;

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema};
use datafusion::prelude::{col, lit, SessionContext};
use oxisql_core::{Row, Value};
use oxisql_datafusion::{
    stream::{can_push_filter, expr_to_sql},
    OxiSqlTableProvider,
};

// ── Streaming pushdown ─────────────────────────────────────────────────────────

/// `col IN (1, 2, 3)` — `can_push_filter` returns `true` and the SQL contains `IN`.
#[test]
fn test_in_list_streaming_push() {
    let filter = col("id").in_list(vec![lit(1i64), lit(2i64), lit(3i64)], false);
    assert!(
        can_push_filter(&filter),
        "IN list filter should be pushable to the backend"
    );
    let sql = expr_to_sql(&filter).expect("expr_to_sql should succeed for IN list");
    assert!(
        sql.contains("IN"),
        "generated SQL should contain 'IN': {sql}"
    );
    assert!(
        sql.contains('1') && sql.contains('2') && sql.contains('3'),
        "generated SQL should contain all list elements: {sql}"
    );
}

/// `col BETWEEN 1 AND 10` — `can_push_filter` returns `true` and SQL contains `BETWEEN`.
#[test]
fn test_between_streaming_push() {
    let filter = col("age").between(lit(1i64), lit(10i64));
    assert!(
        can_push_filter(&filter),
        "BETWEEN filter should be pushable to the backend"
    );
    let sql = expr_to_sql(&filter).expect("expr_to_sql should succeed for BETWEEN");
    assert!(
        sql.contains("BETWEEN"),
        "generated SQL should contain 'BETWEEN': {sql}"
    );
}

/// `col LIKE '%foo%'` — `can_push_filter` returns `true` and SQL contains `LIKE`.
#[test]
fn test_like_streaming_push() {
    let filter = col("name").like(lit("%foo%"));
    assert!(
        can_push_filter(&filter),
        "LIKE filter should be pushable to the backend"
    );
    let sql = expr_to_sql(&filter).expect("expr_to_sql should succeed for LIKE");
    assert!(
        sql.contains("LIKE"),
        "generated SQL should contain 'LIKE': {sql}"
    );
}

// ── Snapshot evaluation helpers ───────────────────────────────────────────────

/// Build a 5-row (name, age) dataset for filter evaluation tests.
fn make_name_age_rows() -> (Vec<Row>, Arc<Schema>) {
    let schema = Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("age", DataType::Int64, false),
    ]));
    let cols: Vec<String> = vec!["name".into(), "age".into()];
    let rows = vec![
        Row::new(
            cols.clone(),
            vec![Value::Text("Alice".into()), Value::I64(30)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::Text("Bob".into()), Value::I64(25)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::Text("Alan".into()), Value::I64(45)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::Text("Charlie".into()), Value::I64(15)],
        ),
        Row::new(
            cols.clone(),
            vec![Value::Text("Dave".into()), Value::I64(95)],
        ),
    ];
    (rows, schema)
}

// ── Snapshot: InList ──────────────────────────────────────────────────────────

/// `code IN ('a', 'b')` filters a 4-row string table to 2 matching rows.
#[tokio::test]
async fn test_in_list_snapshot_eval() {
    let schema = Arc::new(Schema::new(vec![Field::new("code", DataType::Utf8, false)]));
    let cols: Vec<String> = vec!["code".into()];
    let rows = vec![
        Row::new(cols.clone(), vec![Value::Text("a".into())]),
        Row::new(cols.clone(), vec![Value::Text("b".into())]),
        Row::new(cols.clone(), vec![Value::Text("c".into())]),
        Row::new(cols.clone(), vec![Value::Text("d".into())]),
    ];

    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("in_snap", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT code FROM in_snap WHERE code IN ('a', 'b')")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 2,
        "IN ('a','b') should return exactly 2 rows, got {total}"
    );
}

// ── Snapshot: Between ─────────────────────────────────────────────────────────

/// `age BETWEEN 18 AND 65` keeps Alice(30), Bob(25), Alan(45) but not Charlie(15) or Dave(95).
#[tokio::test]
async fn test_between_snapshot_eval() {
    let (rows, schema) = make_name_age_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("between_snap", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT name FROM between_snap WHERE age BETWEEN 18 AND 65")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    // Alice(30), Bob(25), Alan(45) qualify; Charlie(15) and Dave(95) do not.
    assert_eq!(
        total, 3,
        "BETWEEN 18 AND 65 should return 3 rows (Alice, Bob, Alan), got {total}"
    );
}

// ── Snapshot: Like ────────────────────────────────────────────────────────────

/// `name LIKE 'Al%'` matches Alice and Alan but not Bob, Charlie, or Dave.
#[tokio::test]
async fn test_like_snapshot_eval() {
    let (rows, schema) = make_name_age_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("like_snap", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT name FROM like_snap WHERE name LIKE 'Al%'")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    use arrow::array::StringArray;
    let names: Vec<String> = batches
        .iter()
        .flat_map(|b| {
            b.column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("name column should be Utf8")
                .iter()
                .filter_map(|v| v.map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .collect();

    assert_eq!(
        names.len(),
        2,
        "LIKE 'Al%' should return 2 rows (Alice, Alan), got {:?}",
        names
    );
    assert!(
        names.contains(&"Alice".to_string()),
        "Alice should match 'Al%'"
    );
    assert!(
        names.contains(&"Alan".to_string()),
        "Alan should match 'Al%'"
    );
}

// ── Snapshot: compound AND / OR / NOT ────────────────────────────────────────

/// `(age > 18 AND name LIKE 'A%') OR age > 90` matches Alice(30), Alan(45), Dave(95).
#[tokio::test]
async fn test_and_or_snapshot_eval() {
    let (rows, schema) = make_name_age_rows();
    let provider = OxiSqlTableProvider::from_rows(rows, schema);
    let ctx = SessionContext::new();
    ctx.register_table("andor_snap", Arc::new(provider))
        .expect("register_table");

    // age > 18 AND name LIKE 'A%'  → Alice(30), Alan(45)
    // age > 90                     → Dave(95)
    // union                        → Alice, Alan, Dave = 3 rows
    let df = ctx
        .sql("SELECT name FROM andor_snap WHERE (age > 18 AND name LIKE 'A%') OR age > 90")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 3,
        "(age > 18 AND name LIKE 'A%') OR age > 90 should return 3 rows, got {total}"
    );
}

// ── Hash partitioning ─────────────────────────────────────────────────────────

/// Build n rows with sequential integer ids for hash-partition tests.
fn make_id_rows(n: usize) -> (Vec<Row>, Arc<Schema>) {
    let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
    let cols: Vec<String> = vec!["id".into()];
    let rows = (0..n as i64)
        .map(|i| Row::new(cols.clone(), vec![Value::I64(i)]))
        .collect();
    (rows, schema)
}

/// All rows appear in some partition: union of all partitions equals the full dataset.
#[tokio::test]
async fn test_hash_partition_covers_all_rows() {
    let (rows, schema) = make_id_rows(10);
    let provider = OxiSqlTableProvider::from_rows(rows, schema)
        .with_hash_partition("id", 3)
        .expect("with_hash_partition");

    assert_eq!(provider.len(), 10, "with_hash_partition must not drop rows");

    let ctx = SessionContext::new();
    ctx.register_table("hash_all", Arc::new(provider))
        .expect("register_table");

    let df = ctx.sql("SELECT id FROM hash_all").await.expect("sql parse");
    let batches = df.collect().await.expect("collect");
    let total: usize = batches.iter().map(|b| b.num_rows()).sum();
    assert_eq!(
        total, 10,
        "scan of hash partitions must return all 10 rows, got {total}"
    );
}

/// No row appears in more than one partition: sorted deduplication leaves the list unchanged.
#[tokio::test]
async fn test_hash_partition_no_duplicates() {
    let (rows, schema) = make_id_rows(20);
    let provider = OxiSqlTableProvider::from_rows(rows, schema)
        .with_hash_partition("id", 4)
        .expect("with_hash_partition");

    let ctx = SessionContext::new();
    ctx.register_table("hash_dedup", Arc::new(provider))
        .expect("register_table");

    let df = ctx
        .sql("SELECT id FROM hash_dedup ORDER BY id")
        .await
        .expect("sql parse");
    let batches = df.collect().await.expect("collect");

    use arrow::array::Int64Array;
    let ids: Vec<i64> = batches
        .iter()
        .flat_map(|b| {
            b.column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .expect("id column should be Int64")
                .values()
                .to_vec()
        })
        .collect();

    assert_eq!(
        ids.len(),
        20,
        "all 20 rows must be present, got {}",
        ids.len()
    );

    // After deduplication the list must be unchanged (no row appears twice).
    let mut deduped = ids.clone();
    deduped.dedup();
    assert_eq!(
        ids, deduped,
        "hash partition must produce no duplicate rows across partitions"
    );
}

/// `n=3` produces exactly 3 partitions, accessible via `partition_count()`.
#[test]
fn test_hash_partition_n_buckets() {
    let (rows, schema) = make_id_rows(30);
    let provider = OxiSqlTableProvider::from_rows(rows, schema)
        .with_hash_partition("id", 3)
        .expect("with_hash_partition");

    assert_eq!(
        provider.partition_count(),
        3,
        "with_hash_partition(\"id\", 3) must create exactly 3 partitions"
    );
    assert_eq!(
        provider.len(),
        30,
        "all 30 rows must be preserved after hash partitioning"
    );
}
