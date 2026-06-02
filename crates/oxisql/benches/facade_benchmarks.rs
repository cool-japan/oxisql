//! Benchmark the facade dispatch overhead: `Box<dyn Connection>` vs direct backend calls.

use criterion::{criterion_group, criterion_main, Criterion};

/// Benchmark cold connection creation through the facade.
///
/// Each iteration creates a fresh [`EmbeddedConnection`] via `oxisql::connect("memory://")`.
/// This measures the overhead of URI dispatch + GlueSQL MemoryStorage init.
fn bench_connect_memory(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    c.bench_function("connect_memory_cold", |b| {
        b.to_async(&rt).iter(|| async {
            // Cold connection (new EmbeddedConnection each time)
            std::hint::black_box(
                oxisql::connect("memory://")
                    .await
                    .expect("connect must succeed"),
            )
        })
    });
}

/// Benchmark dynamic dispatch through `Box<dyn Connection>`.
///
/// Measures the per-call overhead of going through the vtable for `execute`
/// and `query` on an already-warm in-memory connection with pre-populated data.
fn bench_dispatch_execute(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    // Build a warm connection with pre-populated data outside the timed loop.
    let conn = rt.block_on(async {
        oxisql::connect("memory://")
            .await
            .expect("connect must succeed")
    });
    rt.block_on(async {
        conn.execute("CREATE TABLE bench_t (id INT, val TEXT)", &[])
            .await
            .expect("CREATE TABLE");
        for i in 0_i64..100 {
            conn.execute(&format!("INSERT INTO bench_t VALUES ({i}, 'v')"), &[])
                .await
                .expect("INSERT");
        }
    });

    c.bench_function("dyn_connection_query", |b| {
        b.to_async(&rt).iter(|| async {
            std::hint::black_box(
                conn.query("SELECT id FROM bench_t LIMIT 10", &[])
                    .await
                    .expect("query must succeed"),
            )
        })
    });
}

/// Benchmark connection establishment time through the facade.
///
/// Each iteration creates a fresh [`EmbeddedConnection`] via `oxisql::connect("memory://")`.
/// A new [`tokio::runtime::Runtime`] is created per iteration to measure cold-path
/// connection establishment including facade URI dispatch and GlueSQL storage init.
fn bench_connection_establishment(c: &mut Criterion) {
    let mut group = c.benchmark_group("connection_establishment");

    group.bench_function("embedded_memory_connect", |b| {
        b.iter(|| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async {
                std::hint::black_box(
                    oxisql::connect("memory://")
                        .await
                        .expect("connect must succeed"),
                )
            })
        })
    });

    group.finish();
}

/// Benchmark pooled vs unpooled connection throughput.
///
/// Compares direct `oxisql::connect("memory://")` query overhead (a new
/// [`EmbeddedConnection`] per iteration) against checking out a connection
/// from a shared [`EmbeddedPool`] and executing the same query.
fn bench_pooled_vs_unpooled(c: &mut Criterion) {
    use oxisql_core::ConnectionPool;
    use oxisql_pool::embedded::EmbeddedPool;

    let mut group = c.benchmark_group("pooled_vs_unpooled");

    // Unpooled: new connection created per iteration (includes GlueSQL init)
    group.bench_function("unpooled_query", |b| {
        b.iter(|| {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async {
                let conn = oxisql::connect("memory://")
                    .await
                    .expect("connect must succeed");
                std::hint::black_box(
                    conn.query("SELECT 1", &[])
                        .await
                        .expect("query must succeed"),
                )
            })
        })
    });

    // Pooled: reuse shared pool, only checkout overhead per iteration
    group.bench_function("pooled_query", |b| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let pool = rt.block_on(async { EmbeddedPool::new() });
        b.iter(|| {
            let rt2 = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt2.block_on(async {
                let conn = <EmbeddedPool as ConnectionPool>::get(&pool)
                    .await
                    .expect("pool checkout must succeed");
                std::hint::black_box(
                    conn.query("SELECT 1", &[])
                        .await
                        .expect("query must succeed"),
                )
            })
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_connect_memory,
    bench_dispatch_execute,
    bench_connection_establishment,
    bench_pooled_vs_unpooled
);
criterion_main!(benches);
