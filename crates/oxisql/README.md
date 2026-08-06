# oxisql — The COOLJAPAN Pure-Rust SQL facade

[![Crates.io](https://img.shields.io/crates/v/oxisql.svg)](https://crates.io/crates/oxisql)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

> Unified Pure-Rust SQL facade dispatching to embedded, Postgres, MySQL, SQLite-compat, and DataFusion via a single URI-based API.

## What it is

`oxisql` is the top-level façade crate of the OxiSQL workspace. A single URI
string selects the backend; the caller receives a `Box<dyn Connection>`
regardless of which database is running. Every backend is genuinely Pure Rust —
no `libpq`, no `libmysqlclient`, no `libsqlite3`, no C/C++/Fortran. The
SQLite path now runs on **oxisqlite** (a C-free fork of limbo 0.0.22), so even
the SQLite-compatible option is C-free.

The facade re-exports the core traits and types from `oxisql-core` and adds
URI-based dispatch, connection pooling, TLS, options, introspection, a
`MultiConnection` fan-out helper, and an optional interactive REPL.

## Installation

```toml
[dependencies]
# In-memory only
oxisql = { version = "0.4.1", features = ["embedded"] }

# PostgreSQL
oxisql = { version = "0.4.1", features = ["postgres"] }

# PostgreSQL with logical replication (CDC via pgoutput)
oxisql = { version = "0.4.1", features = ["postgres-replication"] }

# MySQL
oxisql = { version = "0.4.1", features = ["mysql"] }

# Pure-Rust SQLite-compat (oxisqlite, C-free fork of limbo)
oxisql = { version = "0.4.1", features = ["sqlite"] }

# Everything + pooling + migrations
oxisql = { version = "0.4.1", features = [
    "embedded", "postgres", "mysql", "sqlite", "datafusion",
    "pool-embedded", "pool-postgres", "pool-mysql",
    "migrate",
] }
```

MSRV 1.89 · edition 2021 · Apache-2.0.

## Quick start

```rust,no_run
use oxisql::prelude::*;

#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // The URI selects the backend automatically.
    let conn = oxisql::connect("memory://").await?;

    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;
    conn.execute("INSERT INTO users VALUES ($1, $2)", &[&1i64, &"Alice"]).await?;

    // Named parameters work on every backend (:name, $name, or @name).
    let rows = conn.query_named(
        "SELECT id, name FROM users WHERE id = :id",
        &[("id", &1i64 as &dyn ToSqlValue)],
    ).await?;

    let name: String = rows[0].try_get("name")?;
    println!("name = {name}");
    Ok(())
}
```

## Key API

| Item | Signature / shape | Purpose |
|------|-------------------|---------|
| `connect(uri)` | `-> Box<dyn Connection>` | Open a connection; URI picks the backend |
| `connect_or_create(uri)` | `-> Box<dyn Connection>` | Like `connect`, creating the DB if absent (embedded always works) |
| `connect_pooled(uri, max)` | `-> Box<dyn ConnectionPool>` | Type-erased pool with `max` connections |
| `connect_pool(uri, max)` | `-> OxidbPool` | Typed pool enum for backend-specific access |
| `connect_with_options(uri, ConnectOptions)` | `-> Box<dyn Connection>` | Connect with timeout / pool size / TLS options |
| `connect_with_tls(uri, tls_cfg)` | `-> Box<dyn Connection>` | Connect with an explicit `rustls::ClientConfig` |
| `connect_datafusion(uri)` | `-> OxiSqlContext` | OLAP context (DataFusion is not a single-connection backend) |
| `ping(conn)` / `close(conn)` | `-> ()` | Backend-agnostic liveness check / teardown |
| `introspect(conn)` | `-> Vec<TableInfo>` | Schema snapshot from any backend |
| `version()` | `-> &'static str` | Crate version string |
| `backend_info_for_uri(uri)` | `-> Option<BackendInfo>` | Which backend handles a URI, without connecting |
| `BackendInfo` | `{ name, version, features }` | Backend identity / capability report |
| `ConnectOptions` | builder | `new().timeout_ms(_).pool_size(_).require_tls(_)` |
| `DEFAULT_CONNECT_TIMEOUT` | `Duration` (10s) | Default connect timeout applied to the PostgreSQL driver; override via `ConnectOptions::timeout_ms` |
| `display_error(&err)` | `-> String` | Clean, user-facing error text (strips a GlueSQL parser debug-dump artifact) |
| `MultiConnection` | struct | Fan a query out to several connections in parallel |
| `prelude` | module | `use oxisql::prelude::*` brings in traits, `Value`, `Row`, errors |

### URI scheme reference

| URI prefix | Feature flag | Backend | Notes |
|------------|--------------|---------|-------|
| `memory://` | `embedded` | GlueSQL in-memory | Fresh DB each time |
| `redb://path` | `redb` | redb B-tree | Pure Rust, file-backed |
| `fjall://path` | `fjall` | fjall LSM-tree | Pure Rust, file-backed |
| `sled://path` | `sled` | sled key-value | Pure Rust, file-backed |
| `postgres://` / `postgresql://` | `postgres` | tokio-postgres | Pure Rust, no libpq |
| `mysql://` | `mysql` | mysql_async | Pure Rust, no libmysqlclient |
| `sqlite://path` / `sqlite::memory:` | `sqlite` | oxisqlite (C-free fork of limbo) | Pure Rust, no libsqlite3 |
| `datafusion://` | `datafusion` | Apache DataFusion OLAP | Use `connect_datafusion()` |

### Named parameters

`execute_named` and `query_named` are default methods on the `Connection`
trait (defined in `oxisql-core`). They accept `&[(&str, &dyn ToSqlValue)]`,
support `:name`, `$name`, and `@name` placeholders, and rewrite them to
positional `$N` form before dispatch. Every backend inherits them — no
per-backend code, no extra imports beyond the prelude.

### Pooling

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // Type-erased pool (any backend URI):
    let pool = oxisql::connect_pooled("memory://", 4).await?;
    let conn = pool.get().await?;
    conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;

    // Or a typed OxidbPool for backend-specific access:
    let typed = oxisql::connect_pool("memory://", 4).await?;
    typed.health_check().await?;
    Ok(())
}
```

### Connection timeouts

`connect` (and `connect_with_options` / `connect_with_tls`) bound the
PostgreSQL driver with a `DEFAULT_CONNECT_TIMEOUT` of **10 seconds** — a
firewalled or black-holed Postgres host now fails cleanly with
`OxiSqlError::Timeout` instead of hanging indefinitely (previously
unbounded, and often 60-130+s even once the OS TCP stack gave up on its
own). Override it by building a `ConnectOptions` — either via
`.timeout_ms(ms)` or by parsing a `?connect_timeout=<secs>` URI query
parameter with `ConnectOptions::from_uri` — and passing it to
`connect_with_options`:

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // Override the default 10-second timeout with a short one for a
    // fast-fail health check against a possibly-unreachable host.
    let opts = oxisql::ConnectOptions::new().timeout_ms(2_000);
    let _conn = oxisql::connect_with_options("postgres://localhost/mydb", opts).await?;
    Ok(())
}
```

Embedded/local backends (`memory://`, `redb://`, `fjall://`, `sled://`,
`sqlite://`) perform no blocking connect-time I/O and are unaffected; MySQL's
`connect` dials lazily per checkout (`mysql_async` exposes no client-side
connect-timeout hook to wire in), so only PostgreSQL is actually bounded
today.

### DataFusion (OLAP)

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    // DataFusion is an OLAP engine, not a single connection — use this entry point.
    let _ctx = oxisql::connect_datafusion("datafusion://").await?;
    // _ctx is an OxiSqlContext for registering tables and running OLAP queries.
    Ok(())
}
```

### Inspecting a URI without connecting

```rust
let info = oxisql::backend_info_for_uri("memory://").unwrap();
assert_eq!(info.name, "embedded");
// info.features == ["in-memory", "sql", "gluesql"]; info.version == Some(crate version)

