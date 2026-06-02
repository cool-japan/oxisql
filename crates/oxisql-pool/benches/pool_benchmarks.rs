use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use oxisql_pool::embedded::EmbeddedPool;
use oxisql_pool::OxidbPool;

fn bench_embedded_pool_concurrent(c: &mut Criterion) {
    let mut group = c.benchmark_group("embedded_pool_concurrent");

    for n_tasks in [1usize, 4, 16] {
        group.bench_with_input(BenchmarkId::new("tasks", n_tasks), &n_tasks, |b, &n| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let pool = EmbeddedPool::new();

            b.to_async(&rt).iter(|| async {
                let pool = pool.clone();
                let handles: Vec<_> = (0..n)
                    .map(|_| {
                        let pool = pool.clone();
                        tokio::spawn(async move {
                            let _guard = pool.get().await.unwrap();
                            // Simulate brief work
                            std::hint::black_box(42u64)
                        })
                    })
                    .collect();

                for h in handles {
                    h.await.unwrap();
                }
            })
        });
    }

    group.finish();
}

fn bench_embedded_pool_get(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = EmbeddedPool::new();

    c.bench_function("embedded_pool_get_and_release", |b| {
        b.to_async(&rt).iter(|| async {
            let _guard = pool.get().await.unwrap();
            // _guard drops here, releasing lock
        })
    });
}

fn bench_embedded_pool_clone(c: &mut Criterion) {
    let pool = EmbeddedPool::new();

    c.bench_function("embedded_pool_clone", |b| {
        b.iter(|| std::hint::black_box(pool.clone()))
    });
}

fn bench_pool_health_check(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let pool = OxidbPool::Embedded(EmbeddedPool::new());

    c.bench_function("pool_health_check_embedded", |b| {
        b.to_async(&rt).iter(|| async {
            pool.health_check().await.unwrap();
        })
    });
}

/// Benchmark deadpool-postgres connection checkout latency.
///
/// Requires a live PostgreSQL server and the `postgres` feature.
/// Set `POSTGRES_URL` to opt in; the bench silently skips when absent.
///
/// Run with:
/// ```sh
/// POSTGRES_URL="postgresql://user:pass@localhost/db" \
///     cargo bench -p oxisql-pool --features postgres -- pg_pool_checkout
/// ```
#[cfg(feature = "postgres")]
fn bench_pg_pool_checkout(c: &mut Criterion) {
    use oxisql_pool::postgres::OxidbPgPool;

    let url = match std::env::var("POSTGRES_URL") {
        Ok(u) => u,
        // No server configured — register a no-op iteration so criterion
        // doesn't complain about an empty group, and return immediately.
        Err(_) => {
            c.bench_function("pg_pool_checkout/no_server", |b| b.iter(|| ()));
            return;
        }
    };

    let mut group = c.benchmark_group("pg_pool_checkout");
    // Reduce sample size: each iteration requires a real network round-trip.
    group.sample_size(10);

    group.bench_function("checkout_and_release", |b| {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        let pool = rt.block_on(async { OxidbPgPool::try_from_url(&url).expect("build pg pool") });

        b.to_async(&rt).iter(|| async {
            // Checkout a connection, issue a lightweight probe, then release.
            let conn = pool.get().await.expect("pool checkout");
            // Use the connection so the compiler doesn't elide the checkout.
            std::hint::black_box(&conn);
            // conn is returned to the pool on drop.
        });
    });

    group.finish();
}

/// No-op stub compiled when the `postgres` feature is disabled so that
/// `criterion_group!` can always reference `bench_pg_pool_checkout`.
#[cfg(not(feature = "postgres"))]
fn bench_pg_pool_checkout(_c: &mut Criterion) {}

criterion_group!(
    benches,
    bench_embedded_pool_concurrent,
    bench_embedded_pool_get,
    bench_embedded_pool_clone,
    bench_pool_health_check,
    bench_pg_pool_checkout
);
criterion_main!(benches);
