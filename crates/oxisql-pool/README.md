# oxisql-pool — Async connection pooling for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-pool.svg)](https://crates.io/crates/oxisql-pool)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-pool` provides async connection pools for all OxiSQL backends. All pool types implement the `oxisql_core::ConnectionPool` trait. Features are fully opt-in — the default feature set is empty.

## Installation

```toml
[dependencies]
# Choose the backends you need:
oxisql-pool = { version = "0.1.0", features = ["embedded"] }
oxisql-pool = { version = "0.1.0", features = ["postgres"] }
oxisql-pool = { version = "0.1.0", features = ["mysql"] }
oxisql-pool = { version = "0.1.0", features = ["sqlite"] }  # Pure-Rust Limbo
```

## Quick Start

```rust
#[cfg(feature = "embedded")]
{
    use oxisql_pool::embedded::EmbeddedPool;
    use oxisql_core::ConnectionPool;

    let pool = EmbeddedPool::new();
    let conn = pool.get().await?;
    conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;
}
```

```rust
#[cfg(feature = "postgres")]
{
    use deadpool_postgres::{Config, Runtime};
    use oxisql_pool::postgres::OxidbPgPool;

    let mut cfg = Config::new();
    cfg.host = Some("localhost".to_string());
    cfg.dbname = Some("mydb".to_string());
    cfg.user = Some("postgres".to_string());

    let pool = OxidbPgPool::new(cfg, Runtime::Tokio1)?;
    let conn = pool.get().await?;
    // conn implements oxisql_core::Connection
}
```

```rust
#[cfg(feature = "mysql")]
{
    use oxisql_pool::mysql::new_mysql_pool;

    let pool = new_mysql_pool("mysql://root:secret@localhost:3306/mydb", 8)?;
    let conn = pool.get().await?;
}
```

## Feature Flags

| Feature | Pool type | Backend |
|---------|-----------|---------|
| `postgres` | `postgres::OxidbPgPool` | `deadpool-postgres` over `tokio-postgres` |
| `mysql` | `mysql::MysqlPool` | Custom `deadpool` Manager over `mysql_async::Conn` |
| `embedded` | `embedded::EmbeddedPool` | `Arc<Mutex<Glue<MemoryStorage>>>` |
| `sqlite` / `sqlite-compat` | `sqlite_compat::SqliteCompatPool` | Pure-Rust Limbo engine |
| `sqlite-rusqlite` | `sqlite_rusqlite` module | C-backed rusqlite (legacy escape-hatch) |

`oxisql_pool::sqlite` is an alias for `oxisql_pool::sqlite_compat` when the `sqlite` feature is active.

## API Overview

### `OxidbPool` — unified pool enum

```rust
pub enum OxidbPool {
    Postgres(postgres::OxidbPgPool),   // feature = "postgres"
    Mysql(mysql::MysqlPool),           // feature = "mysql"
    Embedded(embedded::EmbeddedPool),  // feature = "embedded"
    Sqlite(sqlite_compat::SqliteCompatPool), // feature = "sqlite"
}
```

| Method | Description |
|--------|-------------|
| `pool.health_check()` | Ping the backing pool; returns `Result<(), PoolError>` |
| `pool.metrics()` | Return a `PoolMetrics` snapshot |

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

### `PoolConfig` and `PoolConfigBuilder`

```rust
use oxisql_pool::{PoolConfig, PoolConfigBuilder};

let config = PoolConfigBuilder::new()
    .max_size(20)
    .min_idle(2)
    .connect_timeout_ms(5_000)
    .idle_timeout_ms(300_000)
    .build();

assert_eq!(config.max_size, 20);
```

`PoolConfig` fields: `max_size: usize`, `min_idle: Option<usize>`, `connect_timeout_ms: Option<u64>`, `idle_timeout_ms: Option<u64>`. Defaults: max_size=10, connect_timeout=30s, idle_timeout=600s.

### `PoolHooks`

Lifecycle callbacks fired at connection events:

```rust
use oxisql_pool::PoolHooks;

let hooks = PoolHooks::new()
    .on_create(|| println!("connection created"))
    .on_checkout(|| println!("connection checked out"))
    .on_checkin(|| println!("connection returned"));
```

### `PoolError`

Covers all backend-specific pool errors:

| Variant | Condition |
|---------|-----------|
| `Postgres(PoolError)` | deadpool-postgres checkout failure |
| `CreatePool(CreatePoolError)` | Postgres pool construction failure |
| `Mysql(PoolError<_>)` | deadpool MySQL checkout failure |
| `MysqlUrl(UrlError)` | MySQL URL parse failure |
| `Sqlite(PoolError<_>)` | Limbo SQLite pool error |
| `Build(String)` | Generic build or configuration error |
| `NoBackend` | No pool backend feature enabled |

### `kv_store` module

`oxisql_pool::kv_store` provides `EmbeddedKvStore` and `OxidbKvStore` — SQL-backed key-value stores layered on top of the pool.

## All Pool Types

### `postgres::OxidbPgPool`

- Backed by `deadpool-postgres`
- Implements `oxisql_core::ConnectionPool`
- `OxidbPgPool::new(cfg, runtime)` — construct from a `deadpool_postgres::Config`
- Checked-out connections implement `oxisql_core::Connection` fully (including prepared statements, schema introspection, and savepoints)

### `mysql::MysqlPool` and `new_mysql_pool`

- Custom `deadpool::managed::Manager` (`MysqlManager`) over raw `mysql_async::Conn`
- `new_mysql_pool(url, pool_size)` — convenience constructor
- Implements `oxisql_core::ConnectionPool`

### `embedded::EmbeddedPool`

- Wraps `Arc<Mutex<Glue<MemoryStorage>>>`
- `EmbeddedPool::new()` — creates a new pool with a fresh in-memory GlueSQL instance
- Implements `oxisql_core::ConnectionPool`
- `pool.pool_health_check()` — verify the pool is not closed

### `sqlite_compat::SqliteCompatPool`

- Pure-Rust Limbo SQLite pool
- No C dependencies
- Implements `oxisql_core::ConnectionPool`

## Test Status

As of 2026-05-30: **48 tests passing, 4 skipped**.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