let info = oxisql::backend_info_for_uri("postgres://").unwrap();
assert_eq!(info.name, "postgres");
// info.version == None (known only after the handshake)
```

### Live server version

`backend_info_for_uri` never connects, so `version` is always `None` for
`postgres` / `mysql` there. Once a connection exists,
`BackendInfo::from_postgres_connection` / `::from_mysql_connection` populate
it from the live handshake instead:

```rust,no_run
#[tokio::main]
async fn main() -> Result<(), oxisql::OxiSqlError> {
    use oxisql::postgres::{PgConnection, TlsMode};

    let conn = PgConnection::connect("host=localhost user=postgres", TlsMode::Disabled).await?;
    let info = oxisql::BackendInfo::from_postgres_connection(&conn);
    println!("server version: {:?}", info.version); // e.g. Some("16.2")
    Ok(())
}
```

`from_mysql_connection` is the `mysql`-feature analogue — it is `async`
because `mysql_async` checks a connection out of its pool per call.

### Logical replication (Postgres)

With `postgres-replication` enabled (implies `postgres`), the facade
re-exports a CDC-style logical-replication client from `oxisql-postgres`
under `oxisql::postgres`: `PgReplicationConnection`, `ReplicationStream`,
`ReplicationEvent`, `LogicalReplicationMessage`, `Lsn`, `IdentifySystem`,
`CreatedSlot`, `ReplicaIdentity`, `TupleData`/`TupleColumn`/`CellValue`,
`ColumnSpec`, `RelationBody`, and the `pg_micros_to_unix_micros` /
`unix_micros_to_pg_micros` LSN-timestamp helpers. `tokio-postgres` cannot
negotiate replication mode, so this drives the `pgoutput` wire protocol
directly instead of going through the ordinary `PgConnection` path.

```rust,no_run
use futures::StreamExt;
use oxisql::postgres::{LogicalReplicationMessage, PgReplicationConnection, ReplicationEvent, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut repl = PgReplicationConnection::connect(
        "host=localhost user=postgres dbname=mydb",
        TlsMode::Disabled,
    ).await?;

    let slot = repl.create_replication_slot("my_slot", false).await?;
    let mut stream = repl
        .start_logical_replication("my_slot", &["my_publication"], slot.consistent_point)
        .await?;

    while let Some(event) = stream.next().await {
        if let ReplicationEvent::Logical {
            message: LogicalReplicationMessage::Commit { commit_lsn, .. },
            ..
        } = event?
        {
            stream.ack(commit_lsn).await?; // acknowledge after durably applying
        }
    }
    Ok(())
}
```

See the [`oxisql-postgres` README](../oxisql-postgres/README.md) for the full
pgoutput wire-protocol / tuple-decoding reference (requires PostgreSQL
`wal_level=logical`).

### Re-exports

At the crate root and via `prelude`: `Connection`, `Transaction`,
`ConnectionPool`, `Value`, `Row`, `RowSet`, `FromValue`, `ToSqlValue`,
`OxiSqlError`, the schema types (`TableInfo`, `ColumnInfo`, `IndexInfo`,
`ForeignKeyInfo`, `TableType`), and the middleware wrappers
(`LoggingConnection`, `MetricsConnection`, `MetricsSnapshot`,
`RetryConnection`, `RetryPolicy`, `RetryPredicate`).

Feature-gated module re-exports: `oxisql::postgres`, `oxisql::mysql`,
`oxisql::datafusion`, `oxisql::pool`, `oxisql::migrate`, `oxisql::cache`.

Feature-gated direct connection types: `EmbeddedConnection` (`embedded`),
`RedbEmbeddedConnection` (`redb`), `FjallEmbeddedConnection` (`fjall`),
`SledEmbeddedConnection` (`sled`), `SqliteConnection` (`sqlite`).

## Feature flags

| Feature | URI scheme | Description |
|---------|------------|-------------|
| `embedded` | `memory://` | GlueSQL in-memory engine |
| `postgres` | `postgres://` / `postgresql://` | tokio-postgres, Pure Rust |
| `postgres-replication` | `postgres://` / `postgresql://` | Logical replication (pgoutput protocol, LSN tracking, COPY BOTH streaming) via `oxisql::postgres`; implies `postgres` |
| `mysql` | `mysql://` | mysql_async, Pure Rust |
| `redb` | `redb://` | redb-backed persistent embedded SQL (implies `embedded`) |
| `fjall` | `fjall://` | fjall-backed persistent embedded SQL (implies `embedded`) |
| `sled` | `sled://` | sled-backed persistent embedded SQL (implies `embedded`) |
| `sqlite` | `sqlite://` / `sqlite::memory:` | Pure-Rust SQLite-compat via oxisqlite (C-free fork of limbo) |
| `datafusion` | `datafusion://` | Apache DataFusion OLAP layer |
| `pool-embedded` | `memory://` | In-memory connection pool |
| `pool-postgres` | `postgres://` | deadpool + tokio-postgres |
| `pool-mysql` | `mysql://` | deadpool + mysql_async |
| `pool-sqlite-compat` | `sqlite://` | Pool over the oxisqlite backend |
| `migrate` | — | SQL migration runner via `oxisql-migrate` |
| `cache` | — | SQL query-result / prepared-plan cache via `oxisql-cache` (LRU + TTL) |
| `repl` | — | `oxisql-repl` binary (implies `embedded`) |

