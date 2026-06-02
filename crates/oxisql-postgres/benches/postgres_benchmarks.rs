use criterion::{criterion_group, criterion_main, BenchmarkGroup, BenchmarkId, Criterion};
use oxisql_core::{Connection, Value};
use oxisql_postgres::types::value_to_param;
use oxisql_postgres::{PgConnection, TlsMode};

// ── existing single-variant benchmarks ────────────────────────────────────────

fn bench_value_to_param_int(c: &mut Criterion) {
    let val = Value::I64(42);
    c.bench_function("value_to_param_i64", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val)))
    });
}

fn bench_value_to_param_text(c: &mut Criterion) {
    let val = Value::Text("hello world".to_string());
    c.bench_function("value_to_param_text", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val)))
    });
}

fn bench_value_to_param_float(c: &mut Criterion) {
    let val = Value::F64(std::f64::consts::PI);
    c.bench_function("value_to_param_f64", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val)))
    });
}

fn bench_value_to_param_null(c: &mut Criterion) {
    let val = Value::Null;
    c.bench_function("value_to_param_null", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val)))
    });
}

fn bench_value_to_param_bool(c: &mut Criterion) {
    let val = Value::Bool(true);
    c.bench_function("value_to_param_bool", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val)))
    });
}

// ── Group: value_to_param_all_types ───────────────────────────────────────────
//
// Benchmarks `value_to_param` for every `Value` variant that exists in
// `oxisql_core`.  All fixtures are built once outside `b.iter()`; only a
// borrow is passed inside so the measurement reflects the conversion path
// rather than allocator overhead.
//
// Note on output variants: extended types (Timestamp, Date, Time, Uuid, Json,
// Decimal, Array) all map to `OwnedParam::Text` via string formatting.  The
// benchmark names label the *input* variant — not the output.

fn bench_value_to_param_all_types(c: &mut Criterion) {
    let mut group: BenchmarkGroup<_> = c.benchmark_group("value_to_param_all_types");

    // Scalar / primitive variants — no allocation inside the iter closure.
    let val_null = Value::Null;
    group.bench_function("null", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_null)))
    });

    let val_bool_true = Value::Bool(true);
    group.bench_function("bool_true", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_bool_true)))
    });

    let val_bool_false = Value::Bool(false);
    group.bench_function("bool_false", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_bool_false)))
    });

    let val_i64_max = Value::I64(i64::MAX);
    group.bench_function("i64_max", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_i64_max)))
    });

    let val_i64_min = Value::I64(i64::MIN);
    group.bench_function("i64_min", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_i64_min)))
    });

    let val_f64 = Value::F64(std::f64::consts::PI);
    group.bench_function("f64_pi", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_f64)))
    });

    // Heap-owning variants — built once, borrowed each iteration.
    let val_text_short = Value::Text("hello world".to_string());
    group.bench_function("text_short", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_text_short)))
    });

    let val_text_long = Value::Text("a".repeat(1024));
    group.bench_function("text_long_1024", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_text_long)))
    });

    let val_blob_small = Value::Blob(vec![0xde, 0xad, 0xbe, 0xef]);
    group.bench_function("blob_small_4b", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_blob_small)))
    });

    let val_blob_large = Value::Blob(vec![0u8; 1024]);
    group.bench_function("blob_large_1024b", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_blob_large)))
    });

    // Extended types — converted to OwnedParam::Text via string formatting.
    // The cost measured is the formatting arithmetic + String allocation.
    let val_timestamp = Value::Timestamp(1_700_000_000_000_000);
    group.bench_function("timestamp", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_timestamp)))
    });

    let val_date = Value::Date(19_000);
    group.bench_function("date", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_date)))
    });

    let val_time = Value::Time(43_200_000_000); // noon
    group.bench_function("time", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_time)))
    });

    // UUID: stored as u128.
    let val_uuid = Value::Uuid(0x550e_8400_e29b_41d4_a716_4466_5544_0000_u128);
    group.bench_function("uuid", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_uuid)))
    });

    let val_json = Value::Json(r#"{"key":"value","num":42}"#.to_string());
    group.bench_function("json", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_json)))
    });

    let val_decimal = Value::Decimal("123456789.123456789".to_string());
    group.bench_function("decimal", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_decimal)))
    });

    // Array of i64 — rendered as Postgres array literal "{1,2,3}".
    let val_array_ints = Value::Array(vec![Value::I64(1), Value::I64(2), Value::I64(3)]);
    group.bench_function("array_ints_3", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_array_ints)))
    });

    // Larger array to show O(n) formatting cost.
    let val_array_large: Value = Value::Array((0_i64..100).map(Value::I64).collect());
    group.bench_function("array_ints_100", |b| {
        b.iter(|| std::hint::black_box(value_to_param(&val_array_large)))
    });

    group.finish();
}

