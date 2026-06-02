use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use oxisql_core::Connection;
use oxisql_embedded::EmbeddedConnection;

fn bench_simple_select(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let conn = rt.block_on(async {
        let conn = EmbeddedConnection::open_memory().expect("open_memory");
        conn.execute("CREATE TABLE bench_t (id INT, val TEXT)", &[])
            .await
            .expect("create table");
        for i in 0..100_i64 {
            conn.execute(
                "INSERT INTO bench_t VALUES ($1, $2)",
                &[&i, &format!("v{i}") as &dyn oxisql_core::ToSqlValue],
            )
            .await
            .expect("insert row");
        }
        conn
    });

    c.bench_function("simple_select_100_rows", |b| {
        let conn = conn.clone();
        b.to_async(&rt).iter(|| {
            let conn = conn.clone();
            async move {
                std::hint::black_box(
                    conn.query("SELECT id, val FROM bench_t", &[])
                        .await
                        .unwrap(),
                )
            }
        })
    });

    c.bench_function("count_100_rows", |b| {
        let conn = conn.clone();
        b.to_async(&rt).iter(|| {
            let conn = conn.clone();
            async move {
                std::hint::black_box(
                    conn.query("SELECT COUNT(*) FROM bench_t", &[])
                        .await
                        .unwrap(),
                )
            }
        })
    });
}

fn bench_insert_single_row(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let conn = rt.block_on(async {
        let conn = EmbeddedConnection::open_memory().expect("open_memory");
        conn.execute("CREATE TABLE bench_insert (id INT, name TEXT)", &[])
            .await
            .expect("create table");
        conn
    });

    let mut counter = 0i64;
    c.bench_function("insert_single_row", |b| {
        let conn = conn.clone();
        b.to_async(&rt).iter(|| {
            counter += 1;
            let id = counter;
            let conn = conn.clone();
            async move {
                std::hint::black_box(
                    conn.execute(
                        "INSERT INTO bench_insert VALUES ($1, $2)",
                        &[&id, &"bench_name" as &dyn oxisql_core::ToSqlValue],
                    )
                    .await
                    .unwrap(),
                )
            }
        })
    });
}

fn bench_query_empty_table(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    let conn = rt.block_on(async {
        let conn = EmbeddedConnection::open_memory().expect("open_memory");
        conn.execute("CREATE TABLE bench_empty (id INT)", &[])
            .await
            .expect("create table");
        conn
    });

    c.bench_function("select_empty_table", |b| {
        let conn = conn.clone();
        b.to_async(&rt).iter(|| {
            let conn = conn.clone();
            async move {
                std::hint::black_box(conn.query("SELECT id FROM bench_empty", &[]).await.unwrap())
            }
        })
    });
}

fn bench_param_binding(c: &mut Criterion) {
    use oxisql_core::Value;
    use oxisql_embedded::{bind_params, bind_params_string};

    let sql = "INSERT INTO t (a, b, c) VALUES ($1, $2, $3)";
    let params = vec![
        Value::I64(42),
        Value::Text("hello".into()),
        Value::Bool(true),
    ];

    c.bench_function("bind_params_ast", |b| {
        b.iter(|| std::hint::black_box(bind_params(sql, &params).unwrap()))
    });

    c.bench_function("bind_params_string_fallback", |b| {
        b.iter(|| std::hint::black_box(bind_params_string(sql, &params).unwrap()))
    });
}