### REPL binary

With `--features repl`, the `oxisql-repl` binary connects to any URI (default
`memory://`) and offers dot commands: `.help`, `.tables`, `.schema <table>`,
`.mode table|csv|json`, `.timer on|off`, `.read <file>`, `.history`, and
`.quit` (aliases `.exit` / `.q`). `SELECT`/`WITH`/`EXPLAIN` render in the
active `.mode` (table/CSV/JSON); other statements report a row count.
Errors are rendered through `oxisql::display_error`, which strips a leaky
GlueSQL parser debug-dump down to its clean message instead of printing a
raw multi-line Rust `Debug` dump.

```bash
cargo run --features repl --bin oxisql-repl -- "memory://"
```

## Test coverage

**60 tests** pass with default features; **135 tests** pass with
`--all-features` (the jump comes from feature-gated `postgres-replication`
unit tests) — 4 more are `#[ignore]`d live-server-gated tests (Postgres/MySQL
portability plus `connect_or_create` auto-create) requiring a running
Postgres / MySQL. Zero failures, zero clippy/rustdoc warnings either way.

## Part of the OxiSQL workspace

`oxisql` is one of 18 crates in the OxiSQL workspace (11 facade/driver crates
plus a 7-crate C-free `oxisqlite-*` engine). See the
[workspace README](../../README.md) for the full architecture, backend matrix,
and the 2,261 workspace tests (2,755 with `--all-features`).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan).
Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
