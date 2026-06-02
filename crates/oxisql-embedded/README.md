# oxisql-embedded — GlueSQL-backed embedded SQL engine for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-embedded.svg)](https://crates.io/crates/oxisql-embedded)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-embedded` provides `EmbeddedConnection`, which implements `oxisql_core::Connection` over a GlueSQL `MemoryStorage` instance. It is a zero-external-service, pure-Rust SQL engine suitable for testing, embedded applications, and in-process analytics.

## Installation

```toml
[dependencies]
oxisql-embedded = "0.1.0"

# Optional persistent backends:
# oxisql-embedded = { version = "0.1.0", features = ["fjall-storage"] }
# oxisql-embedded = { version = "0.1.0", features = ["redb-storage"] }
# oxisql-embedded = { version = "0.1.0", features = ["sled-storage"] }
```

## Status

244 tests passing. Schema introspection (`tables()`, `columns()`, `indexes()`, `foreign_keys()`) is fully operational via the GlueSQL catalog.

## Quick Start

```rust
use oxisql_embedded::EmbeddedConnection;
use oxisql_core::Connection;

#[tokio::main]
async fn main() -> Result<(), oxisql_core::OxiSqlError> {
    let conn = EmbeddedConnection::open_memory()?;

    conn.execute(
        "CREATE TABLE users (id INTEGER, name TEXT)",
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

## API Overview

### Constructors

| Method | Description |
|--------|-------------|
| `EmbeddedConnection::open_memory()` | Create a new in-memory database (destroyed on drop) |
| `EmbeddedConnection::open_file(path)` | Open a file-backed database (requires `sled-storage` or `fjall-storage` feature; returns `UnsupportedUri` otherwise) |
| `EmbeddedConnection::from_glue(glue)` | Wrap an existing `Glue<MemoryStorage>` instance |

All constructors return `Result<EmbeddedConnection, OxiSqlError>` (synchronous, not async).

### SQL import / export

```rust
// Export all tables as a SQL dump string
let dump: String = conn.export_as_sql().await?;

// Import a SQL dump string (executes all statements via execute_batch)
conn.import_from_sql(&dump).await?;
```

### User-defined functions (UDFs)

Scalar UDFs are host-side functions callable through the Rust API (not SQL-level):

```rust
// Register a scalar UDF
conn.register_udf("double", |args| {
    if let Some(oxisql_core::Value::I64(n)) = args.first() {
        oxisql_core::Value::I64(n * 2)
    } else {
        oxisql_core::Value::Null
    }
});

// Call a registered UDF
let result = conn.call_udf("double", vec![oxisql_core::Value::I64(21)])?;
// result == Value::I64(42)
```

### Aggregate UDFs

Aggregate UDFs follow the `init → step* → finalize` pattern and are applied programmatically:

```rust
use oxisql_embedded::AggregateUdf;
use oxisql_core::Value;

conn.register_aggregate("sum_ints", AggregateUdf {
    init: Box::new(|| Value::I64(0)),
    step: Box::new(|acc, val| {
        if let (Value::I64(a), Value::I64(b)) = (acc, val) { Value::I64(a + b) }
        else { Value::Null }
    }),
    finalize: Box::new(|acc| acc),
});

let rows = conn.query("SELECT n FROM nums", &[]).await?;
let values: Vec<Value> = rows.into_iter()
    .flat_map(|r| r.into_values())
    .collect();
let total = conn.apply_aggregate("sum_ints", values)?;
```

### Connection pool via `EmbeddedPool`

Use `oxisql_pool::embedded::EmbeddedPool` for pooled access to an `EmbeddedConnection`. See [oxisql-pool](../oxisql-pool/README.md).

### Schema Introspection

`EmbeddedConnection` fully implements the `Connection` trait's introspection methods via the GlueSQL catalog:

| Method | Description |
|--------|-------------|
| `conn.tables().await` | Returns all table names via `storage.fetch_all_schemas()` |
| `conn.columns(table).await` | Returns `ColumnInfo` for each column; maps all GlueSQL types to `OxiSqlType` |
| `conn.indexes(table).await` | Merges GlueSQL catalog indexes (`SchemaIndex`) with `IndexRegistry` entries created by host-intercepted `CREATE INDEX` statements |
| `conn.foreign_keys(table).await` | Returns `ForeignKeyInfo` mapped from GlueSQL's `ForeignKey` struct |

`indexes()` combines two sources because GlueSQL `MemoryStorage` handles `CREATE INDEX` internally but the crate also maintains an `IndexRegistry` for B-tree indexes created via the host API. Both sources are merged and deduplicated by index name.

## Parameter Binding

GlueSQL does not natively support `$1`-style positional parameters. This crate implements safe client-side binding:

- **AST-level substitution** (primary path): parses SQL into a sqlparser AST, replaces `Expr::Value(Placeholder("$N"))` nodes with proper literal expressions, then re-serialises. Parameters inside string literals are never substituted.
- **String-scan fallback**: used when AST parsing fails (GlueSQL-specific syntax). Handles `$10` vs `$1` boundaries and `$$` escape sequences correctly.

Helper functions are exported for direct use:

```rust
use oxisql_embedded::{bind_params, bind_params_string, escape_sql_value};
use oxisql_core::Value;

let sql = bind_params(
    "INSERT INTO t VALUES ($1, $2)",
    &[Value::I64(1), Value::Text("hello".into())],
)?;
```

## Persistent Backends (feature-gated)

| Feature | Type | Storage |
|---------|------|---------|
| `fjall-storage` | `FjallGlueStorage` | fjall LSM-tree, Pure Rust |
| `redb-storage` | `RedbGlueStorage` | redb B-tree, Pure Rust |
| `sled-storage` | (sled) | sled embedded DB |

These implement GlueSQL's `Store` trait and can be passed to `EmbeddedConnection::from_glue(Glue::new(storage))`. File-backed connections can also be opened via `EmbeddedConnection::open_file(path)` when the appropriate feature is enabled.

## Full-Text Search (FTS)

An `FtsIndex` is embedded in every `EmbeddedConnection` for programmatic full-text indexing and querying at the host layer. Access via `conn.fts_index()`.

## Virtual Tables

```rust
use oxisql_embedded::VirtualTableRegistry;
```

`VirtualTableFn` and `VirtualTableRegistry` allow registering in-process virtual tables that are scanned at query time.

## B-Tree Index

`BTreeIndex`, `IndexKey`, and `IndexRegistry` are available for host-side ordered-index access outside GlueSQL's query engine.

## Savepoints

`savepoint()`, `rollback_to_savepoint()`, and `release_savepoint()` are accepted but are **no-ops** — GlueSQL `MemoryStorage` does not support nested transactions.

## GlueSQL SQL Dialect Notes

| Feature | Status |
|---------|--------|
| `ALTER TABLE ADD/DROP COLUMN` | Not supported; recreate the table |
| Multi-row `VALUES` in INSERT | Use individual INSERT statements |
| Window functions | Not supported in MemoryStorage |
| `INFORMATION_SCHEMA` | Not available |
| `BEGIN` / `COMMIT` / `ROLLBACK` | Syntactically accepted; no MVCC |
| `$1`-style parameters | Supported via host-side binding |

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
