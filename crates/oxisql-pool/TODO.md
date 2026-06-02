# oxidb-pool TODO

## Status
Pure Rust core, `forbid(unsafe_code)`. Three pool backends behind opt-in features: `postgres` (deadpool-postgres wrapper), `mysql` (custom `deadpool::managed::Manager` over `mysql_async::Conn`), and `embedded` (Arc<Mutex<Glue<MemoryStorage>>> no-op pool). Unified `OxidbPool` enum spans all enabled backends with `health_check()` and `metrics()` dispatch. `PoolError` covers Postgres pool/create errors and MySQL pool/URL errors. Integration tests exercise embedded pool DDL/DML and Postgres/MySQL connection (when services are available).

## Core Implementation
- [x] Implement `lock_sync()` on `EmbeddedPool` properly using `std::sync::Mutex` as a secondary wrapper or `tokio::sync::Mutex::blocking_lock()` instead of panicking (~20 SLOC refactor) — resolved: pool design uses tokio::sync::Mutex directly, no sync wrapper needed
- [x] Add pool health-check method: `OxidbPool::health_check() -> Result<(), PoolError>` dispatching to backend-specific ping (SELECT 1 for PG/MySQL, no-op for embedded) (~40 SLOC)
- [x] Add pool metrics: `OxidbPool::metrics() -> PoolMetrics` struct with `max_size`, `active`, `idle`, `wait_count` fields, populated from deadpool Status (~50 SLOC)
- [x] Add SQLite pool backend via `deadpool` + `rusqlite` (feature-gated `sqlite`): `SqliteManager` implementing `deadpool::managed::Manager` for file-backed SQLite connections (~90 SLOC)
- [x] Add pool configuration builder: `PoolConfig` / `PoolConfigBuilder` with `max_size`, `min_idle`, `connect_timeout_ms`, `idle_timeout_ms` settings (~80 SLOC)
- [x] Add connection lifecycle hooks: `on_create`, `on_checkout`, `on_checkin` callback registration for observability and setup (e.g. SET search_path) (~60 SLOC)
- [x] Add `EmbeddedPool::execute(&self, sql: &str) -> Result<u64, PoolError>` convenience method that checks out a connection, runs the query, and returns it (~40 SLOC)
- [x] Add pool shutdown: `EmbeddedPool::close()` (AtomicBool guard) + no-op `close()` on postgres/mysql wrappers that prevents new checkouts (~20 SLOC)

## API Improvements
- [x] Implement `Display` and proper `Error` source chaining for `PoolError` using `thiserror` instead of manual impls (~-10 SLOC net, better diagnostics)
- [x] Add `backend_name() -> &'static str` on each pool type returning "postgres"/"mysql"/"embedded" (~15 SLOC)
- [x] Add `From<Config>` impl for `OxidbPgPool` to simplify construction from deadpool-postgres Config without requiring explicit Runtime (~10 SLOC) — added `TryFrom<Config>` and `try_from_url(url: &str)` methods
- [x] Fix `new_mysql_pool` error mapping: introduce `PoolError::Build(String)` variant, map `BuildError` cleanly without synthetic `mysql_async::Error::Other` (~20 SLOC)
- [x] Add `Clone` derive for `OxidbPgPool` (wrap inner pool in Arc) to match `EmbeddedPool::Clone` (~10 SLOC)

## Testing
- [x] Add `embedded_pool_close_prevents_checkout`: close pool, verify get() returns Err; clones share flag
- [x] Add `embedded_pool_backend_name`: assert "embedded"
- [x] Add `pool_config_builder_defaults` / `pool_config_builder_custom` / `pool_config_builder_idle_timeout`
- [x] Add `embedded_pool_execute_convenience` / `embedded_pool_execute_after_close_fails`
- [x] Add embedded pool concurrent access test: spawn 8 tokio tasks, each acquiring the lock and running DDL/DML, verify serialization (~50 SLOC)
- [x] Add embedded pool migration integration test: run `oxidb-migrate` through an `EmbeddedPool` handle (~40 SLOC)
- [x] Add `pool_hooks_debug_format` / `embedded_pool_checkout_hook_fires` hook tests (~30 SLOC)
- [x] Add pool exhaustion test: `embedded_pool_exhaustion_simulation` — hold single lock, verify second checkout blocks, then succeeds after release (~35 SLOC, embedded backend)
- [x] Add pool sequential access test: `embedded_pool_sequential_access` — verify data written in first checkout is visible to second checkout (~25 SLOC)
- [x] Add MySQL URL parsing edge cases: `test_mysql_url_empty_returns_err` / `test_mysql_url_missing_scheme_returns_err` — empty URL and missing scheme return Err without panicking (~20 SLOC)
- [x] Add health-check test for embedded backend: `embedded_pool_health_check_open` / `embedded_pool_health_check_closed` — Ok on fresh pool, Err on closed pool
- [x] Add metrics accuracy test: `embedded_pool_metrics_open` / `embedded_pool_metrics_closed` — max_size=1, idle=1/0 depending on open/closed state

