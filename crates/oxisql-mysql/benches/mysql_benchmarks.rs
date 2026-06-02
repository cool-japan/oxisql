//! Criterion benchmarks for `oxisql-mysql` type conversion routines.
//!
//! These benchmarks exercise the hot-path value conversion code paths without
//! requiring a live MySQL server, making them suitable for CI and local profiling.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use mysql_async::Value as MysqlValue;
use oxisql_core::Value;
use oxisql_mysql::core_value_to_mysql;
use oxisql_mysql::types::{mysql_value_to_core, mysql_value_to_core_with_type};

// ── mysql → core ──────────────────────────────────────────────────────────────

fn bench_mysql_to_core(c: &mut Criterion) {
    // Pre-build values that will be cloned on each iteration to give the
    // benchmark a consistent per-call cost (mysql_value_to_core takes ownership).
    let int_val = MysqlValue::Int(42);
    let uint_val = MysqlValue::UInt(123_456);
    let float_val = MysqlValue::Float(std::f32::consts::PI);
    let double_val = MysqlValue::Double(std::f64::consts::E);
    let bytes_val = MysqlValue::Bytes(b"hello world".to_vec());
    let null_val = MysqlValue::NULL;
    let date_val = MysqlValue::Date(2024, 3, 15, 0, 0, 0, 0);
    let datetime_val = MysqlValue::Date(2024, 3, 15, 10, 30, 55, 123_456);
    let time_val = MysqlValue::Time(false, 1, 2, 30, 0, 0);

    c.bench_function("mysql_int_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(int_val.clone())).expect("int conversion"),
            )
        })
    });

    c.bench_function("mysql_uint_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(uint_val.clone()))
                    .expect("uint conversion"),
            )
        })
    });

    c.bench_function("mysql_float_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(float_val.clone()))
                    .expect("float conversion"),
            )
        })
    });

    c.bench_function("mysql_double_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(double_val.clone()))
                    .expect("double conversion"),
            )
        })
    });

    c.bench_function("mysql_bytes_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(bytes_val.clone()))
                    .expect("bytes conversion"),
            )
        })
    });

    c.bench_function("mysql_null_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(null_val.clone()))
                    .expect("null conversion"),
            )
        })
    });

    c.bench_function("mysql_date_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(date_val.clone()))
                    .expect("date conversion"),
            )
        })
    });

    c.bench_function("mysql_datetime_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(datetime_val.clone()))
                    .expect("datetime conversion"),
            )
        })
    });

    c.bench_function("mysql_time_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core(std::hint::black_box(time_val.clone()))
                    .expect("time conversion"),
            )
        })
    });
}

// ── mysql → core (with column type hint) ─────────────────────────────────────

fn bench_mysql_to_core_with_type(c: &mut Criterion) {
    use mysql_async::consts::ColumnType;

    let decimal_bytes = MysqlValue::Bytes(b"123456.789".to_vec());
    let json_bytes = MysqlValue::Bytes(br#"{"key":"value"}"#.to_vec());
    let text_bytes = MysqlValue::Bytes(b"plain text string".to_vec());

    c.bench_function("mysql_decimal_bytes_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core_with_type(
                    std::hint::black_box(decimal_bytes.clone()),
                    ColumnType::MYSQL_TYPE_NEWDECIMAL,
                )
                .expect("decimal conversion"),
            )
        })
    });

    c.bench_function("mysql_json_bytes_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core_with_type(
                    std::hint::black_box(json_bytes.clone()),
                    ColumnType::MYSQL_TYPE_JSON,
                )
                .expect("json conversion"),
            )
        })
    });

    c.bench_function("mysql_text_bytes_to_core", |b| {
        b.iter(|| {
            std::hint::black_box(
                mysql_value_to_core_with_type(
                    std::hint::black_box(text_bytes.clone()),
                    ColumnType::MYSQL_TYPE_VARCHAR,
                )
                .expect("text conversion"),
            )
        })
    });
}

// ── core → mysql ──────────────────────────────────────────────────────────────

