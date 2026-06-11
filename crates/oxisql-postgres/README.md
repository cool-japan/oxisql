# oxisql-postgres — Pure-Rust PostgreSQL backend for OxiSQL

> PostgreSQL backend over the `tokio-postgres` wire protocol with OxiTLS / RustCrypto TLS
> (no `libpq`, no `openssl-sys`); COPY, LISTEN/NOTIFY, pipeline batching, and extended type mapping.

[![Crates.io](https://img.shields.io/crates/v/oxisql-postgres.svg)](https://crates.io/crates/oxisql-postgres)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.89](https://img.shields.io/badge/MSRV-1.89-orange.svg)](https://blog.rust-lang.org/2025/08/07/Rust-1.89.0.html)
[![Pure Rust: C-free](https://img.shields.io/badge/Pure%20Rust-C--free-brightgreen.svg)](#what-it-is)

**Status: Stable.**

## What it is

`oxisql-postgres` provides [`PgConnection`], which implements
`oxisql_core::Connection` over [`tokio-postgres`] — speaking the PostgreSQL
**Frontend/Backend Protocol version 3** directly on the wire. There is **no C
client library**: no `libpq`, no `libssl`, no `openssl-sys`, and no `ring`. TLS
is routed entirely through OxiTLS and the `rustls` + `rustls-rustcrypto`
RustCrypto provider, so a build of this crate is 100% Pure Rust and needs no
system C toolchain or `*-dev` headers.

Beyond plain `execute`/`query`, the crate exposes the higher-value parts of the
PostgreSQL protocol that thin wrappers usually omit: the **COPY** bulk
load/unload protocol, **LISTEN/NOTIFY** asynchronous notifications delivered as
a `Stream`, **pipeline batching** (many statements flushed in a single network
round-trip), prepared-statement caching, and an extended type mapping that
round-trips all 13 `oxisql_core::Value` variants (UUID, JSONB, NUMERIC, arrays,
timestamps, …).

It is part of the [OxiSQL](../../README.md) Pure-Rust workspace and is the
backend selected when you open a `postgres://` URL through the `oxisql` facade.

## Installation

```toml
[dependencies]
oxisql-postgres = "0.1.2"
```

MSRV 1.89 · edition 2021 · Apache-2.0.

## Quick start

```rust,no_run
use oxisql_postgres::{PgConnection, TlsMode};
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Plain TCP (suitable for localhost / trusted networks).
    let conn = PgConnection::connect(
        "host=localhost port=5432 user=postgres password=secret dbname=mydb",
        TlsMode::Disabled,
    ).await?;

    conn.execute("CREATE TABLE IF NOT EXISTS t (id BIGINT, val TEXT)", &[]).await?;
    conn.execute("INSERT INTO t VALUES ($1, $2)", &[&1i64, &"hello"]).await?;

    let rows = conn.query("SELECT id, val FROM t WHERE id = $1", &[&1i64]).await?;
    let id: i64 = rows[0].try_get("id")?;
    println!("id = {id}");
    Ok(())
}
```

### TLS via OxiTLS (RustCrypto provider)

```rust,no_run
use oxisql_postgres::{PgConnection, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Trust the Mozilla CA bundle, built with the pure-Rust RustCrypto provider.
    let root_store = oxitls::webpki_root_certs();
    let client_cfg = oxitls::client_config(root_store)
        .map_err(|e| format!("TLS config: {e}"))?;

    let conn = PgConnection::connect(
        "host=db.example.com port=5432 user=postgres dbname=mydb sslmode=require",
        TlsMode::Rustls(client_cfg),
    ).await?;
    let _ = conn;
    Ok(())
}
```

Two ready-made `TlsMode` constructors cover common cases without hand-building a
`ClientConfig`: `TlsMode::skip_verify()` (accept any server certificate — dev
only) and `TlsMode::with_ca_pem(pem_bytes)` (trust a custom CA from PEM). Both
default to the RustCrypto crypto provider.

### Fluent builder

```rust,no_run
use oxisql_postgres::{PgConnectionBuilder, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = PgConnectionBuilder::new()
        .host("localhost")
        .port(5432)
        .user("postgres")
        .password("secret")
        .dbname("mydb")
        .connect_timeout_secs(5)
        .tls_mode(TlsMode::Disabled)
        .connect()
        .await?;
    let _ = conn;
    Ok(())
}
```

## Key API

| Item | Description |
|------|-------------|
| `PgConnection::connect(conn_str, tls)` | Connect with a libpq-style `key=value` string **or** a `postgres://` / `postgresql://` URL (auto-detected) |
| `PgConnection::connect_with_timeout(conn_str, tls, dur)` | As above, wrapped in a Tokio timeout |
| `PgConnection::connect_skip_verify(conn_str)` / `connect_with_ca(conn_str, pem)` | TLS connect shortcuts |
| `PgConnection::from_client(client)` | Wrap an existing `tokio_postgres::Client` |
| `PgConnectionBuilder` | Fluent config: `host` / `port` / `user` / `password` / `dbname` / `connect_timeout_secs` / `tls_mode` (also `tls_skip_verify` / `tls_with_ca_pem`) → `connect()` |
| `TlsMode` | `Disabled` or `Rustls(ClientConfig)`; constructors `skip_verify()`, `with_ca_pem(pem)` |
| `PgTransaction` | `oxisql_core::Transaction` with `savepoint` / `release_savepoint` / `rollback_to_savepoint` |
| `PgPrepared` | Caches `tokio_postgres::Statement`, keyed by a hash of the SQL text |
| `PgPipeline` | `add_execute(sql, params)` / `add_query(sql, params)` / `finish()` → `PipelineResult` — batch in one round-trip |
| `NotificationStream` / `PgNotification` | Async stream returned by `listen(channel)` |
| `ColumnDescription` | Column name + type metadata via `describe(sql)` (no execution) |
| `parse_pg_conn_str(s)` → `PgConnParts` | Parse a connection string / URL into its parts |

## Capabilities

### COPY — bulk load / unload (`src/copy.rs`)

```rust,no_run
# async fn run(conn: &oxisql_postgres::PgConnection) -> Result<(), Box<dyn std::error::Error>> {
// Bulk-insert via COPY ... FROM STDIN (text/TSV).
let rows = vec![
    vec!["1".to_string(), "alice".to_string()],
    vec!["2".to_string(), "bob".to_string()],
];
let n = conn.copy_in_text("users", &["id", "name"], rows.into_iter()).await?;

// Extract rows via COPY ... TO STDOUT.
let out: Vec<Vec<String>> = conn.copy_out_text("users", &["id", "name"]).await?;
let _ = (n, out);
# Ok(())
# }
```

### LISTEN / NOTIFY (`src/notify.rs`)

```rust,no_run
# async fn run(conn: &oxisql_postgres::PgConnection) -> Result<(), Box<dyn std::error::Error>> {
use std::time::Duration;

let mut stream = conn.listen("my_channel").await?;   // NotificationStream
conn.notify("my_channel", "ping").await?;
if let Some(n) = stream.recv_timeout(Duration::from_secs(1)).await {
    println!("{} -> {}", n.channel, n.payload);       // PgNotification
}
stream.unlisten().await?;
# Ok(())
# }
```

Notifications are only routed on connections created via `PgConnection::connect`
(the background driver that dispatches `NotificationResponse` is spawned there);
they are not available on a `from_client` connection.

### Pipeline batching (`src/pipeline.rs`)

```rust,no_run
# async fn run(conn: &oxisql_postgres::PgConnection) -> Result<(), Box<dyn std::error::Error>> {
let mut pipeline = conn.pipeline();
pipeline.add_execute("INSERT INTO t VALUES ($1)", &[&1i64]);
pipeline.add_query("SELECT * FROM t", &[]);
let result = pipeline.finish().await?;   // PipelineResult, single round-trip
let _ = result;
# Ok(())
# }
```

### Prepared statements, describe & introspection

`prepare(sql)` returns a `PgPrepared` whose underlying `Statement` is cached by
SQL hash, so re-preparing the same text reuses the server-side plan.
`describe(sql)` returns `Vec<ColumnDescription>` without executing the query.
Schema introspection (`tables`, `columns`, `indexes`, `foreign_keys`) is served
from `information_schema` and `pg_indexes`.

## Type mapping

All 13 `oxisql_core::Value` variants round-trip. Parameters are encoded with
explicit type OIDs; results can be requested in binary format (`query_binary`).

| PostgreSQL type | `oxisql_core::Value` |
|-----------------|----------------------|
| `BOOL` | `Bool` |
| `INT2` / `INT4` / `INT8` | `I64` |
| `FLOAT4` / `FLOAT8` | `F64` |
| `TEXT` / `VARCHAR` / `BPCHAR` | `Text` |
| `BYTEA` | `Blob` |
| `TIMESTAMP` / `TIMESTAMPTZ` | `Timestamp` (µs since epoch) |
| `DATE` | `Date` (days since epoch) |
| `TIME` | `Time` (µs since midnight) |
| `UUID` | `Uuid` (`u128`) |
| `JSONB` | `Json` |
| `NUMERIC` | `Decimal` (string) |
| `ARRAY` (e.g. `INT[]`, `TEXT[]`) | `Array` |

## Test coverage

**61 tests passing** under `cargo test` (which includes doctests). In addition,
6 integration tests are live-server-gated (TLS live connect, COPY IN/OUT, and
4× LISTEN/NOTIFY). These live-server tests are marked `#[ignore]` because they
require a real PostgreSQL instance; the remaining `#[ignore]`d integration tests
(CRUD cycles, isolation, reconnect, pooling) are also gated the same way and are
skipped when no server is available. Enable them with a running server and the
`integration-postgres` feature.

```bash
cargo test -p oxisql-postgres                              # 61 passed (incl. doctests), live tests skipped
cargo nextest run -p oxisql-postgres --run-ignored all     # also runs live-server tests (nextest excludes doctests)
```

## Part of the OxiSQL workspace

This crate is one of 17 crates in the Pure-Rust [OxiSQL workspace](../../README.md)
(1,720 workspace tests pass). It is the PostgreSQL backend behind the unified
`oxisql` facade; see the workspace README for the embedded engine, MySQL
backend, connection pool, migrations, and DataFusion integration.

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).

[`PgConnection`]: https://docs.rs/oxisql-postgres
[`tokio-postgres`]: https://crates.io/crates/tokio-postgres
