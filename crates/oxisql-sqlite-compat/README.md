# oxisql-sqlite-compat — Pure-Rust SQLite-compatible backend for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-sqlite-compat.svg)](https://crates.io/crates/oxisql-sqlite-compat)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-sqlite-compat` wraps the [Limbo](https://github.com/tursodatabase/limbo) pure-Rust SQLite engine and implements `oxisql_core::Connection`. No `libsqlite3`, no C/C++ dependencies.

## Installation

```toml
[dependencies]
oxisql-sqlite-compat = "0.1.0"
```

## Quick Start

```rust
use oxisql_sqlite_compat::SqliteConnection;
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), oxisql_core::OxiSqlError> {
    // In-memory database (destroyed when the connection is dropped)
    let conn = SqliteConnection::open_memory().await?;

    conn.execute(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)",
        &[],
    ).await?;

    conn.execute(
        "INSERT INTO users VALUES ($1, $2)",
        &[&1i64, &"Alice"],
    ).await?;

    let rows = conn.query("SELECT id, name FROM users", &[]).await?;
    assert_eq!(rows.len(), 1);

    let id: i64 = rows[0].try_get("id")?;
    let name: String = rows[0].try_get("name")?;
    println!("{id}: {name}");
    Ok(())
}
```

## File-Backed Database

```rust
use oxisql_sqlite_compat::SqliteConnection;

let conn = SqliteConnection::open("/tmp/mydb.sqlite3").await?;
```

## API Overview

### `SqliteConnection`

| Method | Description |
|--------|-------------|
| `SqliteConnection::open_memory()` | Create an in-memory SQLite database |
| `SqliteConnection::open(path)` | Open or create a file-backed SQLite database |
| Implements `Connection` trait | `execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream` |

### Schema introspection

`foreign_keys(table)` and `indexes(table)` are implemented by parsing `sqlite_master` DDL text — no Limbo-specific metadata API is used. This means schema introspection works even for databases created outside OxiSQL.

`tables()` and `columns(table)` query `sqlite_master` and `PRAGMA table_info`.

### Type mapping

| SQLite affinity | OxiSQL `Value` variant |
|-----------------|------------------------|
| `INTEGER` | `Value::I64` |
| `REAL` | `Value::F64` |
| `TEXT` | `Value::Text` |
| `BLOB` | `Value::Blob` |
| `NULL` | `Value::Null` |

Limbo 0.0.22 does not expose native date/time/UUID types. Values stored as `INTEGER` microseconds or `TEXT` are returned as `Value::I64` / `Value::Text` respectively.

### Positional parameter rewrite

OxiSQL uses `$1`, `$2`, … placeholders. Limbo only supports `?`. This crate performs a quote-aware rewrite before each statement is prepared, converting `$N` to `?` while preserving string literal content.

### Error type

`SqliteCompatError` wraps Limbo errors and maps them to `OxiSqlError` variants.

## Known Limitations (Limbo 0.0.22)

| Limitation | Detail |
|------------|--------|
| **Affected-row count** | Limbo's `execute()` returns a status code, not a count. A `SELECT changes()` is issued after each DML statement (one extra round-trip per write). |
| **Named parameters** | `:name` / `$name` style named params are supported at the **oxisql-core layer** via `execute_named` / `query_named` default trait methods, which rewrite them to positional `?` before sending to Limbo. The limbo-level named param binding path remains `todo!()` but is bypassed entirely. |
| **Prepared-statement caching** | An LRU statement cache (128 slots, keyed by rewritten SQL) is in place at the oxisql layer. Execution currently falls back to a fresh `conn.execute()` per call due to a known `Statement::reset()` bug in limbo 0.0.22 (it does not clear `Program::n_change`, causing inflated `changes()` counts on reuse). The cache will activate automatically once limbo fixes this — no code changes needed on our side. |
| **ROLLBACK in transactions** | Limbo 0.0.22 does not implement `ROLLBACK`. `SqliteTransaction::rollback()` returns `OxiSqlError::Other("ROLLBACK is not supported by the limbo 0.0.22 engine; this transaction cannot be rolled back — upgrade to limbo 0.1+ when available")` instead of propagating a raw parse error. Feature remains upstream-blocked. |
| **Savepoints** | Not supported. `savepoint()` / `rollback_to_savepoint()` / `release_savepoint()` return `OxiSqlError::Other`. |

These limitations are expected to be resolved in Limbo 0.1+.

## Test coverage

54 tests pass; 3 are skipped (`#[ignore]`) pending upstream ROLLBACK support in limbo 0.1+.

## Connection pool via `SqliteCompatPool`

Use `oxisql_pool::sqlite_compat::SqliteCompatPool` (also aliased as `oxisql_pool::sqlite::SqlitePool`) for pooled access. See [oxisql-pool](../oxisql-pool/README.md).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
