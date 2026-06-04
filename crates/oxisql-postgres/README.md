# oxisql-postgres — Pure-Rust PostgreSQL backend for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-postgres.svg)](https://crates.io/crates/oxisql-postgres)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-postgres` provides `PgConnection`, which implements `oxisql_core::Connection` over `tokio-postgres`. There are no C bindings — no `libpq`, no `openssl-sys`. TLS uses `rustls` + `rustls-rustcrypto`.

## Installation

```toml
[dependencies]
oxisql-postgres = "0.1.1"
```

## Quick Start

```rust
use oxisql_postgres::{PgConnection, TlsMode};
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // No TLS
    let conn = PgConnection::connect(
        "host=localhost port=5432 user=postgres password=secret dbname=mydb",
        TlsMode::Disabled,
    ).await?;

    conn.execute("CREATE TABLE IF NOT EXISTS t (id BIGINT, val TEXT)", &[]).await?;
    conn.execute("INSERT INTO t VALUES ($1, $2)", &[&1i64, &"hello"]).await?;

    let rows = conn.query("SELECT id, val FROM t WHERE id = $1", &[&1i64]).await?;
    let id: i64 = rows[0].try_get("id")?;
    println!("id={id}");
    Ok(())
}
```

## Quick Start (TLS via OxiTLS)

```rust
use oxisql_postgres::{PgConnection, TlsMode};

let root_store = oxitls::webpki_root_certs();
let client_cfg = oxitls::client_config(root_store)
    .map_err(|e| format!("TLS cfg: {e}"))?;

let conn = PgConnection::connect(
    "host=db.example.com port=5432 user=postgres sslmode=require",
    TlsMode::Rustls(client_cfg),
).await?;
```

## API Overview

### `PgConnection`

| Method / Function | Description |
|-------------------|-------------|
| `PgConnection::connect(conn_str, tls)` | Connect using a libpq-style connection string |
| `PgConnection::connect_url(url, tls)` | Connect using a `postgres://` URL |
| `PgConnection::from_client(client, handle)` | Wrap an existing `tokio_postgres::Client` |
| `parse_pg_conn_str(s)` | Parse a connection string into `PgConnParts` |

The connection string format follows the libpq key=value syntax: `host=`, `port=`, `user=`, `password=`, `dbname=`, `sslmode=`.

### `TlsMode`

| Variant | Description |
|---------|-------------|
| `TlsMode::Disabled` | No TLS (plain TCP) |
| `TlsMode::Rustls(ClientConfig)` | TLS via rustls + rustls-rustcrypto |

### `PgConnectionBuilder`

Fluent builder for fine-grained connection configuration:

```rust
use oxisql_postgres::PgConnectionBuilder;

let conn = PgConnectionBuilder::new()
    .host("localhost")
    .port(5432)
    .user("postgres")
    .password("secret")
    .dbname("mydb")
    .connect_timeout(std::time::Duration::from_secs(5))
    .tls(TlsMode::Disabled)
    .connect()
    .await?;
```

### `PgTransaction`

Obtained via `conn.transaction()`. Implements `oxisql_core::Transaction`.

Supports `savepoint()`, `release_savepoint()`, and `rollback_to_savepoint()`.

Dropping a `PgTransaction` without explicit commit schedules a `ROLLBACK` on the active Tokio runtime.

### Extended type mapping

| PostgreSQL type | OxiSQL `Value` variant |
|-----------------|------------------------|
| `BOOLEAN` | `Value::Bool` |
| `INT2 / INT4 / INT8` | `Value::I64` |
| `FLOAT4 / FLOAT8` | `Value::F64` |
| `TEXT / VARCHAR / CHAR` | `Value::Text` |
| `BYTEA` | `Value::Blob` |
| `TIMESTAMP / TIMESTAMPTZ` | `Value::Timestamp` (µs since epoch) |
| `DATE` | `Value::Date` (days since epoch) |
| `TIME / TIMETZ` | `Value::Time` (µs since midnight) |
| `UUID` | `Value::Uuid` (u128) |
| `JSON / JSONB` | `Value::Json` |
| `NUMERIC / DECIMAL` | `Value::Decimal` (string) |
| `INTEGER[] / TEXT[]` etc. | `Value::Array` |

### Advanced features

#### Prepared statements

```rust
let stmt = conn.prepare("SELECT * FROM users WHERE id = $1").await?;
let rows = stmt.query(&[&42i64]).await?;
```

#### Column description (without executing)

```rust
let desc: Vec<ColumnDescription> = conn.describe("SELECT id, name FROM users").await?;
// desc[0].name == "id", desc[0].type_name == "int8"
```

#### Pipeline (batch multiple queries in one network round-trip)

```rust
use oxisql_postgres::PgPipeline;
let mut pipeline = conn.pipeline();
pipeline.add_execute("INSERT INTO t VALUES ($1)", &[&1i64]);
pipeline.add_query("SELECT * FROM t");
let results: Vec<PipelineResult> = pipeline.run().await?;
```

#### LISTEN / NOTIFY

```rust
conn.listen("my_channel").await?;
let mut stream: NotificationStream = conn.notifications();
// stream yields PgNotification { channel, payload }
```

#### COPY (bulk load/unload)

The `copy` module exposes `copy_in` and `copy_out` for PostgreSQL's COPY protocol.

### Wire Protocol

Uses **Frontend/Backend Protocol version 3** via `tokio-postgres`:

| API path | Wire mode |
|----------|-----------|
| `execute` / `query` | Extended-query protocol |
| `execute_batch` | Simple-query protocol |
| `prepare` / `describe` | Extended-query Parse + Describe |
| `pipeline` | Multiple extended-query cycles, single send buffer |

### Schema introspection

`tables()`, `columns(table)`, `indexes(table)`, `foreign_keys(table)` are all supported and query the PostgreSQL information schema / system catalogs.

## Known Limitations

- Logical replication (Streaming Replication Protocol) is not exposed.
- `LISTEN` notifications are only available on connections created via `PgConnection::connect` (not `from_client`).
- PostgreSQL protocol v2 (pre-7.4 servers) is not supported.

## Test Status

As of 2026-05-30: **49 tests passing, 36 skipped** (live-Postgres tests are `#[ignore]`d and skipped when no server is available).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