// ── Group: batch_row_conversion ───────────────────────────────────────────────
//
// Simulates converting a batch of rows (100 rows × 8 columns) through the
// `value_to_param` path — representative of what happens before a bulk INSERT.
//
// The fixture is built once outside `b.iter()`.  Each iteration borrows into
// the pre-built matrix and invokes `value_to_param` for each cell.  The
// per-cell allocations (for extended types that produce OwnedParam::Text) are
// intentional — they reflect the real cost of the conversion pipeline.

fn bench_batch_row_conversion(c: &mut Criterion) {
    let mut group = c.benchmark_group("batch_row_conversion");

    // Build a 100-row × 8-column fixture of mixed types.
    // Columns: i64, f64, text, blob (4 B), timestamp, date, json, null.
    let rows: Vec<Vec<Value>> = (0..100_i64)
        .map(|i| {
            vec![
                Value::I64(i),
                Value::F64(std::f64::consts::PI * (i as f64)),
                Value::Text(format!("row_{i}")),
                Value::Blob(vec![(i & 0xff) as u8, ((i >> 8) & 0xff) as u8, 0xbe, 0xef]),
                Value::Timestamp(1_700_000_000_000_000 + i * 1_000_000),
                Value::Date(19_000 + i as i32),
                Value::Json(format!(r#"{{"id":{i}}}"#)),
                Value::Null,
            ]
        })
        .collect();

    group.bench_function("100rows_8cols_to_param", |b| {
        b.iter(|| {
            for row in &rows {
                for val in row {
                    std::hint::black_box(value_to_param(std::hint::black_box(val)));
                }
            }
        })
    });

    // Also benchmark a narrower 100-row × 3-col all-scalar fixture (no
    // formatting cost) to isolate the overhead from the match dispatch alone.
    let rows_scalar: Vec<Vec<Value>> = (0..100_i64)
        .map(|i| vec![Value::I64(i), Value::F64(i as f64), Value::Bool(i % 2 == 0)])
        .collect();

    group.bench_function("100rows_3cols_scalar_only", |b| {
        b.iter(|| {
            for row in &rows_scalar {
                for val in row {
                    std::hint::black_box(value_to_param(std::hint::black_box(val)));
                }
            }
        })
    });

    group.finish();
}

// ── Benchmark 1: query throughput ─────────────────────────────────────────────
//
// Measures per-call latency for three query shapes against a live Postgres
// server.  Skips gracefully when `POSTGRES_URL` is absent so this compiles and
// is safe to register in CI environments without a database.
//
// Table schema:
//   bench_qt_a (id BIGINT PRIMARY KEY, val TEXT)
//   bench_qt_b (id BIGINT, a_id BIGINT)

fn bench_query_throughput(c: &mut Criterion) {
    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let conn = rt
        .block_on(PgConnection::connect(&url, TlsMode::Disabled))
        .expect("connect");

    rt.block_on(conn.execute_batch(
        "DROP TABLE IF EXISTS bench_qt_b;
         DROP TABLE IF EXISTS bench_qt_a;
         CREATE TABLE bench_qt_a (id BIGINT PRIMARY KEY, val TEXT NOT NULL);
         CREATE TABLE bench_qt_b (id BIGINT, a_id BIGINT);",
    ))
    .expect("setup tables");

    // Insert 100 rows in bench_qt_a and 100 rows in bench_qt_b.
    for i in 0_i64..100 {
        rt.block_on(conn.execute(
            "INSERT INTO bench_qt_a (id, val) VALUES ($1, $2)",
            &[&i, &format!("val_{i}")],
        ))
        .expect("insert a");
        rt.block_on(conn.execute(
            "INSERT INTO bench_qt_b (id, a_id) VALUES ($1, $2)",
            &[&i, &i],
        ))
        .expect("insert b");
    }

    let mut group = c.benchmark_group("query_throughput");

    group.bench_function("simple_select", |b| {
        b.to_async(&rt).iter(|| async {
            std::hint::black_box(conn.query("SELECT 1", &[]).await.expect("simple_select"))
        })
    });

    group.bench_function("parameterized_select", |b| {
        b.to_async(&rt).iter(|| async {
            std::hint::black_box(
                conn.query("SELECT id, val FROM bench_qt_a WHERE id = $1", &[&42_i64])
                    .await
                    .expect("param_select"),
            )
        })
    });

    group.bench_function("join_query", |b| {
        b.to_async(&rt).iter(|| async {
            std::hint::black_box(
                conn.query(
                    "SELECT a.id, a.val FROM bench_qt_a a \
                     JOIN bench_qt_b b ON a.id = b.a_id WHERE b.id = $1",
                    &[&42_i64],
                )
                .await
                .expect("join_query"),
            )
        })
    });

    group.finish();

    rt.block_on(
        conn.execute_batch("DROP TABLE IF EXISTS bench_qt_b; DROP TABLE IF EXISTS bench_qt_a;"),
    )
    .expect("cleanup");
}

// ── Benchmark 2: pool vs single connection ────────────────────────────────────
//
// Compares throughput of:
//   pool_10conn  — 10 independent `PgConnection` clones (no mutex contention
//                  between them), 10 concurrent async tasks per iter.
//   single_conn  — one `PgConnection` shared across 10 sequential queries
//                  (each query serialises through Arc<Mutex<Client>>).
//
// Note: "pool" here is modelled with N pre-connected clients rather than
// `OxidbPgPool` to avoid adding a dev-dependency.  The key difference is the
// same: parallel vs serial use of connections.

fn bench_pool_vs_single_connection(c: &mut Criterion) {
    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    let rt = tokio::runtime::Runtime::new().expect("rt");

    // Open 10 independent connections (each has its own tokio-postgres client).
    let conns: Vec<PgConnection> = (0..10)
        .map(|_| {
            rt.block_on(PgConnection::connect(&url, TlsMode::Disabled))
                .expect("pool conn")
        })
        .collect();

    // One connection used for the sequential benchmark.
    let single = conns[0].clone();

    let mut group = c.benchmark_group("pool_vs_single_connection");

    group.bench_function("pool_10conn", |b| {
        b.to_async(&rt).iter(|| async {
            let futs: Vec<_> = conns.iter().map(|c| c.query("SELECT 1", &[])).collect();
            let results = futures::future::join_all(futs).await;
            std::hint::black_box(results)
        })
    });

    group.bench_function("single_conn", |b| {
        b.to_async(&rt).iter(|| async {
            let mut results = Vec::with_capacity(10);
            for _ in 0..10 {
                results.push(single.query("SELECT 1", &[]).await.expect("single q"));
            }
            std::hint::black_box(results)
        })
    });

    group.finish();
}

// ── Benchmark 3: COPY protocol vs individual INSERT ───────────────────────────
//
// `copy_protocol_100` uses `PgConnection::copy_in_text` (PostgreSQL COPY FROM
// STDIN) to load 100 rows in a single wire transfer.
// `bulk_insert_100` uses 100 individual `INSERT` statements via `execute_batch`.
//
// Both write to a temporary table dropped after the benchmark.

fn bench_copy_vs_insert(c: &mut Criterion) {
    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let conn = rt
        .block_on(PgConnection::connect(&url, TlsMode::Disabled))
        .expect("connect");

    rt.block_on(conn.execute_batch(
        "DROP TABLE IF EXISTS bench_copy_tgt;
         CREATE TABLE bench_copy_tgt (id BIGINT, val TEXT);",
    ))
    .expect("setup");

    // Pre-build the 100-row payload so allocation is not measured.
    let rows: Vec<Vec<String>> = (0_i64..100)
        .map(|i| vec![i.to_string(), format!("val_{i}")])
        .collect();

    // Build a multi-row INSERT statement for the bulk_insert path.
    let insert_sql: String = {
        let values: Vec<String> = (0_i64..100).map(|i| format!("({i}, 'val_{i}')")).collect();
        format!(
            "INSERT INTO bench_copy_tgt (id, val) VALUES {}",
            values.join(",")
        )
    };

    let mut group = c.benchmark_group("copy_vs_insert");

    group.bench_function("bulk_insert_100", |b| {
        b.to_async(&rt).iter(|| async {
            conn.execute_batch(&insert_sql).await.expect("bulk_insert");
            conn.execute_batch("TRUNCATE bench_copy_tgt")
                .await
                .expect("truncate");
        })
    });

    group.bench_function("copy_protocol_100", |b| {
        b.to_async(&rt).iter(|| async {
            let row_iter = rows.clone().into_iter();
            std::hint::black_box(
                conn.copy_in_text("bench_copy_tgt", &["id", "val"], row_iter)
                    .await
                    .expect("copy_in"),
            );
            conn.execute_batch("TRUNCATE bench_copy_tgt")
                .await
                .expect("truncate");
        })
    });

    group.finish();

    rt.block_on(conn.execute_batch("DROP TABLE IF EXISTS bench_copy_tgt;"))
        .expect("cleanup");
}

// ── Benchmark 4: Arc<Mutex<Client>> contention ────────────────────────────────
//
// Measures serialisation cost when N concurrent async tasks all issue queries
// through the *same* `PgConnection` (backed by a single `Arc<Mutex<Client>>`).
// Tasks run on the same runtime; each must wait its turn to lock the client.
//
// Concurrency levels tested: 1 / 5 / 20.

fn bench_arc_mutex_contention(c: &mut Criterion) {
    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let conn = rt
        .block_on(PgConnection::connect(&url, TlsMode::Disabled))
        .expect("connect");

    let mut group = c.benchmark_group("arc_mutex_contention");

    for n_tasks in [1_usize, 5, 20] {
        group.bench_with_input(
            BenchmarkId::new("concurrent_tasks", n_tasks),
            &n_tasks,
            |b, &n| {
                b.to_async(&rt).iter(|| async {
                    let futs: Vec<_> = (0..n).map(|_| conn.query("SELECT 1", &[])).collect();
                    let results = futures::future::join_all(futs).await;
                    std::hint::black_box(results)
                })
            },
        );
    }

    group.finish();
}

// ── Benchmark 5: prepared statement cache hit rate ────────────────────────────
//
// `with_prepare` — calls `Connection::prepare()` on each iteration (cache hit
//   after the first call) then executes via `PreparedStatement::query`.
// `without_prepare` — calls `Connection::query()` directly, which also issues
//   an unnamed `Parse` message on every call (no client-side cache).
//
// The pre-warm step before the bench loop ensures `with_prepare` never pays the
// initial round-trip cost inside `iter()`.

fn bench_prepared_stmt_cache(c: &mut Criterion) {
    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        Err(_) => return,
    };
    let rt = tokio::runtime::Runtime::new().expect("rt");
    let conn = rt
        .block_on(PgConnection::connect(&url, TlsMode::Disabled))
        .expect("connect");

    rt.block_on(conn.execute_batch(
        "DROP TABLE IF EXISTS bench_ps_cache;
         CREATE TABLE bench_ps_cache (id BIGINT PRIMARY KEY);",
    ))
    .expect("setup");

    for i in 0_i64..50 {
        rt.block_on(conn.execute("INSERT INTO bench_ps_cache (id) VALUES ($1)", &[&i]))
            .expect("insert");
    }

    // Pre-warm: one round-trip to server so subsequent prepare() calls are
    // pure HashMap hits.
    let _warmup = rt
        .block_on(conn.prepare("SELECT id FROM bench_ps_cache WHERE id = $1"))
        .expect("warmup prepare");

    let mut group = c.benchmark_group("prepared_stmt_cache");

    group.bench_function("with_prepare", |b| {
        b.to_async(&rt).iter(|| async {
            let mut stmt = conn
                .prepare("SELECT id FROM bench_ps_cache WHERE id = $1")
                .await
                .expect("prepare hit");
            std::hint::black_box(stmt.query(&[&42_i64]).await.expect("exec prepared"))
        })
    });

    group.bench_function("without_prepare", |b| {
        b.to_async(&rt).iter(|| async {
            std::hint::black_box(
                conn.query("SELECT id FROM bench_ps_cache WHERE id = $1", &[&42_i64])
                    .await
                    .expect("query unprepared"),
            )
        })
    });

    group.finish();

    rt.block_on(conn.execute_batch("DROP TABLE IF EXISTS bench_ps_cache;"))
        .expect("cleanup");
}

// ── criterion wiring ──────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_value_to_param_int,
    bench_value_to_param_text,
    bench_value_to_param_float,
    bench_value_to_param_null,
    bench_value_to_param_bool,
    bench_value_to_param_all_types,
    bench_batch_row_conversion,
    bench_query_throughput,
    bench_pool_vs_single_connection,
    bench_copy_vs_insert,
    bench_arc_mutex_contention,
    bench_prepared_stmt_cache,
);
criterion_main!(benches);
