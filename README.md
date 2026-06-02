# OxiSQL — Pure-Rust unified SQL layer

[![crates.io](https://img.shields.io/crates/v/oxisql.svg)](https://crates.io/crates/oxisql)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.89](https://img.shields.io/badge/MSRV-1.89-orange.svg)](https://blog.rust-lang.org/2025/06/05/Rust-1.89.0.html)

OxiSQL is the COOLJAPAN-blessed Pure-Rust database layer: a unified SQL surface
that spans embedded engines, OLTP wire-protocol clients (Postgres, MySQL), and a
SQLite-compatible embedded path — without `libpq`, `mysqlclient`, `libsqlite3`,
or any C/C++ DB driver. It exists because every COOLJAPAN service today either
reaches for `rusqlite` (C SQLite), `tokio-postgres` against `native-tls`/`ring`,
or `sqlx` with a `*-sys` TLS provider — each of which drags `libssl-dev`,
`libpq-dev`, or `libsqlite3-dev` into the ecosystem's CI critical path. OxiSQL
collapses these into one facade, defaults to Pure Rust drivers (`tokio-postgres`,
`mysql_async`, `gluesql`, `limbo`), routes all TLS through OxiTLS with the
rustcrypto provider (no `ring`, no `openssl-sys`), and keeps `libpq` available
only behind an opt-in `system` feature for legacy parity.

**Version 0.1.0 — released 2026-06-01.**
10 crates, 890 tests passing (64 skipped), zero clippy warnings, zero production stubs.
~33,902 lines of Rust across 145 source files.

---

## Crate Status

| Crate | Status | Tests | Description |
|-------|--------|-------|-------------|
| `oxisql` | Stable | 23 | Unified facade: `connect` / `connect_pooled` / `connect_pool` |
| `oxisql-core` | Stable | 104 | `Connection` / `Transaction` / `ConnectionPool` / `Value` traits; named-parameter default methods |
| `oxisql-parse` | Stable | 118 | SQL parsing, query builder, planner, optimizer |
| `oxisql-embedded` | Stable | 244 | GlueSQL in-memory + persistent (fjall / redb / sled); full schema introspection |
| `oxisql-postgres` | Stable | live-gated | Pure Rust tokio-postgres, no libpq |
| `oxisql-mysql` | Stable | live-gated | Pure Rust mysql_async, no libmysqlclient |
| `oxisql-sqlite-compat` | Alpha | 54 (3 skipped) | Limbo pure-Rust SQLite, no libsqlite3; LRU stmt cache |
| `oxisql-pool` | Stable | 0 | deadpool-based pooling, all backends (integration tests require live servers) |
| `oxisql-migrate` | Stable | 6 | File-based migrations, 14-digit timestamps |
| `oxisql-datafusion` | Alpha | 55 | DataFusion TableProvider bridge |

---

## Installation

Add to your workspace's root `Cargo.toml`:

```toml
# Workspace root Cargo.toml
[workspace.dependencies]
oxisql = { version = "0.1.0", features = ["embedded"] }
```

Or add to a single crate:

```toml
[dependencies]
oxisql = { version = "0.1.0", features = ["embedded", "postgres", "pool-embedded", "migrate"] }
```

---

## Quick Start

### In-memory embedded (GlueSQL)

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = oxisql::connect("memory://").await?;
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute(
        "INSERT INTO users VALUES (1, 'Alice')",
        &[],
    ).await?;
    let rows = conn.query("SELECT id, name FROM users", &[]).await?;
    for row in &rows.rows {
        println!("{:?}", row);
    }
    Ok(())
}
```

### Named parameters

`execute_named` and `query_named` are default methods on the `Connection` trait
and are available to all backends with no per-backend implementation. Use
`:name`, `$name`, or `@name` placeholder syntax.

```rust,no_run
use oxisql::prelude::*;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = oxisql::connect("memory://").await?;
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[]).await?;

    let rows = conn.query_named(
        "SELECT id, name FROM users WHERE id = :id",
        &[("id", &1i64 as &dyn oxisql::ToValue)],
    ).await?;
    for row in &rows.rows {
        println!("{:?}", row);
    }
    Ok(())
}
```

### Pooled connections

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // Returns Box<dyn ConnectionPool> — works with any backend URI
    let pool = oxisql::connect_pooled("memory://", 4).await?;
    let conn = pool.get().await?;
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;
    Ok(())
}
```

