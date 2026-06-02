//! Benchmarks for oxisql-datafusion: DataFusion query execution and
//! RecordBatch construction overhead.

use std::sync::Arc;

use arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use oxisql_core::{Row, Value};
use oxisql_datafusion::{OxiSqlContext, OxiSqlStreamProvider, OxiSqlTableProvider};

/// Build an Arrow schema for the standard benchmark rows:
/// `id` (Int64), `name` (Utf8), `value` (Float64).
fn bench_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("value", DataType::Float64, false),
    ]))
}

/// Produce `n` rows with three columns: id (I64), name (Text), value (F64).
fn make_rows(n: usize) -> Vec<Row> {
    (0..n)
        .map(|i| {
            Row::new(
                vec!["id".into(), "name".into(), "value".into()],
                vec![
                    Value::I64(i as i64),
                    Value::Text(format!("item_{i}")),
                    Value::F64(i as f64 * 1.5),
                ],
            )
        })
        .collect()
}

/// Benchmark full DataFusion query execution: register snapshot, plan, and
/// execute `SELECT COUNT(*) FROM bench` for varying snapshot sizes.
fn bench_snapshot_provider(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_provider");
    let schema = bench_schema();

    for n_rows in [100usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::new("rows", n_rows), &n_rows, |b, &n| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let rows = make_rows(n);
            let schema = Arc::clone(&schema);

            b.to_async(&rt).iter(|| async {
                let ctx = OxiSqlContext::new();
                ctx.register_snapshot("bench", rows.clone(), Arc::clone(&schema))
                    .unwrap();
                let result = ctx.execute_sql("SELECT COUNT(*) FROM bench").await.unwrap();
                std::hint::black_box(result)
            })
        });
    }

    group.finish();
}

/// Benchmark `OxiSqlTableProvider::from_rows` construction (RecordBatch build)
/// for varying numbers of Int64 columns over 1 000 rows.
fn bench_record_batch_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("record_batch");

    for n_cols in [4usize, 16] {
        group.bench_with_input(BenchmarkId::new("cols", n_cols), &n_cols, |b, &n| {
            // Build schema once per (n_cols) parameterisation.
            let fields: Vec<Field> = (0..n)
                .map(|j| Field::new(format!("col{j}"), DataType::Int64, true))
                .collect();
            let schema: SchemaRef = Arc::new(Schema::new(fields));

            // Build rows once — we benchmark the provider construction, not
            // the row generation.
            let rows: Vec<Row> = (0..1_000)
                .map(|i| {
                    let cols: Vec<String> = (0..n).map(|j| format!("col{j}")).collect();
                    let vals: Vec<Value> = (0..n).map(|_| Value::I64(i as i64)).collect();
                    Row::new(cols, vals)
                })
                .collect();

            b.iter(|| {
                std::hint::black_box(OxiSqlTableProvider::from_rows(
                    rows.clone(),
                    Arc::clone(&schema),
                ))
            })
        });
    }

    group.finish();
}

/// Benchmark multi-partition parallel scan vs single-partition scan.
///
/// Uses `with_range_partition("id", n_parts)` to split 10 000 rows into 1, 2,
/// 4, or 8 contiguous ranges and then executes `SELECT COUNT(*) FROM part_bench`
/// through DataFusion so that all partitions are scanned.
fn bench_partition_scan(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let schema = bench_schema();
    let rows = make_rows(10_000);

    let mut group = c.benchmark_group("partition_scan");

    for n_parts in [1usize, 2, 4, 8] {
        group.bench_with_input(
            BenchmarkId::new("partitions", n_parts),
            &n_parts,
            |b, &n| {
                let rows = rows.clone();
                let schema = Arc::clone(&schema);

                b.to_async(&rt).iter(|| async {
                    let ctx = OxiSqlContext::new();
                    let provider =
                        OxiSqlTableProvider::from_rows(rows.clone(), Arc::clone(&schema))
                            .with_range_partition("id", n);
                    // Register the pre-built provider via the inner SessionContext.
                    ctx.session_context()
                        .register_table("part_bench", Arc::new(provider))
                        .unwrap();
                    let result = ctx
                        .execute_sql("SELECT COUNT(*) FROM part_bench")
                        .await
                        .unwrap();
                    std::hint::black_box(result)
                })
            },
        );
    }

    group.finish();
}

