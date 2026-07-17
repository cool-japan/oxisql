# oxisql-mysql — Pure-Rust MySQL backend for OxiSQL

> MySQL backend over the `mysql_async` wire protocol with `rustls-rustcrypto` TLS
> (no `libmysqlclient`, no `openssl-sys`); LOAD DATA bulk ingestion,
> stored-procedure multi-result-sets, and the MySQL binary protocol.

[![Crates.io](https://img.shields.io/crates/v/oxisql-mysql.svg)](https://crates.io/crates/oxisql-mysql)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV: 1.89](https://img.shields.io/badge/MSRV-1.89-orange.svg)](https://blog.rust-lang.org/2025/08/07/Rust-1.89.0.html)
[![Pure Rust: C-free](https://img.shields.io/badge/Pure%20Rust-C--free-brightgreen.svg)](#what-it-is)

**Status: Stable.**

## What it is

`oxisql-mysql` provides [`MyConnection`], which implements
`oxisql_core::Connection` over [`mysql_async`] — speaking the MySQL client/server
protocol directly on the wire. There is **no C client library**: no
`libmysqlclient`, no `libssl`, no `openssl-sys`, and no `ring`. TLS is provided
by `rustls` + `rustls-rustcrypto` (the RustCrypto provider), so a build of this
crate is 100% Pure Rust and needs no system C toolchain or `*-dev` headers.

Every query runs through `prep()` + `exec()` on the **binary protocol**, which
gives server-side prepared-statement caching for free. On top of plain
`execute`/`query`, the crate exposes the parts of MySQL that thin wrappers tend
to skip: **`load_data_batched`** for high-throughput bulk ingestion (batched
multi-row `INSERT` over the binary protocol — no `LOCAL INFILE` server
permission required) and **`call_procedure_multi`** for stored procedures that
return several result sets at once.

The connection is backed by an internal `mysql_async::Pool` (internally
reference-counted, so `Clone` is cheap and no extra `Mutex` is needed). It is
part of the [OxiSQL](../../README.md) Pure-Rust workspace and is the backend
selected when you open a `mysql://` URL through the `oxisql` facade.

## Installation

```toml
[dependencies]
oxisql-mysql = "0.3.3"
```

MSRV 1.89 · edition 2021 · Apache-2.0.

## Quick start

```rust,no_run
use oxisql_mysql::{MyConnection, TlsMode};
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = MyConnection::connect(
        "mysql://root:secret@localhost:3306/mydb",
        TlsMode::Disabled,
    ).await?;

    conn.execute("CREATE TABLE IF NOT EXISTS t (id BIGINT, val TEXT)", &[]).await?;
    conn.execute("INSERT INTO t VALUES (?, ?)", &[&1i64, &"hello"]).await?;

    let rows = conn.query("SELECT id, val FROM t WHERE id = ?", &[&1i64]).await?;
    let id: i64 = rows[0].try_get("id")?;
    println!("id = {id}");
    Ok(())
}
```

SQL uses `?` placeholders at the `mysql_async` level; OxiSQL's `$1` / `$2` style
parameters are translated automatically.

### TLS via rustls-rustcrypto

```rust,no_run
use std::sync::Arc;
use oxisql_mysql::{MyConnection, TlsMode};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Build a ClientConfig with the pure-Rust RustCrypto provider.
    // mysql_async builds its own SslOpts; this config installs the provider.
    let provider = Arc::new(rustls_rustcrypto::provider());
    let root_store = rustls::RootCertStore::empty();
    let cfg = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(root_store)
        .with_no_client_auth();

    let conn = MyConnection::connect(
        "mysql://root:secret@db.example.com:3306/mydb",
        TlsMode::Rustls(Arc::new(cfg)),
    ).await?;
    let _ = conn;
    Ok(())
}
```

### Fluent builder

The builder configures connection parameters, pool sizing, timeouts, and TLS
through convenience methods (no hand-built `ClientConfig` needed):

```rust,no_run
use oxisql_mysql::MyConnectionBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = MyConnectionBuilder::new()
        .host("localhost")
        .port(3306)
        .user("root")
        .password("secret")
        .dbname("mydb")
        .connect_timeout_secs(5)
        .ssl_skip_verify()       // dev only; or .ssl_with_ca_pem(pem) / .ssl_disabled()
        .connect()
        .await?;
    let _ = conn;
    Ok(())
}
```

## Key API

| Item | Description |
|------|-------------|
| `MyConnection::connect(url, tls)` | Connect with a `mysql://user:pass@host:port/db` URL |
| `MyConnection::from_pool(pool)` | Wrap an existing `mysql_async::Pool` |
| `MyConnection::disconnect()` | Drain all pool connections for graceful shutdown |
| `MyConnectionBuilder` | Fluent config: `host` / `port` / `user` / `password` / `dbname` / `connect_timeout_secs` / `ssl_disabled` / `ssl_skip_verify` / `ssl_with_ca_pem` / `pool_*` → `connect()` |
| `TlsMode` | `Disabled` or `Rustls(Arc<ClientConfig>)` |
| `MyTransaction` | `oxisql_core::Transaction` with `savepoint` / `release_savepoint` / `rollback_to_savepoint` + `last_insert_id()` |
| `MySqlPrepared` | `oxisql_core::PreparedStatement` over a server-side prepared statement |
| `load_data_batched(table, cols, rows, batch_size)` | Batched multi-row `INSERT` (binary protocol, no `LOCAL INFILE` permission needed) |
| `call_procedure_multi(name, params)` | Call a stored procedure; returns **all** result sets |
| `mysql_url_parts(url)` → `MysqlUrlParts` | Parse `host` / `port` / `dbname` / `user` from a `mysql://` URL |
| `is_reconnect_error(err)` | Identify transient errors (`CR_SERVER_GONE`, `CR_SERVER_LOST`, I/O, …) that are safe to retry |
| `MyConnection::server_version()` | `async` — acquire a pooled connection and return the MySQL/MariaDB server version as `"{major}.{minor}.{patch}"` (e.g. `"8.0.35"`) |

## Capabilities

### LOAD DATA — bulk ingestion

```rust,no_run
# async fn run(conn: &oxisql_mysql::MyConnection) -> Result<(), Box<dyn std::error::Error>> {
use oxisql_core::Value;

let rows = vec![
    vec![Value::I64(1), Value::Text("alice".into())],
    vec![Value::I64(2), Value::Text("bob".into())],
];
// Batched multi-row INSERT over the binary protocol; no LOCAL INFILE needed.
let inserted = conn
    .load_data_batched("users", &["id", "name"], rows, 1000)
    .await?;
let _ = inserted;
# Ok(())
# }
```

### Stored procedures — multiple result sets

```rust,no_run
# async fn run(conn: &oxisql_mysql::MyConnection) -> Result<(), Box<dyn std::error::Error>> {
use oxisql_core::Value;

// Returns every result set the procedure emits, in order.
let result_sets = conn
    .call_procedure_multi("report_summary", vec![Value::I64(2026)])
    .await?;
for (i, rows) in result_sets.iter().enumerate() {
    println!("result set {i}: {} rows", rows.len());
}
# Ok(())
# }
```

### Binary protocol & introspection

All queries go through `prep()` + `exec()`, so the server caches prepared
statements automatically. Schema introspection (`tables`, `columns`, `indexes`,
`foreign_keys`) is served from `INFORMATION_SCHEMA`.

## Type mapping

| MySQL type | `oxisql_core::Value` |
|------------|----------------------|
| `TINYINT(1)` | `Bool` |
| `TINYINT` / `SMALLINT` / `INT` / `BIGINT` | `I64` |
| `FLOAT` / `DOUBLE` | `F64` |
| `TEXT` / `VARCHAR` / `CHAR` | `Text` |
| `BLOB` / `BINARY` / `VARBINARY` | `Blob` |
| `DATETIME` / `TIMESTAMP` | `Timestamp` |
| `DATE` | `Date` |
| `TIME` | `Time` |
| `DECIMAL` / `NUMERIC` | `Decimal` |
| `JSON` | `Json` |

Unsigned values greater than `i64::MAX` gracefully fall back to `Value::Text`
rather than panicking.

## Test coverage

**102 tests passing** under `cargo test` (96 unit/integration tests plus 6
doctests). The live-server integration tests are individually gated behind
**both** `#[cfg(feature = "integration-mysql")]` and `#[ignore]` (they need a
real MySQL 8.x server), so they are not merely skipped but not even compiled
in without the feature.

```bash
cargo test -p oxisql-mysql                                                    # 102 passed (incl. doctests), live tests absent
cargo test -p oxisql-mysql --features integration-mysql -- --include-ignored  # also runs live-server tests
```

## Part of the OxiSQL workspace

This crate is one of 17 crates in the Pure-Rust [OxiSQL workspace](../../README.md)
(2,157 workspace tests pass, 2,651 with `--all-features`). It is the MySQL backend behind the unified `oxisql`
facade; see the workspace README for the embedded engine, PostgreSQL backend,
connection pool, migrations, and DataFusion integration.

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).

[`MyConnection`]: https://docs.rs/oxisql-mysql
[`mysql_async`]: https://crates.io/crates/mysql_async
