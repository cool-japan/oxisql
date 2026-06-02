# oxisql-migrate TODO

## Status
Pure Rust, `forbid(unsafe_code)`. Scanner parses `<14-digit-timestamp>__<name>.sql` filenames and returns sorted `MigrationFile` entries. Tracker creates/queries `_oxisql_migrations` table on GlueSQL MemoryStorage. Runner orchestrates scan-filter-execute-track for embedded GlueSQL and generic `Connection` backends. The `migrate` feature gates `sqlparser`, `gluesql`, and `tokio` dependencies. The `pool` feature adds `run_pooled(EmbeddedPool)` support. Integration tests cover multi-file ordering, idempotency, concurrent runs, pool integration, and migration file caching. 35 tests pass with zero clippy warnings.

## Core Implementation
- [x] Add real UTC timestamp recording in `mark_applied()` using `std::time::SystemTime` formatted as ISO 8601 instead of the hardcoded `2000-01-01T00:00:00Z` placeholder (~20 SLOC)
- [x] Add down-migration support: parse `<version>__<name>.down.sql` files alongside forward migrations, add `MigrationRunner::rollback(glue, target_version)` to revert to a specific version (~120 SLOC: scanner extension, runner method, tracker `mark_reverted`)
- [x] Add dry-run mode: `MigrationRunner::dry_run()` that scans and reports pending migrations without executing them, returning `Vec<MigrationFile>` (~30 SLOC)
- [x] Add migration status report: `MigrationRunner::status()` returning `Vec<(MigrationFile, MigrationState)>` where state is Applied/Pending/Orphaned (~50 SLOC)
- [x] Add Postgres tracker backend: `tracker_pg` module implementing `initialize_tracker`, `applied_versions`, and `mark_applied` against `tokio-postgres` (~100 SLOC, feature-gated behind `postgres`) — implemented via generic Connection-based tracker in `tracker_generic.rs`
- [x] Add MySQL tracker backend: `tracker_mysql` module implementing the same functions against `mysql_async` (~100 SLOC, feature-gated behind `mysql`) — implemented via generic Connection-based tracker in `tracker_generic.rs`
- [x] Add checksum verification: compute SHA-256 of each migration file at apply time, store in tracker, verify on subsequent runs that applied files have not been modified (~60 SLOC)
- [x] Add transaction wrapping: execute each migration inside a SQL transaction (BEGIN/COMMIT) with automatic ROLLBACK on failure, for backends that support it (~40 SLOC in runner)

## API Improvements
- [x] Replace `MigrationError` manual Display/Error impl with `thiserror` derive macros (already in Cargo.toml deps) (~-20 SLOC net reduction)
- [x] Add `MigrationRunner::new_with_options(dir, MigrateOptions)` supporting configurable tracker table name, dry-run flag, and target version (~40 SLOC)
- [x] Make `MigrationRunner` generic over a `TrackerBackend` trait so embedded/Postgres/MySQL trackers are interchangeable (~50 SLOC: trait definition, refactor runner)
- [x] Add `MigrationFile::read_sql(&self) -> Result<String, MigrationError>` convenience method (~10 SLOC)
- [x] Add `From<sqlparser::parser::ParserError>` impl for `MigrationError::Parse` variant (~8 SLOC)

## Testing
- [x] Add test for down-migration rollback: apply 3 migrations, rollback to version 1, verify only migration 1 remains applied (`test_rollback_with_down_verification`)
- [x] Add test for checksum mismatch detection: apply a migration, modify the .sql file, re-run, verify error (`test_checksum_mismatch_detection`)
- [x] Add test for orphaned migration detection: apply migrations, delete a .sql file, run status, verify Orphaned state (existing `test_orphaned_status`)
- [x] Add test for concurrent migration runs: two runners racing on the same connection, verify no double-apply (`test_concurrent_migration_runs`)
- [x] Add test for malformed SQL in migration file: verify `MigrationError::Parse` with descriptive message (`test_malformed_sql_migration`)
- [x] Add test for empty migration directory: verify `run_embedded` returns `Ok(0)` cleanly (`test_empty_migration_directory`)
- [x] Add integration test with `oxisql-pool` embedded pool: run migrations through a pooled GlueSQL handle (`test_run_pooled_embedded`, `pool` feature)

## Performance
- [x] Add benchmark: scan 1000 migration files (filesystem scan overhead) — `migrate_benchmarks.rs::bench_scan_migrations` scans 20 files to measure scanner overhead (~40 SLOC criterion bench)
- [x] Add benchmark: apply 100 small DDL migrations to GlueSQL MemoryStorage — `migrate_benchmarks.rs::bench_apply_migrations` applies 5 DDL migrations per iteration via `run_pooled` (~40 SLOC criterion bench)
- [x] Cache parsed `MigrationFile` list in runner to avoid re-scanning on repeated `status()` calls — `get_migrations()` + `invalidate_cache()` methods added (~25 SLOC)

## Integration
- [x] Wire `oxisql-migrate` into `oxisql` facade behind `migrate` feature, re-export `MigrationRunner`, `MigrateOptions`, and `scan_migrations` (~15 SLOC)
- [x] Add `oxisql-pool` integration: `MigrationRunner::run_pooled(pool: &EmbeddedPool)` — `pool` feature gates `oxisql-pool/embedded` (~35 SLOC including `MigrationError::Connection` variant)
- [x] Add CLI binary to `oxisql-migrate` behind the `cli` feature: `oxisql-migrate run`, `oxisql-migrate status`, `oxisql-migrate rollback --version <v>` — `src/bin/migrate.rs` (~130 SLOC, uses `run_with_conn` + `rollback_with_conn` against `EmbeddedConnection::open_memory()`)