/// Profile Arrow array builder memory allocation patterns for large datasets.
///
/// Measures `Int64Builder` and `StringBuilder` construction costs at 1 000,
/// 10 000, and 100 000 rows to characterise allocation overhead independent of
/// the DataFusion execution layer.
fn bench_arrow_array_builder(c: &mut Criterion) {
    use arrow::array::{Float64Builder, Int64Builder, StringBuilder};

    let mut group = c.benchmark_group("arrow_builder");

    for n_rows in [1_000usize, 10_000, 100_000] {
        group.bench_with_input(BenchmarkId::new("build_int64", n_rows), &n_rows, |b, &n| {
            b.iter(|| {
                let mut builder = Int64Builder::with_capacity(n);
                for i in 0..n {
                    builder.append_value(i as i64);
                }
                std::hint::black_box(builder.finish())
            })
        });

        group.bench_with_input(BenchmarkId::new("build_utf8", n_rows), &n_rows, |b, &n| {
            b.iter(|| {
                let mut builder = StringBuilder::with_capacity(n, n * 8);
                for i in 0..n {
                    builder.append_value(format!("item_{i}"));
                }
                std::hint::black_box(builder.finish())
            })
        });

        group.bench_with_input(
            BenchmarkId::new("build_float64", n_rows),
            &n_rows,
            |b, &n| {
                b.iter(|| {
                    let mut builder = Float64Builder::with_capacity(n);
                    for i in 0..n {
                        builder.append_value(i as f64 * 1.5);
                    }
                    std::hint::black_box(builder.finish())
                })
            },
        );
    }

    group.finish();
}

/// Build the Arrow schema used for the filter-pushdown benchmark:
/// `id` (Int64), `name` (Utf8), `score` (Int64).
fn filter_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
        Field::new("score", DataType::Int64, false),
    ]))
}

/// Produce `n` rows suitable for the filter-pushdown benchmark.
/// `score` ranges 0 .. n, so `score > n/2` returns roughly half the rows.
fn make_filter_rows(n: usize) -> Vec<Row> {
    (0..n)
        .map(|i| {
            Row::new(
                vec!["id".into(), "name".into(), "score".into()],
                vec![
                    Value::I64(i as i64),
                    Value::Text(format!("item_{i}")),
                    Value::I64(i as i64),
                ],
            )
        })
        .collect()
}

/// Compare DataFusion post-scan filtering (snapshot provider) against backend
/// pre-filtering (stream provider) for a 10 000-row table with `score > 5000`.
///
/// * **snapshot_postfilter_10k** — all rows are materialised up front;
///   DataFusion applies the WHERE predicate after the full scan.
/// * **stream_prefilter_10k** — the predicate is pushed down to the embedded
///   SQL backend as a WHERE clause; only matching rows cross the boundary.
fn bench_filter_pushdown(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    const N: usize = 10_000;
    let schema = filter_schema();
    let rows = make_filter_rows(N);

    let mut group = c.benchmark_group("filter_pushdown");

    // Baseline: snapshot provider — DataFusion post-filters after full scan.
    group.bench_function("snapshot_postfilter_10k", |b| {
        let rows = rows.clone();
        let schema = Arc::clone(&schema);
        b.to_async(&rt).iter(|| {
            let rows = rows.clone();
            let schema = Arc::clone(&schema);
            async move {
                let ctx = OxiSqlContext::new();
                ctx.register_snapshot("bench_filter_snap", rows, Arc::clone(&schema))
                    .unwrap();
                let result = ctx
                    .execute_sql("SELECT id FROM bench_filter_snap WHERE score > 5000")
                    .await
                    .unwrap();
                std::hint::black_box(result)
            }
        })
    });

    // Pushdown: stream provider — predicate translated to SQL WHERE clause.
    group.bench_function("stream_prefilter_10k", |b| {
        let schema = Arc::clone(&schema);
        b.to_async(&rt).iter(|| {
            let schema = Arc::clone(&schema);
            async move {
                use oxisql_core::{Connection, ToSqlValue};
                use oxisql_embedded::EmbeddedConnection;

                let conn = EmbeddedConnection::open_memory().unwrap();
                conn.execute(
                    "CREATE TABLE bench_filter_str (id INTEGER, name TEXT, score INTEGER)",
                    &[],
                )
                .await
                .unwrap();
                for i in 0..N {
                    conn.execute(
                        "INSERT INTO bench_filter_str VALUES ($1, $2, $3)",
                        &[
                            &Value::I64(i as i64) as &dyn ToSqlValue,
                            &Value::Text(format!("item_{i}")) as &dyn ToSqlValue,
                            &Value::I64(i as i64) as &dyn ToSqlValue,
                        ],
                    )
                    .await
                    .unwrap();
                }
                let conn_arc: Arc<dyn Connection> = Arc::new(conn);
                let provider = Arc::new(OxiSqlStreamProvider::new(
                    Arc::clone(&conn_arc),
                    "bench_filter_str",
                    Arc::clone(&schema),
                ));
                let ctx = OxiSqlContext::new();
                ctx.session_context()
                    .register_table("bench_filter_str", provider)
                    .unwrap();
                let result = ctx
                    .execute_sql("SELECT id FROM bench_filter_str WHERE score > 5000")
                    .await
                    .unwrap();
                std::hint::black_box(result)
            }
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_snapshot_provider,
    bench_record_batch_construction,
    bench_partition_scan,
    bench_arrow_array_builder,
    bench_filter_pushdown,
);
criterion_main!(benches);
