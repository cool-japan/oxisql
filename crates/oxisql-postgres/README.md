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
oxisql-postgres = "0.4.1"
# oxisql-postgres = { version = "0.4.1", features = ["replication"] }  # PostgreSQL logical replication (pgoutput)
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
| `PgConnection::server_version()` | Raw `server_version` runtime parameter captured from the connection handshake's `ParameterStatus` message (e.g. `"16.4 (Debian 16.4-1.pgdg120+1)"`); `None` for `from_client` connections |

### Connection timeouts

`PgConnection::connect` itself applies no timeout — connecting to an
unreachable host can hang. `PgConnection::connect_with_timeout(conn_str, tls,
duration)` wraps the same connect path in a `tokio::time::timeout`, and as of
0.3.3 it is also what the `oxisql` facade calls into: `oxisql::connect()` /
`connect_with_options` / `connect_with_tls` now go through
`connect_with_timeout` with a 10-second default (overridable via the facade's
`ConnectOptions::connect_timeout_ms`), so a connection attempt that used to
hang indefinitely now fails with a typed timeout error instead. The fix lives
here, in this crate's `connect_with_timeout`; what's new in 0.3.3 is that the
facade's plain `connect()` applies it automatically rather than leaving the
connection unbounded.

## Feature flags

| Feature | Default | Effect |
|---------|---------|--------|
| *(none)* | — | Core `Connection` impl, COPY, LISTEN/NOTIFY, pipeline batching, prepared statements, and the full type mapping — no extra dependencies |
| `integration-postgres` | off | Compiles in the live-server integration test suites under `tests/`; every test they add is individually `#[ignore]`d and needs a running PostgreSQL server |
| `replication` | off | PostgreSQL logical replication (`pgoutput`) support — see "Logical replication" under Capabilities below. Pulls in `postgres-protocol` + `fallible-iterator-02` |

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

### Logical replication (`replication` feature, `src/replication/`)

Behind the non-default `replication` Cargo feature, this crate can act as a
**logical-replication client** speaking PostgreSQL's `pgoutput` output-plugin
wire format directly. `tokio-postgres` cannot negotiate `CopyBoth`/replication
mode, so this path never goes through it: `PgReplicationConnection` drives
the wire protocol itself (via `postgres-protocol` + `fallible-iterator`),
reusing the crate's own connection-string parsing and full TLS / SCRAM-SHA-256
auth stack.

```rust,no_run
use futures::StreamExt;
use oxisql_postgres::{LogicalReplicationMessage, PgReplicationConnection, ReplicationEvent, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut repl = PgReplicationConnection::connect(
        "host=localhost user=postgres dbname=mydb",
        TlsMode::Disabled,
    ).await?;

    let ident = repl.identify_system().await?;
    println!("system id: {}", ident.systemid);

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

(Requires the `replication` feature; `cargo add oxisql-postgres --features replication`.)

- **Connection setup** — `PgReplicationConnection::connect` performs the
  `replication=database` handshake (a separate connection mode from ordinary
  `PgConnection`), then `identify_system()` / `create_replication_slot(name,
  temporary)` / `drop_replication_slot(name)` run over the simple-query
  protocol.
- **Streaming** — `start_logical_replication(slot_name, publication_names,
  start_lsn)` consumes the connection and returns a `ReplicationStream`
  (`impl futures::Stream<Item = Result<ReplicationEvent, PgError>>`), backed by
  a background reader task plus a periodic Standby Status Update keepalive
  task (every 10s).
- **Events** — `ReplicationEvent::Logical` carries a decoded
  `LogicalReplicationMessage` (`Begin` / `Commit` / `Origin` / `Relation` /
  `Type` / `Insert` / `Update` / `Delete` / `Truncate` / `Message`);
  `ReplicationEvent::KeepAlive` is a server liveness probe.
- **Progress tracking** — `stream.ack(lsn)` (or the more general
  `stream.standby_status_update(written, flushed, applied)`) after durably
  applying a transaction.
- **Tuple decoding** — `stream.relation(rel_id)` returns the cached
  `RelationBody` schema; `stream.decode_tuple(rel_id, tuple)` maps a
  `TupleData` to `Vec<CellValue>` (each cell either a decoded
  `oxisql_core::Value` or `UnchangedToast`, for an un-retransmitted TOASTed
  column). Both `pgoutput` wire formats are decoded — text (the format this
  MVP always negotiates) and binary — including PostgreSQL's array-literal
  text syntax (`{1,2,3}`, `{}`, `{NULL,2}`, quoting/escaping, multi-dimensional
  arrays).
- **LSN helpers** — `Lsn` wraps a WAL position and round-trips PostgreSQL's
  canonical text form (`"16/B374D848"`) via `FromStr`/`Display`;
  `pg_micros_to_unix_micros` / `unix_micros_to_pg_micros` convert between the
  PostgreSQL-epoch and Unix-epoch timestamp conventions used on the wire.

This is an MVP scoped to what a CDC (change-data-capture) consumer needs
first: **not** implemented yet are streaming of large in-progress transactions
(`streaming 'on'`), two-phase commit, and parallel streaming (see the crate's
`TODO.md` for the tracked follow-ups); physical replication is out of scope
entirely (a different protocol from `pgoutput` decoding). Live-server
integration tests live in `tests/replication.rs`, gated behind
`integration-postgres,replication` and individually `#[ignore]`d pending a
real `wal_level=logical` server.

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

**68 tests passing** under plain `cargo test` (no extra features; includes 15
doctests), plus 6 `#[ignore]`d live-server tests (TLS live connect, COPY
IN/OUT, 4× LISTEN/NOTIFY) that are skipped without a real PostgreSQL instance.

With `--all-features` (adds `integration-postgres` + `replication`) the suite
grows substantially: 393 tests run under `cargo nextest` (doctests excluded),
plus 41 more that stay `#[ignore]`d pending a live server — the 6 above, a
30-test CRUD/isolation/reconnect/pooling/prepared-statement/type-mapping
integration suite, and the 5-test `tests/replication.rs` logical-replication
suite (needs `wal_level=logical`; see "Logical replication" above). The
`replication` feature's own unit-test suite alone (`cargo test -p
oxisql-postgres --features replication --lib`) totals 339 tests, covering
`pgoutput` message decoding, LSN parsing, `CopyBoth` framing, SCRAM-SHA-256
auth, and text-/binary-format tuple and array decoding.

```bash
cargo test -p oxisql-postgres                                          # 68 passed (incl. doctests), live tests skipped
cargo nextest run -p oxisql-postgres --all-features --run-ignored all  # also runs every live-server test (needs a real server)
```

## Part of the OxiSQL workspace

This crate is one of 18 crates in the Pure-Rust [OxiSQL workspace](../../README.md)
(2,261 workspace tests pass, 2,755 with `--all-features`). It is the PostgreSQL backend behind the unified
`oxisql` facade; see the workspace README for the embedded engine, MySQL
backend, connection pool, migrations, and DataFusion integration.

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).

[`PgConnection`]: https://docs.rs/oxisql-postgres
[`tokio-postgres`]: https://crates.io/crates/tokio-postgres