## Performance
- [x] Add criterion benchmark: embedded pool lock contention — `pool_benchmarks.rs` covers get/release, clone, and health_check for the embedded pool backend (~35 SLOC)
- [x] Add criterion benchmark: embedded pool lock contention under 1/4/16 concurrent tasks — `bench_embedded_pool_concurrent` in `pool_benchmarks.rs` uses `BenchmarkId` and spawns N tokio tasks to measure mutex serialisation overhead (~40 SLOC)
- [x] Add criterion benchmark: deadpool-postgres checkout latency (requires local PG, gated) (~50 SLOC) — `bench_pg_pool_checkout` in `benches/pool_benchmarks.rs`; skips gracefully when `POSTGRES_URL` is unset; `#[cfg(not(feature = "postgres"))]` no-op stub ensures bench compiles under `embedded`-only feature set
- [x] Evaluate replacing `Arc<Mutex<Glue<MemoryStorage>>>` with `tokio::sync::RwLock` for read-heavy embedded workloads where GlueSQL queries are read-only (~30 SLOC refactor + benchmark) — evaluated — not feasible: GlueSQL Glue::execute requires &mut self for all operations including SELECT; RwLock cannot provide concurrent reads

## Integration
- [x] Wire into `oxidb` facade: re-export `OxidbPool`, `OxidbPgPool`, `MysqlPool`, `EmbeddedPool` under `oxidb::pool::*` behind respective features (~20 SLOC) — `oxisql::pool` now re-exports all pool types; `connect_pool()` function dispatches by URI
- [x] Add `oxidb-migrate` integration: accept `&OxidbPool` in `MigrationRunner` to run migrations against pooled connections — `run_with_pool(&OxidbPool)` in `oxisql-migrate` behind `pool` feature (~30 SLOC bridge)
- [x] Add `oxidb-query` integration: `EmbeddedPool::execute_query_builder(qb: &QueryBuilder)` (feature `query-builder`) for running `QueryBuilder` queries through the embedded pool (~40 SLOC)
- [x] Add `oxistore` integration: implement `KvStore` trait backed by `OxidbPool` (PG or MySQL) for SQL-backed key-value storage (~100 SLOC) — `EmbeddedKvStore` (backed by `EmbeddedPool`) and `OxidbKvStore` (wraps `Arc<OxidbPool>`, dispatches per variant) in `kv_store.rs`; 11 tests, all green
- [x] Migrate `sqlite` feature to Pure-Rust backend: `sqlite` feature now uses `oxisql-sqlite-compat` (Limbo engine) instead of `rusqlite` (C-FFI). The `sqlite-compat` feature is a transitional alias for `sqlite`. The old C-backend is preserved under `sqlite-rusqlite` for explicit opt-in. Zero C deps when `sqlite` feature is enabled. All 8 sqlite pool tests updated to async constructor and verified green.

## Future improvements
- [x] Delete orphaned sqlite.rs (ungated rusqlite dead code — `mod sqlite;` was never declared; 13 C-FFI references removed)
- [x] impl ConnectionPool for OxidbPgPool + PgPooledConn/PgPooledTxn/PgPooledPrepared (postgres.rs, ~430 SLOC added)
- [x] impl ConnectionPool for MysqlPool + MysqlPooledConn/MysqlPooledTxn (mysql.rs, ~390 SLOC added)
