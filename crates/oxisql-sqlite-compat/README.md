# oxisql-sqlite-compat — Pure-Rust SQLite-compatible backend for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-sqlite-compat.svg)](https://crates.io/crates/oxisql-sqlite-compat)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Pure-Rust SQLite-compatible backend implementing `oxisql_core::Connection` on top of
the C-free **`oxisqlite`** engine (a COOLJAPAN fork of limbo 0.0.22). **No
`libsqlite3`, no C/C++.**

**Status: Alpha** (but `ROLLBACK`, and `SAVEPOINT` / `RELEASE` / `ROLLBACK TO
SAVEPOINT` issued as SQL, are now fully supported — see below).

## What it is

`oxisql-sqlite-compat` wraps the **`oxisqlite`** engine — a C-free fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22 with every C/C++ dependency
stripped out — and implements `oxisql_core::Connection`, so any OxiSQL consumer can
use SQLite without linking `libsqlite3` or any C/C++ code. `oxisqlite` is a workspace
member that OxiSQL owns and maintains; `limbo` survives only as historical fork
lineage, not as a live dependency.

The whole stack is **100% Pure Rust** and builds under `#![forbid(unsafe_code)]` at
the compat layer.

## Installation (0.3.3)

```toml
[dependencies]
oxisql-sqlite-compat = "0.3.3"
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

### Open a database from an in-memory image (`open_from_bytes`, new in 0.3.3)

`SqliteConnection::open_from_bytes(bytes: &[u8])` opens a database directly from
a byte buffer — e.g. `include_bytes!`, a `VACUUM INTO` target, or
`sqlite3_serialize()` output — with **no temporary file**, so it works on WASI,
in the browser, and on read-only filesystems. It mirrors SQLite's
`sqlite3_deserialize()`.

```rust
# async fn demo(image: &[u8]) -> Result<(), oxisql_core::OxiSqlError> {
use oxisql_core::Connection;
use oxisql_sqlite_compat::SqliteConnection;

