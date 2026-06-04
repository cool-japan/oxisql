# oxisql — The COOLJAPAN Pure-Rust SQL facade

[![Crates.io](https://img.shields.io/crates/v/oxisql.svg)](https://crates.io/crates/oxisql)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql` is the top-level façade crate. A single URI string selects the backend; the caller receives a `Box<dyn Connection>` regardless of which database is running. All backends are Pure Rust — no C/C++ libraries required.

## Installation

```toml
[dependencies]
# In-memory only
oxisql = { version = "0.1.1", features = ["embedded"] }

# PostgreSQL
oxisql = { version = "0.1.1", features = ["postgres"] }

# MySQL
oxisql = { version = "0.1.1", features = ["mysql"] }

# All backends + pooling
oxisql = { version = "0.1.1", features = ["embedded", "postgres", "mysql", "pool-embedded", "pool-postgres", "pool-mysql"] }
```

## Quick Start

```rust
use oxisql::Connection;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // Connect — URI dispatches to the right backend automatically
    let conn = oxisql::connect("memory://").await?;

    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO t VALUES ($1, $2)", &[&1i64, &"Alice"]).await?;

    let rows = conn.query("SELECT id, name FROM t", &[]).await?;
    let name: String = rows[0].try_get("name")?;
    println!("name={name}");
    Ok(())
}
```

## URI Scheme Reference

| URI prefix | Feature flag | Backend | Notes |
|------------|-------------|---------|-------|
| `memory://` | `embedded` | GlueSQL MemoryStorage | Fresh in-memory DB each time |
| `postgres://` or `postgresql://` | `postgres` | tokio-postgres | Pure Rust, no libpq |
| `mysql://` | `mysql` | mysql_async | Pure Rust, no libmysqlclient |
| `redb://path/to/file.db` | `redb` | redb B-tree storage | Pure Rust, file-backed |
| `fjall://path/to/dir` | `fjall` | fjall LSM-tree storage | Pure Rust, file-backed |
| `sqlite://path.db` or `sqlite::memory:` | `sqlite` | Limbo (Pure Rust SQLite) | No libsqlite3 |
| `datafusion://` | `datafusion` | Apache DataFusion OLAP | Use `connect_datafusion()` |

## Entry Points

### `connect(uri)` → `Box<dyn Connection>`

```rust
let conn: Box<dyn oxisql::Connection> = oxisql::connect("memory://").await?;
let conn = oxisql::connect("postgres://user:pass@host/db").await?;
let conn = oxisql::connect("mysql://user:pass@host/db").await?;
let conn = oxisql::connect("sqlite:///tmp/mydb.sqlite3").await?;
let conn = oxisql::connect("redb:///tmp/mydb.redb").await?;
let conn = oxisql::connect("fjall:///tmp/mydb.fjall").await?;
```

### `connect_pooled(uri, size)` → `Box<dyn ConnectionPool>`

Returns a type-erased pool. Pool size controls the maximum number of connections.

```rust
let pool = oxisql::connect_pooled("memory://", 4).await?;
let conn = pool.get().await?;
```

Requires the matching `pool-*` feature: `pool-embedded`, `pool-postgres`, or `pool-mysql`.

### `connect_pool(uri, size)` → `OxidbPool`

Returns a typed `oxisql_pool::OxidbPool` enum for backend-specific pool access.

```rust
let pool = oxisql::connect_pool("memory://", 4).await?;
pool.health_check().await?;
let metrics = pool.metrics();
println!("active={}, idle={}", metrics.active, metrics.idle);
```

### `connect_datafusion(uri)` → `OxiSqlContext`

DataFusion is an OLAP engine, not a single-connection backend. Use this function instead of `connect()`.

```rust
let ctx = oxisql::connect_datafusion("datafusion://").await?;
// ctx is an OxiSqlContext for registering tables and running OLAP queries
```

## Feature Flags

| Feature | URI scheme | Description |
|---------|-----------|-------------|
| `embedded` | `memory://` | GlueSQL in-memory engine |
| `postgres` | `postgres://` / `postgresql://` | tokio-postgres, Pure Rust |
| `mysql` | `mysql://` | mysql_async, Pure Rust |
| `redb` | `redb://` | redb-backed persistent embedded SQL (enables `embedded`) |
| `fjall` | `fjall://` | fjall-backed persistent embedded SQL (enables `embedded`) |
| `sled` | — | sled-backed persistent embedded SQL (enables `embedded`) |
| `sqlite` | `sqlite://` / `sqlite::memory:` | Pure-Rust SQLite via Limbo |
| `datafusion` | `datafusion://` | Apache DataFusion OLAP layer |
| `pool-embedded` | `memory://` | `EmbeddedPool` |
| `pool-postgres` | `postgres://` | `OxidbPgPool` via deadpool-postgres |
| `pool-mysql` | `mysql://` | `MysqlPool` via custom deadpool Manager |
| `pool-sqlite-compat` | `sqlite://` | `SqliteCompatPool` |
| `migrate` | — | SQL migration runner via `oxisql_migrate` |

## Re-exported types at crate root

All commonly used types from `oxisql-core` are re-exported at the crate root:

- `Connection`, `Transaction`, `ConnectionPool`
- `Value`, `Row`, `RowSet`, `FromValue`, `ToSqlValue`
- `OxiSqlError`
- `TableInfo`, `ColumnInfo`, `IndexInfo`, `ForeignKeyInfo`, `TableType`
- `LoggingConnection`, `MetricsConnection`, `ConnectionMetrics`, `MetricsSnapshot`
- `RetryConnection`, `RetryPolicy`, `RetryPredicate`

## Prelude

```rust
use oxisql::prelude::*;
```

Imports `Connection`, `Transaction`, `ConnectionPool`, `Value`, `Row`, `RowSet`, `FromValue`, `ToSqlValue`, `OxiSqlError`, schema types, and middleware types.

## Backend-specific modules

```rust
// PostgreSQL-specific types (requires "postgres" feature)
use oxisql::postgres::{PgConnection, PgError, PgTransaction, TlsMode};

// MySQL-specific types (requires "mysql" feature)
use oxisql::mysql::*;
```

## Named Parameters

`Connection` provides default methods `execute_named` and `query_named` for named-placeholder SQL (`:param_name` syntax). Parameters are passed as a `&[(&str, &dyn ToSqlValue)]` slice and are resolved by name before execution, translating them to positional `$N` parameters internally.

```rust
conn.execute_named(
    "INSERT INTO users (id, name) VALUES (:id, :name)",
    &[("id", &42i64), ("name", &"Alice")],
).await?;

let rows = conn.query_named(
    "SELECT * FROM users WHERE id = :id",
    &[("id", &42i64)],
).await?;
```

These methods are default implementations on `oxisql_core::Connection` and are available on every backend without extra imports.

## `MultiConnection`

`MultiConnection` fans out queries to multiple backends simultaneously:

```rust
use oxisql::MultiConnection;

let multi = MultiConnection::new(vec![conn_a, conn_b]);
// execute/query runs against all connections in parallel
```

## `ConnectOptions`

Fine-grained options for establishing a connection:

```rust
use oxisql::ConnectOptions;

let opts = ConnectOptions::new()
    .timeout_ms(5_000)
    .pool_size(10)
    .require_tls(true);
```

## `BackendInfo`

Inspect which backend would handle a given URI without opening a connection:

```rust
let info = oxisql::backend_info_for_uri("memory://").unwrap();
assert_eq!(info.name, "embedded");
// info.features == ["in-memory", "sql", "gluesql"]

let info = oxisql::backend_info_for_uri("postgres://").unwrap();
assert_eq!(info.name, "postgres");
// info.version == None (known only after handshake)
```

## Direct connection type re-exports

```rust
// requires "embedded" feature
use oxisql::EmbeddedConnection;

// requires "redb" feature
use oxisql::RedbEmbeddedConnection;

// requires "fjall" feature
use oxisql::FjallEmbeddedConnection;

// requires "sqlite" feature
use oxisql::SqliteConnection;
```

## Version

```rust
let v: &str = oxisql::version(); // returns env!("CARGO_PKG_VERSION")
```

## Test Status

As of 2026-05-30: **79 tests passing, 2 skipped** (live-Postgres and live-MySQL portability tests are `#[ignore]`d).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
