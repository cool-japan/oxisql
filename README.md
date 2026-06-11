# OxiSQL — Pure-Rust unified SQL layer

[![crates.io](https://img.shields.io/crates/v/oxisql.svg)](https://crates.io/crates/oxisql)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.89](https://img.shields.io/badge/MSRV-1.89-orange.svg)](https://blog.rust-lang.org/2025/08/07/Rust-1.89.0.html)
[![Pure Rust: C-free](https://img.shields.io/badge/Pure%20Rust-C--free-brightgreen.svg)](#the-c-free-oxisqlite-engine)

OxiSQL is the COOLJAPAN-blessed Pure-Rust database layer: a unified SQL surface
that spans embedded engines, OLTP wire-protocol clients (PostgreSQL, MySQL), and
a SQLite-compatible embedded path — without `libpq`, `libmysqlclient`,
`libsqlite3`, or any C/C++ DB driver. It exists because every COOLJAPAN service
today either reaches for `rusqlite` (C SQLite), `tokio-postgres` against
`native-tls`/`ring`, or `sqlx` with a `*-sys` TLS provider — each of which drags
`libssl-dev`, `libpq-dev`, or `libsqlite3-dev` into the ecosystem's CI critical
path. OxiSQL collapses these into one facade, defaults to Pure Rust drivers
(`tokio-postgres`, `mysql_async`, GlueSQL, and the C-free **oxisqlite** engine),
and routes all TLS through OxiTLS with the rustcrypto provider (no `ring`, no
`openssl-sys`).

As of 0.1.2 the SQLite path is **genuinely C-free**. It is served by an in-tree
fork of the limbo engine (`oxisqlite-*`) from which every C touchpoint has been
removed — no `libsqlite3`, no `mimalloc`, no `lemon` parser generator. The
default build of the entire workspace compiles cleanly with the C compiler
disabled:

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
cargo build --workspace                      # → 0 warnings
```

**Version 0.1.2 — released 2026-06-10.**
17 workspace crates · 1,851 tests passing · 0 failing · 0 clippy warnings.
~118,234 lines of Rust across 323 source files.

---

## What's new in 0.1.2

- **C-free oxisqlite engine fork.** The SQLite-compatible path no longer pulls in
  any C code. A vendored, de-C'd fork of limbo 0.0.22 (`oxisqlite`,
  `oxisqlite-core`, `oxisqlite-sqlite3-parser`, and four support crates) replaces
  the previous build, making the SQLite backend Pure Rust for the first time.
- **Full-transaction ROLLBACK.** `BEGIN; INSERT; ROLLBACK` now correctly discards
  changes, `COMMIT` persists them, and WAL integrity is preserved. The rollback
  machinery was ported from `turso_core` 0.7.0-pre.5 (MIT). The old "ROLLBACK not
  supported" limitation is gone.
- **Apache-2.0 compliance + security-patched TLS.** A GPL-licensed Julian-day
  helper was replaced by an inline pure-Rust implementation, license auditing
  (`cargo deny`) passes, a root [`NOTICE`](NOTICE) records the full fork lineage,
  and the TLS stack is patched against RUSTSEC-2026-0104 (CRL-parsing panic).

---

## Highlights

- **One facade, many backends.** A single `oxisql::connect(uri)` call dispatches
  to in-memory/persistent embedded engines, PostgreSQL, MySQL, the C-free SQLite
  engine, or Apache DataFusion — by URI scheme.
- **Pure Rust by default.** The default feature set is 100% C-free. C/Fortran
  dependencies are not merely avoided — they are eliminated and proven absent
  (`CC=/usr/bin/false`).
- **Async, trait-based core.** `Connection`, `Transaction`, `ConnectionPool`,
  `Row`, and a 13-variant `Value` type unify every backend behind one ergonomic
  async API.
- **Named parameters everywhere.** `:name`, `$name`, and `@name` work on *all*
  backends as default `Connection` methods — no per-backend implementation
  required.
- **Composable middleware.** `LoggingConnection`, `MetricsConnection`, and
  `RetryConnection` wrap any `Box<dyn Connection>`.
- **TLS without C.** OxiTLS + rustls-rustcrypto for PostgreSQL and MySQL — no
  `ring`, no `openssl-sys`, no `native-tls`.
- **Pooling & migrations.** deadpool-backed pools for every backend and a
  file-based, timestamped migration runner.
- **Optional REPL.** A `oxisql-repl` binary (`repl` feature) with `.help`,
  `.tables`, `.schema <t>`, and `.quit`.

---

## Crate Status

OxiSQL ships as **17 workspace crates**: 10 facade/driver crates and a 7-crate,
C-free `oxisqlite-*` engine.

### Facade & drivers (10)

| Crate | Status | Tests | Description |
|-------|--------|-------|-------------|
| `oxisql` | Stable | 80 (+2 live-gated) | Unified facade: `connect` / `connect_pooled` / `connect_pool` / `connect_datafusion` |
| `oxisql-core` | Stable | 114 | `Connection` / `Transaction` / `ConnectionPool` / `Value` traits; named-parameter default methods; middleware |
| `oxisql-parse` | Stable | 129 | SQL parsing, fluent query builder, logical planner, optimizer |
| `oxisql-embedded` | Stable | 278 | GlueSQL in-memory + persistent (redb / fjall / sled); schema introspection |
| `oxisql-postgres` | Stable | 61 (+6 live-gated) | Pure Rust `tokio-postgres`, no libpq; OxiTLS/rustcrypto |
| `oxisql-mysql` | Stable | 95 | Pure Rust `mysql_async`, no libmysqlclient |
| `oxisql-datafusion` | Alpha | 57 (+4) | Apache DataFusion `TableProvider` bridge |
| `oxisql-pool` | Stable | 57 (+4 live-gated) | deadpool-based pooling for all backends |
| `oxisql-migrate` | Stable | 37 | File-based SQL migrations, 14-digit timestamps |
| `oxisql-sqlite-compat` | Alpha | 61 (+1 FK-DDL) | C-free SQLite engine on top of `oxisqlite-*`; **ROLLBACK now supported**; LRU stmt cache |

### oxisqlite engine (7)

These crates form the in-tree, C-free fork of limbo. They are **internal** —
consumed by `oxisql-sqlite-compat` and not part of OxiSQL's public surface.

| Crate | Status | Tests | Description |
|-------|--------|-------|-------------|
| `oxisqlite` | Internal | 5 | Top-level engine facade / connection entry point |
| `oxisqlite-core` | Internal | 538 (+12 fuzz/stress) | Storage engine: B-tree, pager, WAL, VDBE, transactions, ROLLBACK |
| `oxisqlite-ext` | Internal | — | Built-in extensions / virtual-table glue |
| `oxisqlite-macros` | Internal | — | Procedural macros for the engine |
| `oxisqlite-sqlite3-parser` | Internal | 208 (+6) | SQL parser (pre-generated, no `lemon` C generator) |
| `oxisqlite-time` | Internal | — | Pure-Rust date/time helpers (chrono-based) |
| `oxisqlite-uuid` | Internal | — | Pure-Rust UUID support |

> A vendored crate, `rustls-rustcrypto-patched`, is applied via
> `[patch.crates-io]` to fix RUSTSEC-2026-0104. It is **not** a workspace member
> — see [The C-free oxisqlite engine](#the-c-free-oxisqlite-engine) and
> [Pure Rust — FFI eliminated](#pure-rust--ffi-eliminated).

---

## Installation

Add to your workspace's root `Cargo.toml`:

```toml
# Workspace root Cargo.toml
[workspace.dependencies]
oxisql = { version = "0.1.2", features = ["embedded"] }
```

Or add to a single crate:

```toml
[dependencies]
oxisql = { version = "0.1.2", features = ["embedded", "postgres", "pool-embedded", "migrate"] }
```

---

## Quick Start

### In-memory embedded (GlueSQL)

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = oxisql::connect("memory://").await?;
    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO users VALUES (1, 'Alice')", &[]).await?;
    let rows = conn.query("SELECT id, name FROM users", &[]).await?;
    for row in &rows {
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
        &[("id", &1i64 as &dyn ToSqlValue)],
    ).await?;
    for row in &rows {
        println!("{:?}", row);
    }
    Ok(())
}
```

### SQLite with full ROLLBACK (now Pure Rust)

As of 0.1.2 the SQLite path runs on the C-free `oxisqlite` engine and supports
real transactional `ROLLBACK`. The example below opens an in-memory database via
the `sqlite` feature and verifies that a rolled-back `INSERT` leaves no rows.

```rust,no_run
use oxisql::SqliteConnection;
use oxisql::prelude::*;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    let conn = SqliteConnection::open_memory().await?;
    conn.execute("CREATE TABLE t (id INTEGER, name TEXT)", &[]).await?;

    // BEGIN ... ROLLBACK discards the INSERT.
    conn.execute("BEGIN", &[]).await?;
    conn.execute("INSERT INTO t VALUES (1, 'Alice')", &[]).await?;
    conn.execute("ROLLBACK", &[]).await?;

    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await?;
    println!("rows after ROLLBACK: {:?}", rows); // → one row holding COUNT(*) = 0
    Ok(())
}
```

You can reach the same backend through the facade with
`oxisql::connect("sqlite::memory:")` or `oxisql::connect("sqlite://path/to/file.db")`.

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

| URI | Engine | Feature |
|-----|--------|---------|
| `redb://path/to/file.db` | redb B-tree | `redb` |
| `fjall://path/to/dir` | fjall LSM-tree | `fjall` |
| `sled://path/to/dir` | sled key-value | `sled` |

`export_as_sql()` / `import_from_sql()` round-trip a database as SQL text.

### PostgreSQL (`postgres://`)

| Property | Value |
|----------|-------|
| URI | `postgres://user:pass@host/db` or `postgresql://...` |
| Feature flag | `postgres` |
| Driver | `tokio-postgres` (Pure Rust) |
| TLS | OxiTLS + rustcrypto (no ring, no libssl) |
| libpq | Never required |

Supports prepared statements, transactions, COPY bulk ingestion, LISTEN/NOTIFY,
pipeline batching, and extended type mapping (DATE, TIMESTAMP, UUID, JSONB,
NUMERIC, ARRAY).

> A C-linked `libpq` path does **not** exist today. If legacy parity is ever
> required it could be added behind an opt-in feature in a future release, but no
> such flag is shipped.

### MySQL (`mysql://`)

| Property | Value |
|----------|-------|
| URI | `mysql://user:pass@host/db` |
| Feature flag | `mysql` |
| Driver | `mysql_async` (Pure Rust) |
| TLS | OxiTLS + rustls-tls (no libssl) |
| libmysqlclient | Never required |

Supports prepared statements, transactions, multi-result-sets for stored
procedures, LOAD DATA bulk ingestion, the binary protocol, and extended type
mapping (DECIMAL, DATETIME(6), JSON, ENUM).

### SQLite-compat via oxisqlite (`sqlite://`)

| Property | Value |
|----------|-------|
| URI | `sqlite://path/to/file.db` or `sqlite::memory:` |
| Feature flag | `sqlite` |
| Engine | [oxisqlite](#the-c-free-oxisqlite-engine) — C-free fork of [limbo](https://github.com/tursodatabase/limbo) 0.0.22 |
| Status | Alpha |
| ROLLBACK | **Supported** (BEGIN / COMMIT / ROLLBACK, WAL-safe) |
| Pure Rust | **Yes** — no `libsqlite3`, no `mimalloc`, no `lemon` |

The SQLite-compatible backend sits on top of the in-tree `oxisqlite-*` engine.
Full transactional rollback works (5 dedicated tests in
`crates/oxisql-sqlite-compat/tests/rollback.rs`). `SAVEPOINT` is not yet wired
up and returns a clear error rather than panicking — see
[Known Limitations](#known-limitations).

### DataFusion OLAP (`datafusion://`)

| Property | Value |
|----------|-------|
| URI | `datafusion://` (not a `Connection` — use `connect_datafusion`) |
| Feature flag | `datafusion` |
| Engine | Apache DataFusion |
| Status | Alpha |

Use `oxisql::connect_datafusion("datafusion://")` to obtain an `OxiSqlContext`.
Tables from any backend can be registered via `datafusion::register_table` for
cross-backend OLAP queries (filter / projection / limit pushdown).

---

## Feature Flags

| Feature | URI scheme | Backend | Notes |
|---------|------------|---------|-------|
| `embedded` | `memory://` | GlueSQL in-memory | Base embedded feature |
| `postgres` | `postgres://` / `postgresql://` | tokio-postgres | Pure Rust, no libpq |
| `mysql` | `mysql://` | mysql_async | Pure Rust, no libmysqlclient |
| `sqlite` | `sqlite://` / `sqlite::memory:` | oxisqlite (C-free) | Pure Rust, no libsqlite3; Alpha |
| `redb` | `redb://` | redb B-tree | Persistent embedded; implies `embedded` |
| `fjall` | `fjall://` | fjall LSM-tree | Persistent embedded; implies `embedded` |
| `sled` | `sled://` | sled key-value | Persistent embedded; implies `embedded` |
| `datafusion` | `datafusion://` | DataFusion OLAP | Use `connect_datafusion`; Alpha |
| `pool-postgres` | — | deadpool + tokio-postgres | Pulls in `postgres` behaviour |
| `pool-mysql` | — | deadpool + mysql_async | Pulls in `mysql` behaviour |
| `pool-embedded` | — | EmbeddedPool | In-memory pool |
| `pool-sqlite-compat` | — | SqliteCompatPool | Alpha; pulls in `sqlite` |
| `migrate` | — | MigrationRunner | File-based SQL migrations |
| `repl` | — | `oxisql-repl` binary | `.help` / `.tables` / `.schema <t>` / `.quit` |

### Common feature combinations

```toml
# In-memory only
oxisql = { version = "0.1.2", features = ["embedded"] }

# PostgreSQL + pooling
oxisql = { version = "0.1.2", features = ["postgres", "pool-postgres"] }

# MySQL + migrations
oxisql = { version = "0.1.2", features = ["mysql", "pool-mysql", "migrate"] }

# C-free SQLite + pooling
oxisql = { version = "0.1.2", features = ["sqlite", "pool-sqlite-compat"] }

# All OLTP backends + pooling + migrations
oxisql = { version = "0.1.2", features = [
    "embedded", "postgres", "mysql", "sqlite",
    "pool-embedded", "pool-postgres", "pool-mysql", "pool-sqlite-compat",
    "migrate",
] }

# Full stack including DataFusion OLAP and the REPL
oxisql = { version = "0.1.2", features = [
    "embedded", "postgres", "mysql", "sqlite", "datafusion",
    "pool-embedded", "pool-postgres", "pool-mysql",
    "migrate", "repl",
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
  |                        named-parameter default methods (:name / $name / @name)
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
  +-- oxisql-sqlite-compat C-free SQLite-compat (Alpha) — ROLLBACK supported
  |       |                LRU prepared-statement cache
  |       |
  |       +-- oxisqlite                 engine facade / connection entry point
  |             |
  |             +-- oxisqlite-core              B-tree, pager, WAL, VDBE, ROLLBACK
  |             +-- oxisqlite-sqlite3-parser    SQL parser (no lemon C generator)
  |             +-- oxisqlite-ext               extensions / vtab glue
  |             +-- oxisqlite-macros            engine procedural macros
  |             +-- oxisqlite-time              pure-Rust date/time helpers
  |             +-- oxisqlite-uuid              pure-Rust UUID support
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
| `Value::I64` | INTEGER, BIGINT, SMALLINT |
| `Value::F64` | REAL, DOUBLE PRECISION, FLOAT |
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
the `Connection` trait (defined in `oxisql-core`). Every backend inherits them
automatically — no per-backend implementation is required. Placeholder syntax:
`:name`, `$name`, or `@name`. The default methods rewrite named placeholders to
positional form before dispatch; on a missing binding they return
`OxiSqlError::Params`. Parameter values implement the `ToSqlValue` trait. Import
via `use oxisql::prelude::*`.

### Query middleware

OxiSQL provides composable middleware over any `Box<dyn Connection>`:

- `LoggingConnection` — logs every SQL operation with timing via the `log` crate.
- `RetryConnection` — retries transient failures with a configurable `RetryPolicy`.
- `MetricsConnection` — collects per-operation counters and latencies.

A `MultiConnection` wrapper additionally lets a single handle fan out across
several backend connections.

### Inter-Oxi dependencies

- **Depends on:** OxiTLS (transport / TLS), `oxicode` (row serde),
  OxiCrypto (encryption-at-rest), OxiStore (lower storage layer).
- **Depended on by:** `oxirouter`, `oxirs`, `oxify`, `oxigdal-db-connectors`,
  `oximedia`, `oxigaf`, `oxirag`.

---

## The C-free oxisqlite engine

Before 0.1.2, OxiSQL's "Pure Rust SQLite" claim was not actually true: the
upstream limbo engine transitively pulled in C code. `oxisqlite` fixes that. It
is an in-tree fork of **limbo 0.0.22**
(commit `e59c5185ddc2b6451307324042efd81115376df1`, MIT) from which every C
touchpoint has been excised:

1. **`mimalloc` C allocator** — removed; the engine uses the system/Rust
   allocator.
2. **`lemon.c` parser generator** — removed; the generated `parse.rs` /
   `keywords.rs` are pre-generated and committed, so no C generator runs at build
   time.
3. **`built` / `git2` build-info** — removed; build metadata is hardcoded as
   `const`s instead of being collected by a C-backed build script.

Because all three are gone, the workspace builds with the C compiler disabled:

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
```

### Engine crates

| Crate | Role | ~LOC |
|-------|------|-----:|
| `oxisqlite` | Engine facade / connection entry point | 973 |
| `oxisqlite-core` | Storage engine: B-tree, pager, WAL, VDBE, transactions, ROLLBACK | 62,000 |
| `oxisqlite-ext` | Built-in extensions / virtual-table glue | 1,100 |
| `oxisqlite-macros` | Procedural macros for the engine | 900 |
| `oxisqlite-sqlite3-parser` | SQL parser (pre-generated, no `lemon`) | 14,800 |
| `oxisqlite-time` | Pure-Rust date/time helpers | 1,200 |
| `oxisqlite-uuid` | Pure-Rust UUID support | 126 |

### Fork lineage & licensing

- Base: limbo 0.0.22 (`e59c5185`, MIT).
- ROLLBACK machinery: ported from `turso_core` 0.7.0-pre.5 (MIT).
- The GPL `julian_day_converter` dependency was replaced by an inline,
  chrono-based pure-Rust implementation
  (`oxisqlite-core/functions/julian_day.rs`); the `cfg_block` crate was dropped.
- `deny.toml` allows the engine's licenses (Zlib, Unicode-3.0, MPL-2.0,
  CDLA-Permissive-2.0); `cargo deny check licenses bans sources` passes.

The full lineage and third-party attributions are recorded in the root
[`NOTICE`](NOTICE).

---

## Pure Rust — FFI eliminated

OxiSQL's default build contains no C, C++, or Fortran. Each conventional native
dependency is replaced by a Pure-Rust equivalent:

| Native library | Replaced by |
|----------------|-------------|
| `libpq` | `tokio-postgres` (Pure Rust) |
| `libmysqlclient` | `mysql_async` (Pure Rust) |
| `libsqlite3` (via `rusqlite-sys`) | `oxisqlite` (C-free fork of limbo; Pure Rust) |
| `libssl` / `native-tls` / `ring` | OxiTLS + rustls-rustcrypto |

The TLS provider is additionally hardened: `rustls-rustcrypto-patched` is applied
via `[patch.crates-io]` to fix RUSTSEC-2026-0104 (a CRL-parsing panic). The patch
drops the vulnerable `rustls-webpki` path and routes `alg_id` through
`rustls-pki-types`.

Build-time proof (the default workspace build, with the C compiler forced to
fail):

```text
CC=/usr/bin/false cargo build --workspace   # → exit 0
cargo build --workspace                      # → 0 warnings
cargo deny check licenses bans sources       # → PASS
```

`cargo deny` passes with three pre-existing advisories explicitly accepted
because no safe upgrade exists: `paste` (unmaintained), `rsa` (Marvin attack
advisory), and `rustls-pemfile` (unmaintained).

---

## Known Limitations

These are OxiSQL's own roadmap items, not upstream blockers:

- **SAVEPOINT** on the SQLite backend is not yet wired up. It returns a clear
  `OxiSqlError` rather than panicking or producing a cryptic parse error.
- **Foreign-key metadata** for the SQLite backend is reconstructed by parsing
  `sqlite_master` DDL rather than via a dedicated pragma. One FK-DDL test is
  `#[ignore]`d while this is finished.
- **Prepared-statement cache** falls back to direct execution for statements it
  cannot cache; this is transparent but not yet optimal.
- **PostgreSQL / MySQL live-server tests** are `#[ignore]`-gated and require an
  accessible server. All non-live unit and compile-time tests pass without
  external services (≈35 ignored tests workspace-wide are almost all
  live-server-gated, with a handful of fuzz/stress cases).

See [`TODO.md`](TODO.md) for the full, tracked roadmap.

---

## License, NOTICE & Authors

Licensed under the **Apache License, Version 2.0**.
See [`LICENSE`](LICENSE) for the full text and [`NOTICE`](NOTICE) for the
oxisqlite fork lineage and third-party attributions.

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
