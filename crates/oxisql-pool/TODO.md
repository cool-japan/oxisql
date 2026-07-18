# oxisql-pool — TODO

## Status: Stable (0.4.0)

Pure Rust core, `#![forbid(unsafe_code)]`. Four pool backends behind opt-in
features: `postgres` (`deadpool-postgres` wrapper), `mysql` (custom
`deadpool::managed::Manager` over `mysql_async::Conn`), `embedded`
(`Arc<Mutex<Glue<MemoryStorage>>>`), and `sqlite` (Pure-Rust `oxisqlite` engine via
`oxisql-sqlite-compat`). The unified `OxidbPool` enum spans every enabled backend
with `health_check()` and `metrics()` dispatch; each concrete pool also exposes
`backend_name()`. `PoolError` covers Postgres pool/create errors, MySQL pool/URL
errors, and SQLite pool errors. The default feature set is empty.

**Tests: 35 pass with default features; 62 pass with `--all-features`**
(52 integration + 10 doc); **4 ignored** (the ignored tests are live-server-gated
MySQL/Postgres pool tests). Zero clippy warnings.

## Done

### Core implementation
- [x] `OxidbPool::health_check() -> Result<(), PoolError>` dispatching to a backend-specific ping (`SELECT 1` for PG/MySQL, liveness check for embedded/sqlite)
- [x] `OxidbPool::metrics() -> PoolMetrics` with `max_size`, `active`, `idle`, `wait_count`, `acquired_total`, `released_total`, `timeout_count`, populated from the deadpool `Status`
- [x] `PoolConfig` / `PoolConfigBuilder` with `max_size`, `min_idle`, `connect_timeout_ms`, `idle_timeout_ms`
- [x] Connection lifecycle hooks: `on_create`, `on_checkout`, `on_checkin` (`PoolHooks`)
- [x] `EmbeddedPool::execute(&self, sql)` convenience method
- [x] Pool shutdown: `EmbeddedPool::close()` (AtomicBool guard) + no-op `close()` on PG/MySQL wrappers
- [x] Pure-Rust SQLite pool: `sqlite` feature backed by `oxisql-sqlite-compat` (the `oxisqlite` engine); `sqlite-compat` is a transitional alias

### API improvements
- [x] `Display` + `Error` source chaining for `PoolError` via `thiserror`
- [x] `backend_name() -> &'static str` on each pool type (`"postgres"`/`"mysql"`/`"embedded"`/`"sqlite"`)
- [x] `TryFrom<Config>` and `try_from_url(url)` on `OxidbPgPool`
- [x] `PoolError::Build(String)` variant; clean `new_mysql_pool` error mapping
- [x] `Clone` for `OxidbPgPool` (inner pool wrapped in `Arc`) to match `EmbeddedPool`
- [x] `impl ConnectionPool` for `OxidbPgPool` (+ `PgPooledConn`/`PgPooledTxn`/`PgPooledPrepared`)
- [x] `impl ConnectionPool` for `MysqlPool` (+ `MysqlPooledConn`/`MysqlPooledTxn`)

### Integration
- [x] Re-exported under `oxisql::pool::*`; `connect_pool()` dispatches by URI
- [x] `oxisql-migrate` bridge: `MigrationRunner::run_with_pool(&OxidbPool)` (behind `pool`)
- [x] `oxisql-query` bridge: `EmbeddedPool::execute_query_builder(&QueryBuilder)` (behind `query-builder`)
- [x] `kv_store`: `EmbeddedKvStore` + `OxidbKvStore` (SQL-backed key-value store), 11 tests green
- [x] Removed the legacy C-FFI SQLite path: deleted the orphaned `sqlite.rs` (ungated `rusqlite` dead code); the SQLite pool is now 100% Pure Rust

### Testing & performance
- [x] Embedded pool: close-prevents-checkout, concurrent access (8 tasks), exhaustion simulation, sequential visibility, health-check, metrics accuracy
- [x] MySQL URL parsing edge cases (empty / missing scheme return `Err` without panic)
- [x] `PoolConfigBuilder` defaults/custom/idle-timeout tests; `PoolHooks` debug/checkout-fires tests
- [x] Criterion benches: embedded pool get/release, clone, health-check, and 1/4/16-task contention; gated PG checkout latency bench

## Roadmap / next
- [x] Optional `min_idle` pre-warming for the embedded and SQLite pools (eagerly open connections up to `min_idle` at construction) (done 2026-06-10)
  - `new_sqlite_compat_pool_with_config(path, config)` in `sqlite_compat.rs`: holds `min_idle` connections simultaneously post-build to force deadpool to create distinct slots, then releases them all back to the idle pool. Cap at `max_size` to avoid pool exhaustion.
  - `EmbeddedPool::with_config(config)` in `embedded.rs`: stores `config.min_idle` in a `min_idle: usize` field; pre-warming is documented as a no-op (single shared `Arc<Mutex<Glue>>` — no discrete slots to warm).
  - Tests: `test_sqlite_pool_with_config_min_idle`, `test_sqlite_pool_with_config_no_min_idle`, `embedded_pool_with_config_no_op` — all green.
- [x] Surface `acquired_total` / `released_total` / `timeout_count` for the embedded backend (done 2026-06-10)
  - Verified `acquired_total`/`released_total`/`timeout_count` already implemented in `embedded.rs` (atomic counters). `timeout_count` is always 0 (Mutex never times out).
  - Added `embedded_pool_metrics_acquired_and_timeout` test: 3 × `pool.get().await` → assert `acquired_total ≥ 3` and `timeout_count == 0`. Green.
- [x] Add a pooled-SQLite criterion bench (checkout latency on the `oxisqlite` engine) (done 2026-06-10)
  - `bench_sqlite_pool_checkout` added to `benches/pool_benchmarks.rs` with `#[cfg(feature = "sqlite")]` / `#[cfg(not(feature = "sqlite"))]` no-op stub. Registered in `criterion_group!`. Mirrors the embedded bench pattern (`b.to_async(&rt).iter`). Compiles with and without `sqlite` feature.
- [ ] Investigate a read-shared embedded pool once GlueSQL exposes a `&self` read path (today `Glue::execute` requires `&mut self`, so `RwLock` cannot give concurrent reads)

## Known limitations
- The embedded pool serialises all access through a single `tokio::sync::Mutex` — GlueSQL's `Glue::execute` requires `&mut self` even for `SELECT`, so concurrent reads are not possible; this is by design for the in-memory backend.
- MySQL/Postgres integration tests need a live server and are `#[ignore]`d (4 tests) when none is configured.