fn bench_core_to_mysql(c: &mut Criterion) {
    let null_val = Value::Null;
    let bool_val = Value::Bool(true);
    let i64_val = Value::I64(42);
    let f64_val = Value::F64(std::f64::consts::PI);
    let text_val = Value::Text("hello world".to_string());
    let blob_val = Value::Blob(b"binary data".to_vec());
    let timestamp_val = Value::Timestamp(1_710_497_455_000_000);
    let json_val = Value::Json(r#"{"key":"value","number":42}"#.to_string());
    let decimal_val = Value::Decimal("9999999.99".to_string());

    c.bench_function("core_null_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&null_val))))
    });

    c.bench_function("core_bool_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&bool_val))))
    });

    c.bench_function("core_i64_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&i64_val))))
    });

    c.bench_function("core_f64_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&f64_val))))
    });

    c.bench_function("core_text_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&text_val))))
    });

    c.bench_function("core_blob_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&blob_val))))
    });

    c.bench_function("core_timestamp_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&timestamp_val))))
    });

    c.bench_function("core_json_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&json_val))))
    });

    c.bench_function("core_decimal_to_mysql", |b| {
        b.iter(|| std::hint::black_box(core_value_to_mysql(std::hint::black_box(&decimal_val))))
    });
}

// ── prepared statement overhead (type-conversion round-trip) ─────────────────

/// Benchmark the per-query overhead of binding and reading back a
/// representative prepared-statement payload (10 mixed-type parameters).
///
/// Models "prepared statement reuse vs fresh parse": both paths pay the same
/// type-conversion cost on every execution.  The conversion itself is the
/// dominant per-query CPU cost when the server holds the parsed plan.
fn bench_prepared_stmt_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("prepared_stmt_overhead");

    // Ten parameters covering all common types sent as prepared-statement
    // bindings: integer, string, float, bool, null.
    let params: Vec<Value> = vec![
        Value::I64(42),
        Value::Text("hello".to_string()),
        Value::F64(std::f64::consts::PI),
        Value::Bool(true),
        Value::Null,
        Value::I64(i64::MAX),
        Value::Text("world".to_string()),
        Value::F64(0.0),
        Value::Bool(false),
        Value::I64(0),
    ];

    group.bench_function("convert_10_params_to_mysql", |b| {
        b.iter(|| {
            let result: Vec<MysqlValue> = params
                .iter()
                .map(|v| std::hint::black_box(core_value_to_mysql(v)))
                .collect();
            std::hint::black_box(result)
        })
    });

    // Pre-build the corresponding MySQL result row (what the server returns
    // after executing the prepared statement).
    let mysql_result_row: Vec<MysqlValue> = vec![
        MysqlValue::Int(42),
        MysqlValue::Bytes(b"hello".to_vec()),
        MysqlValue::Double(std::f64::consts::PI),
        MysqlValue::Int(1),
        MysqlValue::NULL,
        MysqlValue::Int(i64::MAX),
        MysqlValue::Bytes(b"world".to_vec()),
        MysqlValue::Double(0.0),
        MysqlValue::Int(0),
        MysqlValue::Int(0),
    ];

    group.bench_function("convert_10_results_from_mysql", |b| {
        b.iter(|| {
            let result: Vec<Value> = mysql_result_row
                .iter()
                .map(|v| {
                    std::hint::black_box(
                        mysql_value_to_core(std::hint::black_box(v.clone()))
                            .expect("result conversion"),
                    )
                })
                .collect();
            std::hint::black_box(result)
        })
    });

    // Round-trip: binding 10 params → 10 MySQL values → 10 core values.
    // Represents the combined CPU cost for a single prepared-statement execution.
    group.bench_function("round_trip_10_params", |b| {
        b.iter(|| {
            let mysql_vals: Vec<MysqlValue> = params.iter().map(core_value_to_mysql).collect();
            let result: Vec<Value> = mysql_vals
                .into_iter()
                .map(|v| {
                    std::hint::black_box(
                        mysql_value_to_core(std::hint::black_box(v))
                            .expect("round-trip conversion"),
                    )
                })
                .collect();
            std::hint::black_box(result)
        })
    });

    group.finish();
}

// ── pool construction under varying sizes ─────────────────────────────────────

/// Benchmark `MyConnectionBuilder` configuration with varying `max_size` pool
/// settings.  No network connection is made — this measures only the cost of
/// setting up the builder configuration struct, modelling pool acquisition
/// latency differences that stem purely from the configuration bookkeeping path.
fn bench_pool_construction(c: &mut Criterion) {
    use oxisql_mysql::MyConnectionBuilder;

    let mut group = c.benchmark_group("pool_construction");

    for max_size in [2_usize, 10, 50] {
        group.bench_with_input(
            BenchmarkId::new("build_pool_config", max_size),
            &max_size,
            |b, &n| {
                b.iter(|| {
                    // Build + fully configure the builder — this is the work
                    // callers do before calling `.connect()`.  No I/O is issued.
                    let builder = MyConnectionBuilder::new()
                        .host("localhost")
                        .port(3306)
                        .user("root")
                        .password("pw")
                        .dbname("test")
                        .pool_max(n)
                        .pool_min(1);
                    std::hint::black_box(builder)
                })
            },
        );
    }

    group.finish();
}