### Running migrations

```rust,no_run
use oxisql::migrate::{MigrationRunner, scan_migrations};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = oxisql::connect("memory://").await?;
    let migrations = scan_migrations("migrations/")?;
    let mut runner = MigrationRunner::new(migrations);
    runner.run_with_pool(conn.as_ref()).await?;
    Ok(())
}
```

### PostgreSQL with TLS (OxiTLS / rustcrypto)

```rust,no_run
use oxisql::postgres::{PgConnection, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Plain-text via the facade:
    let conn = oxisql::connect("postgres://user:pass@localhost/mydb").await?;

    // With rustls/OxiTLS (no ring, no openssl-sys):
    let tls_cfg = std::sync::Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth(),
    );
    let conn_tls = oxisql::connect_with_tls(
        "postgres://user:pass@localhost/mydb",
        Some(tls_cfg),
    ).await?;
    Ok(())
}
```

---

## Backends

### Embedded — GlueSQL (`memory://`)

| Property | Value |
|----------|-------|
| URI | `memory://` |
| Feature flag | `embedded` |
| Storage | In-memory (reset on drop) |
| Pure Rust | Yes |
| Persistent variants | `redb://path`, `fjall://path`, `sled://path` |

The embedded backend wraps [GlueSQL](https://github.com/gluesql/gluesql) for
in-memory SQL. Three persistent variants are available:
- `redb://path/to/file.db` — redb B-tree engine (feature: `redb`)
- `fjall://path/to/dir` — fjall LSM-tree engine (feature: `fjall`)
- `sled://path/to/dir` — sled key-value engine (feature: `sled`)

### PostgreSQL (`postgres://`)

| Property | Value |
|----------|-------|
| URI | `postgres://user:pass@host/db` or `postgresql://...` |
| Feature flag | `postgres` |
| Driver | tokio-postgres (Pure Rust) |
| TLS | OxiTLS + rustcrypto (no ring, no libssl) |
| libpq | Never required; feature-gated `system` flag planned for legacy parity |

Supports: prepared statements, transactions, COPY bulk ingestion, LISTEN/NOTIFY,
pipeline batching, extended type mapping (DATE, TIMESTAMP, UUID, JSONB, NUMERIC, ARRAY).

### MySQL (`mysql://`)

| Property | Value |
|----------|-------|
| URI | `mysql://user:pass@host/db` |
| Feature flag | `mysql` |
| Driver | mysql_async (Pure Rust) |
| TLS | OxiTLS + rustls-tls (no libssl) |
| libmysqlclient | Never required |

Supports: prepared statements, transactions, multi-result-sets for stored
procedures, LOAD DATA bulk ingestion, binary protocol, extended type mapping
(DECIMAL, DATETIME(6), JSON, ENUM).

### SQLite-compat via Limbo (`sqlite://`)

| Property | Value |
|----------|-------|
| URI | `sqlite://path/to/file.db` or `sqlite::memory:` |
| Feature flag | `sqlite` |
| Driver | [Limbo](https://github.com/tursodatabase/limbo) 0.0.22 (Pure Rust) |
| Status | Alpha |
| libsqlite3 | Never required |

26 tests pass. 2 tests are `#[ignore]`d pending ROLLBACK support in Limbo
upstream. See the [Known Limitations](#known-limitations) section.

### DataFusion OLAP (`datafusion://`)

| Property | Value |
|----------|-------|
| URI | `datafusion://` (not a `Connection` — use `connect_datafusion`) |
| Feature flag | `datafusion` |
| Engine | Apache DataFusion 53.x |
| Status | Alpha |

Use `oxisql::connect_datafusion("datafusion://")` to get an `OxiSqlContext`.
Tables from any backend can be registered via `datafusion::register_table` for
cross-backend OLAP queries.

---

## Feature Flags

| Feature | URI scheme | Backend | Notes |
|---------|------------|---------|-------|
| `embedded` | `memory://` | GlueSQL in-memory | Base embedded feature |
| `postgres` | `postgres://` / `postgresql://` | tokio-postgres | Pure Rust, no libpq |
| `mysql` | `mysql://` | mysql_async | Pure Rust, no libmysqlclient |
| `sqlite` | `sqlite://` | Limbo | Pure Rust, no libsqlite3; Alpha |
| `redb` | `redb://` | redb B-tree | Persistent embedded; implies `embedded` |
| `fjall` | `fjall://` | fjall LSM-tree | Persistent embedded; implies `embedded` |
| `sled` | `sled://` | sled key-value | Persistent embedded; implies `embedded` |
| `datafusion` | `datafusion://` | DataFusion OLAP | Use `connect_datafusion`; Alpha |
| `pool-postgres` | — | deadpool + tokio-postgres | Requires `postgres` behaviour |
| `pool-mysql` | — | deadpool + mysql_async | Requires `mysql` behaviour |
| `pool-embedded` | — | EmbeddedPool | In-memory pool |
| `pool-sqlite-compat` | — | SqliteCompatPool | Alpha; requires `sqlite` |
| `migrate` | — | MigrationRunner | File-based SQL migrations |

### Common feature combinations

```toml
# In-memory only
oxisql = { version = "0.1.0", features = ["embedded"] }

# PostgreSQL + pooling
oxisql = { version = "0.1.0", features = ["postgres", "pool-postgres"] }

# MySQL + migrations
oxisql = { version = "0.1.0", features = ["mysql", "pool-mysql", "migrate"] }

# All OLTP backends + pooling + migrations
oxisql = { version = "0.1.0", features = [
    "embedded", "postgres", "mysql",
    "pool-embedded", "pool-postgres", "pool-mysql",
    "migrate",
] }

# Full stack including DataFusion OLAP
oxisql = { version = "0.1.0", features = [
    "embedded", "postgres", "mysql", "datafusion",
    "pool-embedded", "pool-postgres", "pool-mysql",
    "migrate",
] }
```

---

## Architecture

```
oxisql (facade crate)
  |
  +-- oxisql-core          traits: Connection, Transaction, Row, Value, OxiSqlError
  |                        PreparedStatement, ConnectionPool, SchemaInspector,
  |                        LoggingConnection, RetryConnection, MetricsConnection
  |
  +-- oxisql-parse         SQL parsing (sqlparser), fluent QueryBuilder,
  |                        logical planner (Scan/Filter/Project/Join/Aggregate),
  |                        optimizer (predicate pushdown, join reordering)
  |
  +-- oxisql-embedded      GlueSQL in-memory + fjall/redb/sled persistent backends
  |                        export_as_sql() / import_from_sql()
  |
  +-- oxisql-postgres      tokio-postgres wire client, OxiTLS/rustcrypto TLS,
  |                        COPY, LISTEN/NOTIFY, pipeline batching
  |
  +-- oxisql-mysql         mysql_async wire client, rustls TLS,
  |                        bulk LOAD DATA, multi-result-sets, binary protocol
  |
  +-- oxisql-sqlite-compat Limbo 0.0.22 pure-Rust SQLite-compat (Alpha)
  |
  +-- oxisql-pool          deadpool-based pools for all backends
  |                        OxidbPool enum, PoolMetrics, PoolConfig
  |
  +-- oxisql-migrate       File-based SQL migrations, 14-digit timestamps
  |                        MigrationRunner, run_with_pool(), status(), pending()
  |
  +-- oxisql-datafusion    DataFusion TableProvider bridge,
                           OxiSqlContext, filter/projection/limit pushdown
```

### Value type system

The `Value` enum (in `oxisql-core`) has 13 variants covering the full type
surface of all supported backends:

| Variant | SQL types |
|---------|-----------|
| `Value::Null` | NULL |
| `Value::Bool` | BOOLEAN |
| `Value::Integer` | INTEGER, BIGINT, SMALLINT |
| `Value::Float` | REAL, DOUBLE PRECISION, FLOAT |
| `Value::Text` | TEXT, VARCHAR, CHAR |
| `Value::Blob` | BYTEA, BLOB, VARBINARY |
| `Value::Decimal` | NUMERIC, DECIMAL |
| `Value::Timestamp` | TIMESTAMP, TIMESTAMPTZ, DATETIME |
| `Value::Date` | DATE |
| `Value::Time` | TIME |
| `Value::Uuid` | UUID |
| `Value::Json` | JSON, JSONB |
| `Value::Array` | ARRAY types (PostgreSQL) |

Full round-trip mapping is implemented for all RDBMS backends.

### Named parameters

`Connection::execute_named` and `Connection::query_named` are default methods on
the `Connection` trait (defined in `oxisql-core::params`). Every backend inherits
them automatically — no per-backend implementation is required. Placeholder
syntax: `:name`, `$name`, or `@name`. The default methods rewrite named
placeholders to positional `$N` form before dispatch. On binding failure a new
`OxiSqlError::Params` variant is returned. Import via `use oxisql::prelude::*`
or `use oxisql_core::Connection`.

### Query middleware

OxiSQL provides composable middleware over any `Box<dyn Connection>`:

- `LoggingConnection` — logs every SQL operation with timing via the `log` crate.
- `RetryConnection` — retries transient failures with configurable `RetryPolicy`.
- `MetricsConnection` — collects per-operation counters and latencies.

### Inter-Oxi dependencies

- **Depends on:** OxiTLS (transport / TLS), `oxicode` (row serde),
  OxiCrypto (encryption-at-rest), OxiStore (lower storage layer).
- **Depended on by:** `oxirouter`, `oxirs`, `oxify`, `oxigdal-db-connectors`,
  `oximedia`, `oxigaf`, `oxirag`.

---

## Known Limitations

### oxisql-sqlite-compat (Limbo 0.0.22)

- **ROLLBACK not yet supported by Limbo 0.0.22.** Two tests are marked
  `#[ignore]` pending an upstream fix. `SqliteTransaction::rollback()` returns
  a clear `OxiSqlError::Other("ROLLBACK is not supported by the limbo 0.0.22
  engine…")` rather than a cryptic parse error. File
  [tursodatabase/limbo](https://github.com/tursodatabase/limbo) for tracking.
- **Savepoints** are not yet supported — blocked on Limbo 0.1+.
- **Named parameters at the driver level** (`:name` style passed directly to
  Limbo) are blocked on Limbo 0.1+ API stabilisation. The `Connection` trait
  default methods `execute_named` / `query_named` in `oxisql-core` provide
  named-parameter support at the facade layer for all backends today.
- **Foreign key metadata** is currently retrieved via DDL parsing
  (`sqlite_master`) because Limbo 0.0.22 does not support `PRAGMA foreign_key_list`.

### Live-server tests (postgres / mysql)

`oxisql-postgres` and `oxisql-mysql` test suites contain live-server integration
tests that are CI-gated behind `#[ignore]`. They require an accessible
PostgreSQL or MySQL server. All non-live unit and compile-time tests pass without
external services.

---

## Replaces (FFI eliminated)

| C library | Replaced by |
|-----------|-------------|
| `libpq` | `tokio-postgres` (Pure Rust) |
| `libmysqlclient` | `mysql_async` (Pure Rust) |
| `libsqlite3` (via `rusqlite-sys`) | `limbo` (Pure Rust) |
| `libssl` / `native-tls` / `ring` | OxiTLS + rustcrypto |

---

## License & Authors

Licensed under the **Apache License, Version 2.0**.
See [LICENSE](LICENSE) for the full text.

Copyright © 2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