fn bench_mutex_contention(c: &mut Criterion) {
    use oxisql_core::Connection;
    use std::sync::Arc;

    let mut group = c.benchmark_group("mutex_contention");

    for n_tasks in [1_usize, 4, 8] {
        group.bench_with_input(
            criterion::BenchmarkId::new("concurrent_queries", n_tasks),
            &n_tasks,
            |b, &n| {
                let rt = tokio::runtime::Runtime::new().unwrap();

                // All clones share the same Arc<Mutex<Glue<MemoryStorage>>>,
                // so every query serialises through one mutex — this is the
                // contention we are profiling.
                let base = EmbeddedConnection::open_memory().unwrap();
                rt.block_on(async {
                    base.execute("CREATE TABLE mc_t (id INT)", &[])
                        .await
                        .unwrap();
                });
                let base = Arc::new(base);

                b.to_async(&rt).iter(|| {
                    let base = Arc::clone(&base);
                    async move {
                        let handles: Vec<_> = (0..n)
                            .map(|_| {
                                let conn = (*base).clone();
                                tokio::spawn(async move {
                                    std::hint::black_box(
                                        conn.query("SELECT id FROM mc_t", &[]).await.unwrap(),
                                    )
                                })
                            })
                            .collect();
                        for h in handles {
                            let _ = h.await;
                        }
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark GlueSQL memory behaviour for large tables.
///
/// Measures insert throughput, `COUNT(*)`, and `SELECT * LIMIT 100` for
/// tables containing 1 000, 10 000, and 100 000 rows.  100k is the upper
/// bound chosen for timing stability; 1M rows would make benchmark runs
/// impractically long with GlueSQL MemoryStorage.
///
/// Each iteration creates a fresh in-memory connection so the benchmark
/// captures the full cost (table creation + inserts + query) rather than
/// only the steady-state query time.
fn bench_large_table(c: &mut Criterion) {
    use oxisql_core::{ToSqlValue, Value};

    let mut group = c.benchmark_group("large_table");
    // Fewer samples per iteration — each iteration is expensive at 100k rows.
    group.sample_size(10);

    for row_count in [1_000_usize, 10_000, 100_000] {
        // ── insert benchmark ─────────────────────────────────────────────────
        group.bench_with_input(
            BenchmarkId::new("insert_rows", row_count),
            &row_count,
            |b, &n| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                b.to_async(&rt).iter(|| async move {
                    let conn = EmbeddedConnection::open_memory().unwrap();
                    conn.execute("CREATE TABLE bench_large_t (id INTEGER, v TEXT)", &[])
                        .await
                        .unwrap();
                    for i in 0..n {
                        conn.execute(
                            "INSERT INTO bench_large_t VALUES ($1, $2)",
                            &[
                                &Value::I64(i as i64) as &dyn ToSqlValue,
                                &Value::Text(format!("val_{i}")) as &dyn ToSqlValue,
                            ],
                        )
                        .await
                        .unwrap();
                    }
                    std::hint::black_box(n)
                });
            },
        );

        // ── COUNT(*) benchmark ───────────────────────────────────────────────
        group.bench_with_input(
            BenchmarkId::new("count_rows", row_count),
            &row_count,
            |b, &n| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                // Pre-populate once; we only benchmark the COUNT query.
                let conn = rt.block_on(async {
                    let c = EmbeddedConnection::open_memory().unwrap();
                    c.execute("CREATE TABLE bench_count_t (id INTEGER, v TEXT)", &[])
                        .await
                        .unwrap();
                    for i in 0..n {
                        c.execute(
                            "INSERT INTO bench_count_t VALUES ($1, $2)",
                            &[
                                &Value::I64(i as i64) as &dyn ToSqlValue,
                                &Value::Text(format!("val_{i}")) as &dyn ToSqlValue,
                            ],
                        )
                        .await
                        .unwrap();
                    }
                    c
                });

                b.to_async(&rt).iter(|| {
                    let conn = conn.clone();
                    async move {
                        std::hint::black_box(
                            conn.query("SELECT COUNT(*) FROM bench_count_t", &[])
                                .await
                                .unwrap(),
                        )
                    }
                });
            },
        );

        // ── SELECT * LIMIT 100 benchmark ─────────────────────────────────────
        group.bench_with_input(
            BenchmarkId::new("select_limit100", row_count),
            &row_count,
            |b, &n| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                // Pre-populate once; we only benchmark the SELECT query.
                let conn = rt.block_on(async {
                    let c = EmbeddedConnection::open_memory().unwrap();
                    c.execute("CREATE TABLE bench_limit_t (id INTEGER, v TEXT)", &[])
                        .await
                        .unwrap();
                    for i in 0..n {
                        c.execute(
                            "INSERT INTO bench_limit_t VALUES ($1, $2)",
                            &[
                                &Value::I64(i as i64) as &dyn ToSqlValue,
                                &Value::Text(format!("val_{i}")) as &dyn ToSqlValue,
                            ],
                        )
                        .await
                        .unwrap();
                    }
                    c
                });

                b.to_async(&rt).iter(|| {
                    let conn = conn.clone();
                    async move {
                        std::hint::black_box(
                            conn.query("SELECT * FROM bench_limit_t LIMIT 100", &[])
                                .await
                                .unwrap(),
                        )
                    }
                });
            },
        );
    }

    group.finish();
}

/// Benchmark persistent redb backend I/O overhead vs pure in-memory mode.
///
/// Measures the cost of 100 sequential INSERTs + a COUNT(*) query in both
/// memory (GlueSQL `MemoryStorage`) and persistent (redb) backends.  Each
/// iteration creates a fresh connection so the full setup cost is included.
/// `sample_size(10)` is used because file I/O makes iterations slow.
#[cfg(feature = "redb-storage")]
fn bench_persistent_vs_memory(c: &mut Criterion) {
    use oxisql_core::{ToSqlValue, Value};
    use oxisql_embedded::RedbEmbeddedConnection;

    let mut group = c.benchmark_group("persistent_vs_memory");
    group.sample_size(10); // file I/O is slow

    // Baseline: in-memory INSERT + SELECT
    group.bench_function("memory_insert_100", |b| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        b.to_async(&rt).iter(|| async {
            let conn = EmbeddedConnection::open_memory().expect("open_memory");
            conn.execute("CREATE TABLE bench_pvm (id INTEGER, v TEXT)", &[])
                .await
                .expect("create table");
            for i in 0_i64..100 {
                conn.execute(
                    "INSERT INTO bench_pvm VALUES ($1, $2)",
                    &[
                        &Value::I64(i) as &dyn ToSqlValue,
                        &Value::Text(format!("val_{i}")) as &dyn ToSqlValue,
                    ],
                )
                .await
                .expect("insert");
            }
            let rows = conn
                .query("SELECT COUNT(*) FROM bench_pvm", &[])
                .await
                .expect("count");
            std::hint::black_box(rows)
        });
    });

    // Persistent: redb INSERT + COUNT
    group.bench_function("redb_insert_100", |b| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        b.to_async(&rt).iter(|| async {
            let path = std::env::temp_dir().join(format!(
                "bench_redb_{}.db",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .subsec_nanos()
            ));
            let conn = RedbEmbeddedConnection::open(&path).expect("open_redb");
            conn.execute("CREATE TABLE bench_pvm (id INTEGER, v TEXT)", &[])
                .await
                .expect("create table");
            for i in 0_i64..100 {
                conn.execute(
                    "INSERT INTO bench_pvm VALUES ($1, $2)",
                    &[
                        &Value::I64(i) as &dyn ToSqlValue,
                        &Value::Text(format!("val_{i}")) as &dyn ToSqlValue,
                    ],
                )
                .await
                .expect("insert");
            }
            let rows = conn
                .query("SELECT COUNT(*) FROM bench_pvm", &[])
                .await
                .expect("count");
            let _ = std::fs::remove_file(&path);
            std::hint::black_box(rows)
        });
    });

    group.finish();
}

#[cfg(not(feature = "redb-storage"))]
criterion_group!(
    benches,
    bench_simple_select,
    bench_insert_single_row,
    bench_query_empty_table,
    bench_param_binding,
    bench_mutex_contention,
    bench_large_table,
);

#[cfg(feature = "redb-storage")]
criterion_group!(
    benches,
    bench_simple_select,
    bench_insert_single_row,
    bench_query_empty_table,
    bench_param_binding,
    bench_mutex_contention,
    bench_large_table,
    bench_persistent_vs_memory,
);

criterion_main!(benches);
