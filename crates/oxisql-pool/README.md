# oxisql-pool — Async connection pooling for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-pool.svg)](https://crates.io/crates/oxisql-pool)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Async connection pooling for every OxiSQL backend — `deadpool-postgres`, a custom
`mysql_async` manager, an embedded pool, and a **Pure-Rust SQLite pool**. All of
them implement the `oxisql_core::ConnectionPool` trait.

**Status: Stable.**

## What it is

`oxisql-pool` gives you one pooling layer that spans four different database
backends, each behind its own opt-in feature flag (the default feature set is
**empty**). Whether you pool Postgres, MySQL, the embedded GlueSQL engine, or the
C-free SQLite engine, every pool exposes the same `ConnectionPool` surface and the
same `PoolMetrics` snapshot, so application code stays backend-agnostic.

The SQLite pool is **100% Pure Rust** — it is backed by the `oxisqlite` engine via
`oxisql-sqlite-compat`. The legacy `sqlite-rusqlite` feature and the C-FFI
`rusqlite` dependency have been **removed**; there are no C/C++ dependencies on the
SQLite path.

## Installation (0.3.3)

```toml
[dependencies]
# Choose the backends you need (all features are opt-in):
oxisql-pool = { version = "0.4.1", features = ["embedded"] }
oxisql-pool = { version = "0.4.1", features = ["postgres"] }
oxisql-pool = { version = "0.4.1", features = ["mysql"] }
oxisql-pool = { version = "0.4.1", features = ["sqlite"] }  # Pure-Rust oxisqlite engine
```

- MSRV: **1.89** · edition **2021** · `#![forbid(unsafe_code)]`

## Quick start

### Embedded pool

```rust
# #[cfg(feature = "embedded")]
# async fn demo() -> Result<(), oxisql_pool::PoolError> {
use oxisql_pool::embedded::EmbeddedPool;
use oxisql_core::ConnectionPool;

let pool = EmbeddedPool::new();
let conn = pool.get().await?;
conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;

let metrics = pool.metrics();
assert_eq!(metrics.max_size, 1);
# Ok(())
# }
```

### Pure-Rust SQLite pool

```rust
# #[cfg(feature = "sqlite")]
# async fn demo() -> Result<(), oxisql_pool::PoolError> {
use oxisql_pool::sqlite::SqlitePool;       // alias for sqlite_compat::SqliteCompatPool
use oxisql_core::ConnectionPool;

// In-memory, Pure-Rust SQLite — no libsqlite3, no C/C++.
let pool = SqlitePool::open_memory(4).await?;
let conn = pool.get().await?;
conn.execute("CREATE TABLE kv (k TEXT, v TEXT)", &[]).await?;
assert_eq!(pool.backend_name(), "sqlite");
# Ok(())
# }
```

### Postgres pool (`deadpool-postgres`)

```rust
# #[cfg(feature = "postgres")]
# fn demo() -> Result<(), oxisql_pool::PoolError> {
use deadpool_postgres::{Config, Runtime};
use oxisql_pool::postgres::OxidbPgPool;

let mut cfg = Config::new();
cfg.host = Some("localhost".to_string());
cfg.dbname = Some("mydb".to_string());
cfg.user = Some("postgres".to_string());

let pool = OxidbPgPool::new(cfg, Runtime::Tokio1)?;
// `pool.get().await?` yields a connection that implements oxisql_core::Connection
# Ok(())
# }
```

### MySQL pool (custom `deadpool` manager)

```rust
# #[cfg(feature = "mysql")]
# fn demo() -> Result<(), oxisql_pool::PoolError> {
use oxisql_pool::mysql::new_mysql_pool;

let pool = new_mysql_pool("mysql://root:secret@localhost:3306/mydb", 8)?;
// `pool.get().await?` yields a pooled mysql_async connection
# Ok(())
# }
```

### Unified `OxidbPool` enum

```rust
# #[cfg(feature = "embedded")]
# async fn demo() -> Result<(), oxisql_pool::PoolError> {
use oxisql_pool::{OxidbPool, embedded::EmbeddedPool};

let pool = OxidbPool::Embedded(EmbeddedPool::new());
pool.health_check().await?;           // backend-specific ping
let m = pool.metrics();               // PoolMetrics snapshot
println!("idle={} active={}", m.idle, m.active);
# Ok(())
# }
```

## Key API

