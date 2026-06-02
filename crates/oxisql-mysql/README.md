# oxisql-mysql — Pure-Rust MySQL backend for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-mysql.svg)](https://crates.io/crates/oxisql-mysql)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-mysql` provides `MyConnection`, which implements `oxisql_core::Connection` over `mysql_async`. There are no C bindings — no `libmysqlclient`, no `openssl-sys`. TLS uses `rustls` + `rustls-rustcrypto`.

## Installation

```toml
[dependencies]
oxisql-mysql = "0.1.0"
```

## Quick Start

```rust
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
    println!("id={id}");
    Ok(())
}
```

## Quick Start (TLS via rustls-rustcrypto)

```rust
use std::sync::Arc;
use oxisql_mysql::{MyConnection, TlsMode};

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
```

## API Overview

### `MyConnection`

| Method | Description |
|--------|-------------|
| `MyConnection::connect(url, tls)` | Connect to MySQL using a `mysql://` URL |
| Implements `Connection` trait | `execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream` |

Parameters in SQL use `?` placeholders at the mysql_async level. OxiSQL's `$1` / `$2` style parameters are translated automatically.

### `MyConnectionBuilder`

Fluent builder for fine-grained connection configuration:

```rust
use oxisql_mysql::MyConnectionBuilder;

let conn = MyConnectionBuilder::new()
    .url("mysql://root:secret@localhost/mydb")
    .tls(TlsMode::Disabled)
    .connect()
    .await?;
```

### `TlsMode`

| Variant | Description |
|---------|-------------|
| `TlsMode::Disabled` | No TLS (plain TCP) |
| `TlsMode::Rustls(Arc<ClientConfig>)` | TLS via rustls + rustls-rustcrypto |

### `MyTransaction`

Obtained via `conn.transaction()`. Implements `oxisql_core::Transaction`.

Supports `savepoint()`, `release_savepoint()`, and `rollback_to_savepoint()`.

### Extended type mapping

| MySQL type | OxiSQL `Value` variant |
|------------|------------------------|
| `TINYINT(1)` | `Value::Bool` |
| `TINYINT / SMALLINT / INT / BIGINT` | `Value::I64` |
| `FLOAT / DOUBLE` | `Value::F64` |
| `TEXT / VARCHAR / CHAR` | `Value::Text` |
| `BLOB / BINARY / VARBINARY` | `Value::Blob` |
| `DATETIME / TIMESTAMP` | `Value::Timestamp` (µs since epoch) |
| `DATE` | `Value::Date` (days since epoch) |
| `TIME` | `Value::Time` (µs since midnight) |
| `DECIMAL / NUMERIC` | `Value::Decimal` (string) |
| `JSON` | `Value::Json` |

### `MySqlPrepared`

Prepared statements via `conn.prepare(sql)`. Implements `oxisql_core::PreparedStatement`.

```rust
let stmt = conn.prepare("SELECT * FROM users WHERE id = ?").await?;
let rows = stmt.query(&[&42i64]).await?;
```

### URL parsing utility

```rust
use oxisql_mysql::mysql_url_parts;

let parts = mysql_url_parts("mysql://alice:secret@db.example.com:3307/shop")
    .expect("valid URL");
assert_eq!(parts.host, "db.example.com");
assert_eq!(parts.port, 3307);
assert_eq!(parts.dbname, Some("shop".to_string()));
assert_eq!(parts.user, Some("alice".to_string()));
```

`MysqlUrlParts` fields: `host: String`, `port: u16`, `dbname: Option<String>`, `user: Option<String>`.

### Schema introspection

`tables()`, `columns(table)`, `indexes(table)`, and `foreign_keys(table)` are supported and query `INFORMATION_SCHEMA`.

### Auto-reconnect

`is_reconnect_error(err)` is exposed to identify transient connection errors that are safe to retry.

## Connection pool via `MysqlPool`

Use `oxisql_pool::mysql::MysqlPool` (or `new_mysql_pool`) for pooled access backed by a custom `deadpool` Manager over raw `mysql_async::Conn` objects. See [oxisql-pool](../oxisql-pool/README.md).

## Test Status

As of 2026-05-30: **90 tests passing, 15 skipped** (live-MySQL tests are `#[ignore]`d and skipped when no server is available).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
