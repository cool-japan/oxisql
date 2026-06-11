use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use limbo_core::{Database, PlatformIO, IO};
use pprof::criterion::{Output, PProfProfiler};
use std::sync::Arc;

fn bench_prepare_query(criterion: &mut Criterion) {
    #[allow(clippy::arc_with_non_send_sync)]
    let io = Arc::new(PlatformIO::new().unwrap());
    let db = Database::open_file(io.clone(), "../testing/testing.db", false).unwrap();
    let limbo_conn = db.connect().unwrap();

    let queries = [
        "SELECT 1",
        "SELECT * FROM users LIMIT 1",
        "SELECT first_name, count(1) FROM users GROUP BY first_name HAVING count(1) > 1 ORDER BY count(1)  LIMIT 1",
    ];

    for query in queries.iter() {
        let mut group = criterion.benchmark_group(format!("Prepare `{}`", query));

        group.bench_with_input(
            BenchmarkId::new("limbo_parse_query", query),
            query,
            |b, query| {
                b.iter(|| {
                    limbo_conn.prepare(query).unwrap();
                });
            },
        );

        group.finish();
    }
}

fn bench_execute_select_rows(criterion: &mut Criterion) {
    #[allow(clippy::arc_with_non_send_sync)]
    let io = Arc::new(PlatformIO::new().unwrap());
    let db = Database::open_file(io.clone(), "../testing/testing.db", false).unwrap();
    let limbo_conn = db.connect().unwrap();

    let mut group = criterion.benchmark_group("Execute `SELECT * FROM users LIMIT ?`");

    for i in [1, 10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::new("limbo_execute_select_rows", i),
            &i,
            |b, i| {
                // TODO: LIMIT doesn't support query parameters.
                let mut stmt = limbo_conn
                    .prepare(format!("SELECT * FROM users LIMIT {}", *i))
                    .unwrap();
                let io = io.clone();
                b.iter(|| {
                    loop {
                        match stmt.step().unwrap() {
                            limbo_core::StepResult::Row => {
                                black_box(stmt.row());
                            }
                            limbo_core::StepResult::IO => {
                                let _ = io.run_once();
                            }
                            limbo_core::StepResult::Done => {
                                break;
                            }
                            limbo_core::StepResult::Interrupt | limbo_core::StepResult::Busy => {
                                unreachable!();
                            }
                        }
                    }
                    stmt.reset();
                });
            },
        );
    }

    group.finish();
}

fn bench_execute_select_1(criterion: &mut Criterion) {
    #[allow(clippy::arc_with_non_send_sync)]
    let io = Arc::new(PlatformIO::new().unwrap());
    let db = Database::open_file(io.clone(), "../testing/testing.db", false).unwrap();
    let limbo_conn = db.connect().unwrap();

    let mut group = criterion.benchmark_group("Execute `SELECT 1`");

    group.bench_function("limbo_execute_select_1", |b| {
        let mut stmt = limbo_conn.prepare("SELECT 1").unwrap();
        let io = io.clone();
        b.iter(|| {
            loop {
                match stmt.step().unwrap() {
                    limbo_core::StepResult::Row => {
                        black_box(stmt.row());
                    }
                    limbo_core::StepResult::IO => {
                        let _ = io.run_once();
                    }
                    limbo_core::StepResult::Done => {
                        break;
                    }
                    limbo_core::StepResult::Interrupt | limbo_core::StepResult::Busy => {
                        unreachable!();
                    }
                }
            }
            stmt.reset();
        });
    });

    group.finish();
}

fn bench_execute_select_count(criterion: &mut Criterion) {
    #[allow(clippy::arc_with_non_send_sync)]
    let io = Arc::new(PlatformIO::new().unwrap());
    let db = Database::open_file(io.clone(), "../testing/testing.db", false).unwrap();
    let limbo_conn = db.connect().unwrap();

    let mut group = criterion.benchmark_group("Execute `SELECT count() FROM users`");

    group.bench_function("limbo_execute_select_count", |b| {
        let mut stmt = limbo_conn.prepare("SELECT count() FROM users").unwrap();
        let io = io.clone();
        b.iter(|| {
            loop {
                match stmt.step().unwrap() {
                    limbo_core::StepResult::Row => {
                        black_box(stmt.row());
                    }
                    limbo_core::StepResult::IO => {
                        let _ = io.run_once();
                    }
                    limbo_core::StepResult::Done => {
                        break;
                    }
                    limbo_core::StepResult::Interrupt | limbo_core::StepResult::Busy => {
                        unreachable!();
                    }
                }
            }
            stmt.reset();
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(100, Output::Flamegraph(None)));
    targets = bench_prepare_query, bench_execute_select_1, bench_execute_select_rows, bench_execute_select_count
}
criterion_main!(benches);
