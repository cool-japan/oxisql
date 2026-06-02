use criterion::{criterion_group, criterion_main, Criterion};
use oxisql_parse::{parse, parse_one, plan_statement, Optimizer};

// ── Parse throughput benchmarks ───────────────────────────────────────────────

fn bench_parse_simple(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse_throughput");

    group.bench_function("simple_select", |b| {
        b.iter(|| {
            std::hint::black_box(
                parse("SELECT id, name FROM users WHERE id = 1").expect("valid SQL"),
            )
        })
    });

    group.bench_function("complex_join", |b| {
        b.iter(|| {
            std::hint::black_box(
                parse(
                    "SELECT u.id, u.name, o.total \
                     FROM users u \
                     JOIN orders o ON u.id = o.user_id \
                     WHERE u.active = true \
                     ORDER BY o.total DESC \
                     LIMIT 10",
                )
                .expect("valid SQL"),
            )
        })
    });

    group.bench_function("insert_values", |b| {
        b.iter(|| {
            std::hint::black_box(
                parse(
                    "INSERT INTO orders (user_id, total, created_at) \
                     VALUES (1, 99.99, '2024-01-01')",
                )
                .expect("valid SQL"),
            )
        })
    });

    group.bench_function("aggregate_groupby", |b| {
        b.iter(|| {
            std::hint::black_box(
                parse(
                    "SELECT department, COUNT(*), AVG(salary) \
                     FROM employees \
                     GROUP BY department \
                     HAVING COUNT(*) > 5 \
                     ORDER BY AVG(salary) DESC",
                )
                .expect("valid SQL"),
            )
        })
    });

    group.bench_function("cte_window_function", |b| {
        b.iter(|| {
            std::hint::black_box(
                parse(
                    "WITH ranked AS ( \
                         SELECT id, name, salary, \
                                RANK() OVER (PARTITION BY dept ORDER BY salary DESC) AS rnk \
                         FROM employees \
                     ) \
                     SELECT id, name, salary FROM ranked WHERE rnk = 1",
                )
                .expect("valid SQL"),
            )
        })
    });

    group.finish();
}

// ── Query planning overhead benchmarks ────────────────────────────────────────

fn bench_plan_overhead(c: &mut Criterion) {
    let mut group = c.benchmark_group("plan_overhead");

    // Pre-parse statements so we only measure planning time.
    let simple_stmt = parse_one("SELECT id FROM users WHERE id = 1").expect("valid SQL");
    let join_stmt = parse_one(
        "SELECT u.id, u.name, o.total \
         FROM users u \
         JOIN orders o ON u.id = o.user_id \
         WHERE u.active = true \
         LIMIT 10",
    )
    .expect("valid SQL");
    let agg_stmt = parse_one(
        "SELECT department, COUNT(*), AVG(salary) \
         FROM employees \
         GROUP BY department \
         HAVING COUNT(*) > 5",
    )
    .expect("valid SQL");

    group.bench_function("plan_simple_select", |b| {
        b.iter(|| std::hint::black_box(plan_statement(&simple_stmt).expect("valid plan")))
    });

    group.bench_function("plan_join_with_limit", |b| {
        b.iter(|| std::hint::black_box(plan_statement(&join_stmt).expect("valid plan")))
    });

    group.bench_function("plan_aggregate_having", |b| {
        b.iter(|| std::hint::black_box(plan_statement(&agg_stmt).expect("valid plan")))
    });

    // End-to-end: parse + plan together (realistic usage).
    group.bench_function("parse_and_plan_simple", |b| {
        b.iter(|| {
            let stmt = parse_one("SELECT id FROM users WHERE id = 1 LIMIT 10").expect("valid SQL");
            std::hint::black_box(plan_statement(&stmt).expect("valid plan"))
        })
    });

    group.finish();
}

// ── Optimizer pass-chain benchmarks ───────────────────────────────────────────

fn bench_optimizer_chain(c: &mut Criterion) {
    let mut group = c.benchmark_group("optimizer_pass_chain");

    // Build plans once, then measure optimizer cost repeatedly.
    let simple_plan =
        plan_statement(&parse_one("SELECT id FROM users WHERE id = 1").expect("valid SQL"))
            .expect("valid plan");
    let join_plan = plan_statement(
        &parse_one(
            "SELECT u.id, u.name, o.total \
             FROM users u \
             JOIN orders o ON u.id = o.user_id \
             WHERE u.active = true \
             ORDER BY o.total DESC \
             LIMIT 10",
        )
        .expect("valid SQL"),
    )
    .expect("valid plan");
    let agg_plan = plan_statement(
        &parse_one(
            "SELECT department, COUNT(*), AVG(salary) \
             FROM employees \
             GROUP BY department \
             HAVING COUNT(*) > 5",
        )
        .expect("valid SQL"),
    )
    .expect("valid plan");

    group.bench_function("optimize_simple_select", |b| {
        let opt = Optimizer::default();
        b.iter(|| std::hint::black_box(opt.optimize(simple_plan.clone())))
    });

    group.bench_function("optimize_join_query", |b| {
        let opt = Optimizer::default();
        b.iter(|| std::hint::black_box(opt.optimize(join_plan.clone())))
    });

    group.bench_function("optimize_aggregate_query", |b| {
        let opt = Optimizer::default();
        b.iter(|| std::hint::black_box(opt.optimize(agg_plan.clone())))
    });

    // Full pipeline: parse → plan → optimize.
    group.bench_function("full_pipeline_simple", |b| {
        b.iter(|| {
            let stmt = parse_one("SELECT id FROM users WHERE id = 1 LIMIT 10").expect("valid SQL");
            let plan = plan_statement(&stmt).expect("valid plan");
            let opt = Optimizer::default();
            std::hint::black_box(opt.optimize(plan))
        })
    });

    group.bench_function("full_pipeline_complex", |b| {
        b.iter(|| {
            let stmt = parse_one(
                "SELECT u.id, u.name, o.total \
                 FROM users u \
                 JOIN orders o ON u.id = o.user_id \
                 WHERE u.active = true \
                 ORDER BY o.total DESC \
                 LIMIT 10",
            )
            .expect("valid SQL");
            let plan = plan_statement(&stmt).expect("valid plan");
            let opt = Optimizer::default();
            std::hint::black_box(opt.optimize(plan))
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_parse_simple,
    bench_plan_overhead,
    bench_optimizer_chain
);
criterion_main!(benches);