| Item | Description |
|------|-------------|
| `OxidbPool` | Unified enum: `Postgres`, `Mysql`, `Embedded`, `Sqlite` (each variant is feature-gated) |
| `OxidbPool::health_check()` | Backend-specific ping (`SELECT 1` for PG/MySQL, liveness check for embedded/sqlite) → `Result<(), PoolError>` |
| `OxidbPool::metrics()` | Returns a `PoolMetrics` snapshot |
| `PoolMetrics` | `max_size`, `active`, `idle`, `wait_count`, `acquired_total`, `released_total`, `timeout_count` |
| `PoolConfig` / `PoolConfigBuilder` | `max_size` (default 10), `min_idle`, `connect_timeout_ms` (default 30 s), `idle_timeout_ms` (default 600 s) |
| `PoolHooks` | Lifecycle callbacks: `on_create`, `on_checkout`, `on_checkin` |
| `PoolError` | Unified pool error across all backends (see table below) |
| `postgres::OxidbPgPool` | Postgres pool over `deadpool-postgres` |
| `mysql::MysqlPool` + `new_mysql_pool` | MySQL pool over a custom `deadpool::managed::Manager` |
| `embedded::EmbeddedPool` | Embedded GlueSQL pool (`Arc<Mutex<Glue<MemoryStorage>>>`) |
| `sqlite_compat::SqliteCompatPool` | Pure-Rust SQLite pool (aliased `sqlite::SqlitePool`) |
| `kv_store::EmbeddedKvStore`, `kv_store::OxidbKvStore` | SQL-backed key-value stores layered on the pool |

Each concrete pool type also exposes `backend_name() -> &'static str`
(`"postgres"` / `"mysql"` / `"embedded"` / `"sqlite"`).

### `PoolMetrics`

| Field | Type | Description |
|-------|------|-------------|
| `max_size` | `usize` | Maximum connections the pool will create |
| `active` | `usize` | Connections currently checked out |
| `idle` | `usize` | Connections available for checkout |
| `wait_count` | `usize` | Waiters blocked waiting for a connection |
| `acquired_total` | `u64` | Total successful checkouts since pool creation |
| `released_total` | `u64` | Total connections returned since creation |
| `timeout_count` | `u64` | Total checkout timeouts since creation |

### `PoolConfig` / `PoolConfigBuilder`

```rust
use oxisql_pool::{PoolConfig, PoolConfigBuilder};

let config: PoolConfig = PoolConfigBuilder::new()
    .max_size(20)
    .min_idle(2)
    .connect_timeout_ms(5_000)
    .idle_timeout_ms(300_000)
    .build();

assert_eq!(config.max_size, 20);
```

Fields: `max_size: usize`, `min_idle: Option<usize>`, `connect_timeout_ms: Option<u64>`,
`idle_timeout_ms: Option<u64>`. Defaults: `max_size = 10`, `connect_timeout = 30 s`,
`idle_timeout = 600 s`.

### `PoolHooks`

```rust
use oxisql_pool::PoolHooks;

let hooks = PoolHooks::new()
    .on_create(|| println!("connection created"))
    .on_checkout(|| println!("connection checked out"))
    .on_checkin(|| println!("connection returned"));
```

### `PoolError`

| Variant | Condition |
|---------|-----------|
| `Postgres(..)` | `deadpool-postgres` checkout failure |
| `CreatePool(..)` | Postgres pool construction failure |
| `Mysql(..)` | `deadpool` MySQL checkout failure |
| `MysqlUrl(..)` | MySQL URL parse failure |
| `Sqlite(..)` | SQLite (`oxisqlite`) pool error |
| `Build(String)` | Generic build or configuration error |
| `NoBackend` | No pool backend feature enabled |

### `kv_store` module

`oxisql_pool::kv_store` provides `EmbeddedKvStore` (backed by `EmbeddedPool`) and
`OxidbKvStore` (wrapping `Arc<OxidbPool>` and dispatching per variant) — SQL-backed
key-value stores layered on top of the pool.

## Feature flags

All features are **opt-in**; the default set is empty.

| Feature | Pool type | Backend |
|---------|-----------|---------|
| `postgres` | `postgres::OxidbPgPool` | `deadpool-postgres` over `tokio-postgres` |
| `mysql` | `mysql::MysqlPool` | Custom `deadpool::managed::Manager` over `mysql_async::Conn` |
| `embedded` | `embedded::EmbeddedPool` | `Arc<Mutex<Glue<MemoryStorage>>>` |
| `sqlite` | `sqlite_compat::SqliteCompatPool` | **Pure-Rust `oxisqlite` engine** (canonical) |
| `sqlite-compat` | (alias of `sqlite`) | Transitional alias — same Pure-Rust `oxisqlite` engine |
| `query-builder` | — | `EmbeddedPool` ↔ `QueryBuilder` integration (implies `embedded` + `oxisql-parse`) |
| `pool` | — | Marker feature for pool integration in sibling crates |

`oxisql_pool::sqlite` is an alias for `oxisql_pool::sqlite_compat` whenever the
`sqlite` feature is active.

## Test coverage

**35 tests pass** with default features; **62 tests pass** with `--all-features`
(52 integration + 10 doc tests), **4 ignored**. The 4 ignored tests are
live-server-gated MySQL/Postgres pool tests that require a running database
server; they are skipped when no server is available.

## See also

This crate is one of a 17-crate Pure-Rust workspace. See the
[workspace README](../../README.md) for the full picture; the SQLite engine lives in
[`oxisql-sqlite-compat`](../oxisql-sqlite-compat/README.md) and migrations in
[`oxisql-migrate`](../oxisql-migrate/README.md).

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
