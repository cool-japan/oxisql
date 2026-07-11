# oxisql-embedded

> In-memory SQL via GlueSQL with optional persistent backends (fjall LSM-tree, redb B-tree, sled key-value); full schema introspection and CSV/SQL import/export — all Pure Rust.

[![Crates.io](https://img.shields.io/crates/v/oxisql-embedded.svg)](https://crates.io/crates/oxisql-embedded)
[![Docs.rs](https://docs.rs/oxisql-embedded/badge.svg)](https://docs.rs/oxisql-embedded)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue.svg)](https://www.rust-lang.org)

## What it is

`oxisql-embedded` is the zero-external-service, **Pure Rust** backend of the [OxiSQL](https://github.com/cool-japan/oxisql) workspace. It wraps a [GlueSQL](https://github.com/gluesql/gluesql) engine behind `oxisql_core::Connection`, giving you an embeddable SQL database that needs no server, no driver, and no C/C++/Fortran dependency.

By default it runs entirely in memory (`MemoryStorage`, reset on drop). Three optional, feature-gated persistent backends — **fjall** (LSM-tree), **redb** (B-tree), and **sled** (key-value) — let the same API write to disk durably. On top of the SQL engine the crate adds full schema introspection, RFC 4180 CSV import/export, SQL dump import/export, host-side scalar/aggregate UDFs, virtual tables, full-text search, B-tree secondary indexes, and safe client-side parameter binding.

- **Status:** Stable.
- **Edition:** 2021 · **MSRV:** 1.89 · **License:** Apache-2.0.

## Installation

```toml
[dependencies]
oxisql-embedded = "0.1.2"

# Optional persistent backends (pick any subset):
# oxisql-embedded = { version = "0.1.2", features = ["fjall-storage"] }
# oxisql-embedded = { version = "0.1.2", features = ["redb-storage"] }
# oxisql-embedded = { version = "0.1.2", features = ["sled-storage"] }
```

## Quick start

```rust
use oxisql_embedded::EmbeddedConnection;
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), oxisql_core::OxiSqlError> {
    // Volatile in-memory database (reset on drop).
    let conn = EmbeddedConnection::open_memory()?;

    conn.execute("CREATE TABLE users (id INTEGER, name TEXT)", &[]).await?;

    // Positional parameters ($1, $2) are bound safely client-side.
    conn.execute(
        "INSERT INTO users VALUES ($1, $2)",
        &[&1i64, &"Alice"],
    ).await?;

    // Named parameters (:name / $name / @name) work on every backend
    // via the oxisql_core default methods.
    conn.execute_named(
        "INSERT INTO users VALUES (:id, :name)",
        &[(":id", &2i64), (":name", &"Bob")],
    ).await?;

    let rows = conn.query("SELECT id, name FROM users ORDER BY id", &[]).await?;
    assert_eq!(rows.len(), 2);

    let id: i64 = rows[0].try_get("id")?;
    let name: String = rows[0].try_get("name")?;
    println!("{id}: {name}");

    // Schema introspection straight from the GlueSQL catalog.
    for table in conn.tables().await? {
        println!("table: {}", table.name);
    }
    Ok(())
}
```

### Persistent backend in one line

```rust
use oxisql_embedded::RedbEmbeddedConnection; // requires `redb-storage`
use oxisql_core::Connection;

# async fn demo() -> Result<(), oxisql_core::OxiSqlError> {
let path = std::env::temp_dir().join("oxisql_demo.redb");
let conn = RedbEmbeddedConnection::open(&path)?;
conn.execute("CREATE TABLE kv (k TEXT, v TEXT)", &[]).await?;
// Data survives drop + reopen — ACID, order-preserving keys.
# Ok(())
# }
```

`FjallEmbeddedConnection::open(path)` and `SledEmbeddedConnection::open(path)` follow the same pattern behind their respective feature flags.

### Via the `oxisql` facade

The facade resolves a URI to the right backend, so application code stays backend-agnostic:

| URI | Backend |
|-----|---------|
| `memory://` | In-memory `MemoryStorage` (default) |
| `redb://path/to/file.db` | redb persistent (feature `redb`) |
| `fjall://path/to/dir` | fjall persistent (feature `fjall`) |
| `sled://path/to/dir` | sled persistent (feature `sled`) |

```rust,ignore
let conn = oxisql::connect("memory://").await?;
```

## Key API

| Item | Description |
|------|-------------|
| `EmbeddedConnection::open_memory()` | New volatile in-memory database (default backend). |
| `EmbeddedConnection::open_file(path)` | File-backed database when a persistent feature is enabled; returns `UnsupportedUri` otherwise. |
| `EmbeddedConnection::from_glue(glue)` | Wrap an existing GlueSQL `Glue` instance. |
| `RedbEmbeddedConnection::open(path)` | redb B-tree backend (feature `redb-storage`). |
| `FjallEmbeddedConnection::open(path)` | fjall LSM-tree backend (feature `fjall-storage`). |
| `SledEmbeddedConnection::open(path)` | sled key-value backend (feature `sled-storage`). |
| `EmbeddedTransaction` / `EmbeddedPrepared` | Transaction guard and prepared-statement handle. |
| `Connection` impl | `execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream` (plus `execute_named` / `query_named` defaults). |
| `import_csv(table, csv)` / `export_table_to_csv(table)` | Pure-Rust RFC 4180 CSV round-trip (`csv.rs`). |
| `import_from_sql(sql)` / `export_as_sql()` | SQL-dump import (via `execute_batch`) and export (via `fetch_all_schemas()` + `Schema::to_ddl()`). |
| `explain(sql)` | Pattern-based query plan string. |
| `UdfRegistry` — `register_udf` / `call_udf` | Host-side scalar functions (Rust API, not SQL level). |
| `AggregateUdf` — `register_aggregate` / `apply_aggregate` | `init → step* → finalize` aggregates. |
| `VirtualTableRegistry` | Register in-process virtual tables scanned at query time. |
| `FtsIndex` | Inverted-index full-text search (`MATCH` interception). |
| `BTreeIndex` / `IndexKey` / `IndexRegistry` | Host-side ordered secondary indexes. |
| `bind_params` / `bind_params_string` / `escape_sql_value` | Parameter-binding helpers exposed for direct use. |

### Parameter binding

GlueSQL has no native positional placeholders, so this crate binds them safely on the client:

- **AST-level substitution (primary):** the SQL is parsed with `sqlparser` (`params.rs`), `Placeholder("$N")` nodes are replaced with typed literal expressions, and the statement is re-serialised. Placeholders inside string literals are never touched.
- **String-scan fallback:** used when GlueSQL-specific syntax fails the generic parser; handles `$10` vs `$1` boundaries and `$$` escapes.
- **BLOB** values are emitted as `X'..'` hex literals.

Named placeholders (`:name`, `$name`, `@name`) are provided by `oxisql_core` default methods and therefore work identically across **all** OxiSQL backends.

## Feature flags

| Feature | Effect |
|---------|--------|
| *(default)* | In-memory `MemoryStorage` only — 100% Pure Rust, no extra deps. |
| `fjall-storage` | Enables `FjallEmbeddedConnection` / `FjallGlueStorage` (fjall LSM-tree). |
| `redb-storage` | Enables `RedbEmbeddedConnection` / `RedbGlueStorage` (redb B-tree). |
| `sled-storage` | Enables `SledEmbeddedConnection` / `SledGlueStorage` (sled key-value). |

## Storage backends

| Backend | Persistence | Notes | Feature |
|---------|-------------|-------|---------|
| `MemoryStorage` | Volatile — reset on drop | Default; no extra dependency | *(built in)* |
| fjall LSM-tree | Persistent | Journal/WAL crash safety, Pure Rust | `fjall-storage` |
| redb B-tree | Persistent | ACID, order-preserving keys, Pure Rust | `redb-storage` |
| sled key-value | Persistent | Embedded key-value store, Pure Rust | `sled-storage` |

## Test coverage

**278 tests pass** with `--all-features` (unit + integration; CSV, schema introspection, persistence round-trips, parameter binding, UDFs, FTS, virtual tables, and more). The crate is part of a workspace where **2,030 tests pass** in total.

## Part of the OxiSQL workspace

`oxisql-embedded` is one of 17 Pure-Rust crates in the OxiSQL project. See the [workspace README](../../README.md) for the facade, connection pooling, the DataFusion OLAP bridge, and the wire-protocol backends.

## License

Apache-2.0 © 2024–2026 COOLJAPAN OU (Team Kitasan).