// `image` is a complete SQLite database file loaded into memory —
// e.g. `include_bytes!("app.db")`, a `VACUUM INTO` target, or
// `sqlite3_serialize()` output.
let conn = SqliteConnection::open_from_bytes(image).await?;
let rows = conn.query("SELECT count(*) FROM sqlite_master", &[]).await?;
# let _ = rows;
# Ok(())
# }
```

A synchronous counterpart, `SqliteConnectionBlocking::open_from_bytes(bytes: &[u8])`
(behind the `blocking` feature), drives the same call to completion on a fresh
`current_thread` Tokio runtime. Malformed input (too short, wrong magic header, or
an invalid page size) returns a typed `OxiSqlError` and never panics. Any valid
on-disk page size (512 through 65536) is accepted for reading; the 65536 page
size remains write-limited by a pre-existing engine `u16` usable-space constraint.

## Key API

| Item | Description |
|------|-------------|
| `SqliteConnection::open_memory()` | Create an in-memory SQLite database |
| `SqliteConnection::open(path)` | Open or create a file-backed SQLite database |
| `SqliteConnection::open_from_bytes(bytes)` | Open directly from an in-memory database image — no temporary file; mirrors `sqlite3_deserialize()`. Sync counterpart: `SqliteConnectionBlocking::open_from_bytes` (`blocking` feature) |
| `SqliteConnection` | Implements `oxisql_core::Connection` (`execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream`) |
| `SqliteTransaction` | Implements `Transaction`; `commit()` persists, **`rollback()` discards** all pending changes (also fires `ROLLBACK` on drop as a safety net) |
| `SqlitePrepared` | Implements `PreparedStatement` |
| `SqliteCompatError` | Wraps `oxisqlite` errors and maps them to `OxiSqlError` variants |
| `SqliteConnectionBlocking` (`blocking` feature) | Synchronous counterpart to `SqliteConnection` — each method drives the async API to completion via a fresh `current_thread` Tokio runtime, for non-async call sites |

### Type mapping

| SQLite affinity | OxiSQL `Value` variant |
|-----------------|------------------------|
| `INTEGER` | `Value::I64` |
| `REAL` | `Value::F64` |
| `TEXT` | `Value::Text` |
| `BLOB` | `Value::Blob` |
| `NULL` | `Value::Null` |

SQLite has no native `DATE` / `TIMESTAMP` / `TIME` / `UUID` storage class, but when a
column's **declared** SQL type names one of them, query results are lifted into the
matching richer `Value` variant instead of the generic mapping above (the match is
case-insensitive and prefix-based, so e.g. `"TIMESTAMP WITH TZ"` still triggers
`TIMESTAMP` handling):

| Declared column type | Storage | Produced `Value` |
|-----------------------|----------------|--------------------------|
| `DATE` (not `DATETIME`) | Text / Integer | `Value::Date` (days since epoch) |
| `DATETIME` / `TIMESTAMP` | Text / Integer | `Value::Timestamp` (µs since epoch) |
| `TIME` (not `TIMESTAMP`) | Text / Integer | `Value::Time` (µs since midnight) |
| `UUID` | Text / 16-byte `BLOB` | `Value::Uuid` (`u128`) |

A column with no declared type — or declared-type text that fails to parse — falls
back to the generic `Value::Text` / `Value::I64` / `Value::Blob` mapping rather than
erroring.

### Positional & named parameters

OxiSQL uses `$1`, `$2`, … placeholders; the engine accepts `?`. The crate performs a
**quote-aware `$N → ?`** rewrite before each statement is prepared, preserving string
literal content. Named parameters (`:name` / `$name` / `@name`) are handled at the
`oxisql-core` layer (via the `execute_named` / `query_named` default trait methods),
which rewrite them to positional `?` before the statement reaches the engine.

### Schema introspection

- `tables()` and `columns(table)` query `sqlite_master` + `PRAGMA table_info`.
- `indexes(table)` is derived by **parsing the `sqlite_master` DDL text** — no
  engine-specific metadata API is required, so introspection works even for
  databases created outside OxiSQL. (`PRAGMA index_list` / `PRAGMA index_info`
  are not yet implemented in the `oxisqlite` engine.)
- `foreign_keys(table)` uses the engine's native `PRAGMA foreign_key_list`.

### Affected-row counts

The engine's `execute()` returns a status code rather than a row count, so the
compat layer reads it back via `conn.changes()` after each DML statement — a
native, synchronous accessor call (not a `SELECT changes()` SQL round-trip).
`Statement::reset()` zeroes `Program::n_change` before each cached-statement
reuse, so this is accurate even when the statement cache serves the call.

### Statement cache

An LRU statement cache (128 slots, keyed by the rewritten SQL) is **active** for
every DML/DDL statement. `Statement::reset()` now zeroes `Program::n_change` in
`oxisqlite-core`, so a cache hit — reusing an already-compiled `limbo::Statement` via
`stmt.execute()` — reports a correct per-execution `changes()` count instead of the
inflated one that motivated the original fallback. If a cached statement was compiled
before a schema change (`ALTER`, `CREATE INDEX`, other DDL, …), the engine's
transaction-cookie check surfaces `SchemaChanged` on the first `step()`; the compat
layer catches that, discards the stale compiled program, re-prepares against the
refreshed schema, and retries exactly once — replacing the old keyword-prefix
`is_ddl` heuristic.

## Feature flags

| Feature | Effect |
|---------|--------|
| `index_experimental` | `CREATE INDEX` support, forwarded to `oxisqlite-core`'s experimental index path (enabled by default on the engine dependency) |
| `blocking` | Synchronous wrappers around `SqliteConnection` (`SqliteConnectionBlocking`, `SqliteBlockingTransaction`, `SqliteBlockingPrepared`) that drive the async API to completion via a freshly-built `current_thread` Tokio runtime per call — for integrating into non-async code paths |

## Known limitations

These are OxiSQL-owned `oxisqlite` engine roadmap items, not upstream blockers — we
maintain the engine ourselves.

| Limitation | Detail |
|------------|--------|
| **`Transaction::savepoint()` trait methods** | `SqliteTransaction` does not override `oxisql_core::Transaction`'s `savepoint` / `release_savepoint` / `rollback_to_savepoint` methods, so calling those specific Rust API methods returns the default `OxiSqlError::Other("savepoints are not supported by this backend")`. Issuing the equivalent SQL text directly — `execute("SAVEPOINT s1")` / `"RELEASE s1"` / `"ROLLBACK TO s1"` — works correctly, including nested savepoints and autocommit-mode `RELEASE`; see `tests/savepoint.rs`. |
| **Index metadata** | `indexes(table)` is derived by parsing `sqlite_master` DDL text; `PRAGMA index_list` / `PRAGMA index_info` are not yet implemented in the `oxisqlite` engine. |
| **Date/time/UUID without a declared column type** | A column with no declared SQL type (or declared-type text that fails to parse) still returns the generic `Value::Text` / `Value::I64` / `Value::Blob` mapping — see [Type mapping](#type-mapping) above for the declared-type-aware richer mapping. |

## Test coverage

**85 tests pass, 0 ignored** with default features (`cargo nextest run`); **94
tests pass, 0 ignored** with every feature enabled (`cargo nextest run
--all-features`) — the `blocking` feature adds 9 tests: the pre-existing 5 in
`tests/blocking.rs` plus 4 new blocking `open_from_bytes` cases in
`tests/open_from_bytes_blocking.rs`. `test_foreign_keys_basic` — formerly this
crate's one ignored test — now passes: `foreign_keys()` is backed by the engine's
native `PRAGMA foreign_key_list` rather than `sqlite_master` DDL parsing.
`tests/open_from_bytes.rs` contributes **5 new tests** (not feature-gated) for the
0.3.3 `open_from_bytes` entry point: a valid-image round trip that matches a
file-backed connection's results, write-after-open, and three malformed-input
error cases (empty, garbage, and truncated buffers). Among the other passing
tests, `tests/rollback.rs` contributes **5 ROLLBACK tests** (discard-on-rollback,
persist-on-commit, multi-row rollback, post-rollback reuse, and the
bare-`ROLLBACK`-without-transaction error path) and `tests/savepoint.rs` contributes
**8 SAVEPOINT tests** (rollback-to, release, nested savepoints, autocommit-mode
`RELEASE`, case-insensitive names, and interaction with a full `ROLLBACK`).

## Connection pool via `SqliteCompatPool`

Use `oxisql_pool::sqlite_compat::SqliteCompatPool` (also aliased
`oxisql_pool::sqlite::SqlitePool`) for pooled access. See
[`oxisql-pool`](../oxisql-pool/README.md).

## See also

This crate is one of a 17-crate Pure-Rust workspace. See the
[workspace README](../../README.md).

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
