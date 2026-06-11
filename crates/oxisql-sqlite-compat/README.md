# oxisql-sqlite-compat — Pure-Rust SQLite-compatible backend for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-sqlite-compat.svg)](https://crates.io/crates/oxisql-sqlite-compat)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Pure-Rust SQLite-compatible backend implementing `oxisql_core::Connection` on top of
the C-free **`oxisqlite`** engine (a COOLJAPAN fork of limbo 0.0.22). **No
`libsqlite3`, no C/C++.**

**Status: Alpha** (but `ROLLBACK` is now fully supported — see below).

## What it is

`oxisql-sqlite-compat` wraps the **`oxisqlite`** engine — a C-free fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22 with every C/C++ dependency
stripped out — and implements `oxisql_core::Connection`, so any OxiSQL consumer can
use SQLite without linking `libsqlite3` or any C/C++ code. `oxisqlite` is a workspace
member that OxiSQL owns and maintains; `limbo` survives only as historical fork
lineage, not as a live dependency.

The whole stack is **100% Pure Rust** and builds under `#![forbid(unsafe_code)]` at
the compat layer.

## Installation (0.1.2)

```toml
[dependencies]
oxisql-sqlite-compat = "0.1.2"
```

- MSRV: **1.89** · edition **2021** · `#![forbid(unsafe_code)]`

## Quick start

```rust
use oxisql_sqlite_compat::SqliteConnection;
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), oxisql_core::OxiSqlError> {
    // In-memory database (destroyed when the connection is dropped).
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

### Transactions with working ROLLBACK

`ROLLBACK` is **fully supported as of 0.1.2** — a `BEGIN`/`INSERT`/`ROLLBACK`
sequence discards the uncommitted rows, and the connection stays usable afterwards:

```rust
use oxisql_sqlite_compat::SqliteConnection;
use oxisql_core::{Connection, Value};

#[tokio::main]
async fn main() -> Result<(), oxisql_core::OxiSqlError> {
    let conn = SqliteConnection::open_memory().await?;
    conn.execute("CREATE TABLE t (id INTEGER)", &[]).await?;

    // BEGIN; INSERT; ROLLBACK — the row is discarded.
    let mut txn = conn.transaction().await?;
    txn.execute("INSERT INTO t VALUES (1)", &[]).await?;
    txn.rollback().await?;                       // ← discards all pending changes

    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await?;
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(0))); // 0 rows after rollback

    // COMMIT still persists, as expected.
    let mut txn = conn.transaction().await?;
    txn.execute("INSERT INTO t VALUES (42)", &[]).await?;
    txn.commit().await?;
    let rows = conn.query("SELECT COUNT(*) FROM t", &[]).await?;
    assert_eq!(rows[0].get_by_index(0), Some(&Value::I64(1)));
    Ok(())
}
```

### File-backed database

```rust
# async fn demo() -> Result<(), oxisql_core::OxiSqlError> {
use oxisql_sqlite_compat::SqliteConnection;