// ── batch type conversion: 100 rows × 5 columns ───────────────────────────────

/// Benchmark converting a full result batch as returned by a typical `SELECT`.
///
/// 100 rows × 5 columns exercises the allocation and conversion hot path that
/// `MyConnection::query` traverses for every network response.  This models
/// realistic SELECT result processing overhead.
fn bench_type_conversion_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("type_conversion_batch");

    // Build the source data once; each iteration clones rows to simulate the
    // per-call allocation behaviour of `query`.
    let rows: Vec<Vec<MysqlValue>> = (0_i64..100)
        .map(|i| {
            vec![
                MysqlValue::Int(i),
                MysqlValue::Bytes(format!("name_{i}").into_bytes()),
                MysqlValue::Double(f64::from(i as f32) * 1.5),
                MysqlValue::NULL,
                MysqlValue::Int(i * 100),
            ]
        })
        .collect();

    group.bench_function("100rows_5cols_to_core", |b| {
        b.iter(|| {
            let result: Vec<Vec<Value>> = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| {
                            std::hint::black_box(
                                mysql_value_to_core(std::hint::black_box(v.clone()))
                                    .expect("batch conversion"),
                            )
                        })
                        .collect()
                })
                .collect();
            std::hint::black_box(result)
        })
    });

    // Also benchmark the reverse: simulating a 100-row INSERT batch where each
    // row's core values are converted to MySQL params.
    let core_rows: Vec<Vec<Value>> = (0_i64..100)
        .map(|i| {
            vec![
                Value::I64(i),
                Value::Text(format!("name_{i}")),
                Value::F64(i as f64 * 1.5),
                Value::Null,
                Value::I64(i * 100),
            ]
        })
        .collect();

    group.bench_function("100rows_5cols_to_mysql", |b| {
        b.iter(|| {
            let result: Vec<Vec<MysqlValue>> = core_rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| std::hint::black_box(core_value_to_mysql(v)))
                        .collect()
                })
                .collect();
            std::hint::black_box(result)
        })
    });

    group.finish();
}

// ── bulk load comparison: batched vs individual INSERT param building ─────────

/// Benchmark the parameter-build cost difference between batched INSERT
/// (as performed by `load_data_batched`) and individual-row INSERT.
///
/// Both paths exercise the same `core_value_to_mysql` hot path; the difference
/// is whether all 100 rows are collected into a single `Vec<Vec<_>>` (batch) or
/// accumulated into a running `total` count (individual).  This isolates the
/// allocation and iteration overhead that separates bulk from row-at-a-time
/// ingestion at the Rust layer.
fn bench_bulk_load_comparison(c: &mut Criterion) {
    let mut group = c.benchmark_group("bulk_load_comparison");

    // 100 rows × 3 columns — same shape as a typical `load_data_batched` call.
    let batch_data: Vec<Vec<Value>> = (0_i64..100)
        .map(|i| {
            vec![
                Value::I64(i),
                Value::Text(format!("name_{i}")),
                Value::F64(i as f64 * 1.5),
            ]
        })
        .collect();

    // Simulate batched INSERT: build all params at once (what `load_data_batched` does).
    group.bench_function("batch_100rows_param_build", |b| {
        b.iter(|| {
            let params: Vec<Vec<MysqlValue>> = batch_data
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|v| std::hint::black_box(core_value_to_mysql(v)))
                        .collect()
                })
                .collect();
            std::hint::black_box(params)
        })
    });

    // Simulate individual INSERT: build one row's params at a time.
    group.bench_function("individual_100rows_param_build", |b| {
        b.iter(|| {
            let mut total = 0_usize;
            for row in &batch_data {
                let params: Vec<MysqlValue> = row
                    .iter()
                    .map(|v| std::hint::black_box(core_value_to_mysql(v)))
                    .collect();
                total += params.len();
            }
            std::hint::black_box(total)
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_mysql_to_core,
    bench_mysql_to_core_with_type,
    bench_core_to_mysql,
    bench_prepared_stmt_overhead,
    bench_pool_construction,
    bench_type_conversion_batch,
    bench_bulk_load_comparison
);
criterion_main!(benches);
