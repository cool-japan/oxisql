use criterion::{criterion_group, criterion_main, Criterion};
use oxisql_migrate::runner::MigrationRunner;
use oxisql_migrate::scanner::scan_migrations;
use std::env;

fn bench_scan_migrations(c: &mut Criterion) {
    // Create a temp dir with 20 migration files using 14-digit timestamp names.
    let dir = env::temp_dir().join("oxisql_bench_scan_migrate");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 1u64..=20 {
        // 14-digit timestamp: base 20230101000000 + i
        let version = 20_230_101_000_000u64 + i;
        std::fs::write(
            dir.join(format!("{version}__step_{i}.sql")),
            format!("CREATE TABLE scan_t{i} (id INT);"),
        )
        .unwrap();
    }

    c.bench_function("scan_20_migration_files", |b| {
        b.iter(|| {
            let files = scan_migrations(&dir).unwrap();
            std::hint::black_box(files)
        })
    });

    // Cleanup after bench — ignore errors if dir is already gone.
    std::fs::remove_dir_all(&dir).ok();
}

fn bench_apply_migrations(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("apply_5_ddl_migrations", |b| {
        b.to_async(&rt).iter(|| async {
            use oxisql_pool::embedded::EmbeddedPool;

            // Use nanos for a unique suffix per iteration to avoid leftover files
            // from a previous iteration colliding with the tracker state.
            let suffix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos();
            let dir = env::temp_dir().join(format!("oxisql_bench_apply_{suffix}"));
            std::fs::create_dir_all(&dir).unwrap();

            for i in 1u64..=5 {
                let version = 20_230_101_000_000u64 + i;
                std::fs::write(
                    dir.join(format!("{version}__bench{i}.sql")),
                    format!("CREATE TABLE apply_bench_{suffix}_{i} (id INT);"),
                )
                .unwrap();
            }

            let pool = EmbeddedPool::new();
            let runner = MigrationRunner::new(dir.to_str().unwrap());
            let n = runner.run_pooled(&pool).await.unwrap();
            std::hint::black_box(n);

            std::fs::remove_dir_all(&dir).ok();
        })
    });
}

criterion_group!(benches, bench_scan_migrations, bench_apply_migrations);
criterion_main!(benches);