let conn = SqliteConnection::open("/path/to/mydb.sqlite3").await?;
# Ok(())
# }
```

## Key API

| Item | Description |
|------|-------------|
| `SqliteConnection::open_memory()` | Create an in-memory SQLite database |
| `SqliteConnection::open(path)` | Open or create a file-backed SQLite database |
| `SqliteConnection` | Implements `oxisql_core::Connection` (`execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream`) |
| `SqliteTransaction` | Implements `Transaction`; `commit()` persists, **`rollback()` discards** all pending changes (also fires `ROLLBACK` on drop as a safety net) |
| `SqlitePrepared` | Implements `PreparedStatement` |
| `SqliteCompatError` | Wraps `oxisqlite` errors and maps them to `OxiSqlError` variants |

### Type mapping

| SQLite affinity | OxiSQL `Value` variant |
|-----------------|------------------------|
| `INTEGER` | `Value::I64` |
| `REAL` | `Value::F64` |
| `TEXT` | `Value::Text` |
| `BLOB` | `Value::Blob` |
| `NULL` | `Value::Null` |

Date/time and UUID values have no native engine type and are stored as `TEXT`
(ISO strings / UUID text) or `INTEGER` (e.g. epoch microseconds), surfacing as
`Value::Text` / `Value::I64` respectively. Richer type mapping is on the roadmap.

### Positional & named parameters

OxiSQL uses `$1`, `$2`, … placeholders; the engine accepts `?`. The crate performs a
**quote-aware `$N → ?`** rewrite before each statement is prepared, preserving string
literal content. Named parameters (`:name` / `$name` / `@name`) are handled at the
`oxisql-core` layer (via the `execute_named` / `query_named` default trait methods),
which rewrite them to positional `?` before the statement reaches the engine.

### Schema introspection

- `tables()` and `columns(table)` query `sqlite_master` + `PRAGMA table_info`.
- `indexes(table)` and `foreign_keys(table)` are derived by **parsing the
  `sqlite_master` DDL text** — no engine-specific metadata API is required, so
  introspection works even for databases created outside OxiSQL.

### Affected-row counts

The engine's `execute()` returns a status code rather than a row count, so the compat
layer issues a `SELECT changes()` after each DML statement to report affected rows
(one extra round-trip per write).

### Statement cache

An LRU statement cache (128 slots, keyed by the rewritten SQL) is wired in at the
compat layer. Execution currently **falls back to a fresh `conn.execute()` per call**
because of an `oxisqlite` `Statement::reset()` quirk: `reset()` clears `ProgramState`
but not `Program::n_change`, so a re-used cached statement would report an inflated
`changes()` count. The cache infrastructure is in place and will activate
automatically once that reset is fixed in the engine — this is OxiSQL's own roadmap
item (we own `oxisqlite`).

## Feature flags

| Feature | Effect |
|---------|--------|
| `index_experimental` | `CREATE INDEX` support, forwarded to `oxisqlite-core`'s experimental index path (enabled by default on the engine dependency) |

## Known limitations

These are OxiSQL-owned `oxisqlite` engine roadmap items, not upstream blockers — we
maintain the engine ourselves.

| Limitation | Detail |
|------------|--------|
| **Savepoints** | `SAVEPOINT` / `RELEASE` / `ROLLBACK TO SAVEPOINT` return a clear "not supported yet" `OxiSqlError` rather than a raw parse error. Planned for a future `oxisqlite` release. |
| **Foreign-key metadata** | Derived by parsing `sqlite_master` DDL text; there is no `PRAGMA foreign_key_list` yet. (The engine also does not yet preserve FK DDL in `sqlite_master` in every case — see the one ignored test.) |
| **Statement-cache fallback** | Cache populated but bypassed pending the `Statement::reset()` / `Program::n_change` fix described above. |
| **Date/time/UUID** | Stored and returned as `TEXT` / `INTEGER`; no dedicated `Value` variants yet. |

## Test coverage

**61 tests pass**, **1 ignored**. The single ignored test
(`test_foreign_keys_basic`) is gated because `oxisqlite` 0.0.22 does not yet preserve
foreign-key DDL in `sqlite_master`; it is **not** a live-server gate. Among the
passing tests, `tests/rollback.rs` contributes **5 ROLLBACK tests** that verify
discard-on-rollback, persist-on-commit, multi-row rollback, post-rollback reuse, and
the bare-`ROLLBACK`-without-transaction error path.

## Connection pool via `SqliteCompatPool`

Use `oxisql_pool::sqlite_compat::SqliteCompatPool` (also aliased
`oxisql_pool::sqlite::SqlitePool`) for pooled access. See
[`oxisql-pool`](../oxisql-pool/README.md).

## See also

This crate is one of a 17-crate Pure-Rust workspace. See the
[workspace README](../../README.md).

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
