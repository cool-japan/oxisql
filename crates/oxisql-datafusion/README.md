# oxisql-datafusion

> Apache DataFusion 53.x bridge exposing OxiSQL-backed tables to OLAP SQL, with live streaming and filter/projection/limit/sort pushdown.

[![Crates.io](https://img.shields.io/crates/v/oxisql-datafusion.svg)](https://crates.io/crates/oxisql-datafusion)
[![Docs.rs](https://docs.rs/oxisql-datafusion/badge.svg)](https://docs.rs/oxisql-datafusion)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![MSRV](https://img.shields.io/badge/MSRV-1.89-blue.svg)](https://www.rust-lang.org)

## What it is

`oxisql-datafusion` bridges the [OxiSQL](https://github.com/cool-japan/oxisql) workspace to [Apache DataFusion](https://datafusion.apache.org/) (53.x), so OLAP SQL — joins, aggregations, window functions — can be planned and executed by DataFusion against OxiSQL data.

It offers two `TableProvider` flavours: a **snapshot** provider that materialises a fixed set of rows as an Arrow `RecordBatch`, and a **live-streaming** provider that drives a real `oxisql_core::Connection` at scan time and yields batches incrementally. Both push supported predicates down to SQL (filter, projection, limit, sort), and multiple OxiSQL tables can be registered in one catalog for cross-table joins. Everything is Pure Rust.

- **Status:** Alpha.
- **Edition:** 2021 · **MSRV:** 1.89 · **License:** Apache-2.0.

## Installation

```toml
[dependencies]
oxisql-datafusion = "0.3.3"

# Optional features:
# oxisql-datafusion = { version = "0.3.3", features = ["parse"] }     # SQL → DataFusion plan bridge
# oxisql-datafusion = { version = "0.3.3", features = ["columnar"] }  # Parquet table provider
```

## Quick start

Register a live embedded connection and run OLAP SQL through DataFusion:

```rust,ignore
use std::sync::Arc;
use arrow::datatypes::{DataType, Field, Schema};
use oxisql_core::Connection;
use oxisql_datafusion::OxiSqlContext;
use oxisql_embedded::EmbeddedConnection;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Populate an OxiSQL-backed table.
    let conn = Arc::new(EmbeddedConnection::open_memory()?);
    conn.execute("CREATE TABLE sales (region TEXT, amount INTEGER)", &[]).await?;
    conn.execute("INSERT INTO sales VALUES ('EU', 100), ('EU', 50), ('US', 75)", &[]).await?;

    let schema = Arc::new(Schema::new(vec![
        Field::new("region", DataType::Utf8,  false),
        Field::new("amount", DataType::Int64, false),
    ]));

    // 2. Register it as a live-streaming DataFusion table.
    let ctx = OxiSqlContext::new();
    ctx.register_table("sales", conn, schema)?;

    // 3. Let DataFusion plan and execute the OLAP query.
    let batches = ctx
        .execute_sql("SELECT region, SUM(amount) AS total FROM sales GROUP BY region")
        .await?;

    println!("{} record batch(es)", batches.len());
    Ok(())
}
```

Or build a static snapshot provider directly from rows:

```rust
use std::sync::Arc;
use arrow::datatypes::{DataType, Field, Schema};
use oxisql_core::{Row, Value};
use oxisql_datafusion::OxiSqlTableProvider;

let schema = Arc::new(Schema::new(vec![
    Field::new("id",    DataType::Int64,   false),
    Field::new("name",  DataType::Utf8,    false),
    Field::new("score", DataType::Float64, false),
]));

let rows = vec![Row::new(
    vec!["id".into(), "name".into(), "score".into()],
    vec![Value::I64(1), Value::Text("Alice".into()), Value::F64(95.5)],
)];

// Optionally split into 4 contiguous partitions for parallel scans.
let provider = OxiSqlTableProvider::from_rows(rows, schema)
    .with_range_partition("id", 4);
```

### Via the `oxisql` facade

```rust,ignore
// `datafusion://` (or its alias `memory://`) yields an empty OxiSqlContext.
let ctx = oxisql::connect_datafusion("datafusion://").await?;
```

## Key API

| Item | Description |
|------|-------------|
| `OxiSqlTableProvider::from_rows(rows, schema)` | Snapshot provider from a pre-collected row set + Arrow schema. |
| `OxiSqlTableProvider::from_connection(conn, table, schema)` *(async)* | Snapshot built by running `SELECT * FROM {table}`. |
| `provider.refresh(conn, table)` *(async)* | Re-query the connection to replace the snapshot. |
| `provider.with_range_partition(key_col, n)` | Sort by `key_col` and split into `n` contiguous partitions. |
| `provider.with_hash_partition(key_col, n)` | Hash-bucket rows by `key_col` into `n` partitions (no ordering guarantee within a bucket). |
| `provider.with_auto_partition(n_parallel, target_batch_size)` | Auto-size range partitions from row count vs. `target_batch_size`, capped at `n_parallel`; no-op if partitioning would not help. Available on both `OxiSqlTableProvider` and `OxiSqlStreamProvider`. |
| `OxiSqlStreamProvider::new(conn, table, schema)` | Live-streaming provider driving a real `Connection` at scan time. |
| `provider.with_sort(vec![(col, SortOrder::Asc \| SortOrder::Desc)])` | Push an `ORDER BY` into the backing query. |
| `OxiSqlContext::new()` / `from_session_context(ctx)` | DataFusion `SessionContext` wrapper, fresh or wrapping an existing context. |
| `ctx.register_table(name, conn, schema)` | Register a live connection (uses `OxiSqlStreamProvider`). |
| `ctx.register_snapshot(name, rows, schema)` | Register a static snapshot (uses `OxiSqlTableProvider`). |
| `ctx.register_view(name, sql)` *(async)* | Register a SQL view (`CREATE VIEW name AS sql`) queryable by later statements. |
| `ctx.deregister_table(name)` | Remove a previously registered table; returns whether one existed. |
| `ctx.execute_sql(sql)` *(async)* | Run SQL, return `Vec<RecordBatch>`. |
| `ctx.sql(sql)` *(async)* | Run SQL, return a DataFusion `DataFrame`. |
| `ctx.explain_plan(sql)` *(async)* | Logical + physical plan as a formatted string. |
| `ctx.register_scalar_function(...)` / `ctx.register_aggregate_function(...)` | Register a scalar UDF / aggregate UDAF. |
| `ctx.register_parquet(name, path)` *(feature `columnar`)* | Register a Parquet file as a queryable table (schema inferred from file metadata). |
| `ctx.session_context()` / `into_session_context()` | Borrow / take the underlying `SessionContext`. |
| `register_oxisql_table(ctx, name, conn, schema)` | Free fn — one-line registration of any `Connection`-backed table. |
| `register_embedded_table(ctx, name, conn, schema)` *(async)* | Free fn — convenience for `EmbeddedConnection`; also available as the `ctx.register_embedded_table(conn, table_name)` method. |
| `OxiSqlFusionError` | Error type: `OxiSql(String)`, `Arrow(ArrowError)`, `DataFusion(DataFusionError)`, `SchemaMismatch`, `UnsupportedType`. |

## Feature flags

| Feature | Effect |
|---------|--------|
| *(default)* | `OxiSqlTableProvider`, `OxiSqlStreamProvider`, `OxiSqlContext`, pushdown, catalog registration, type mapping. |
| `parse` | `plan_bridge` module: `sql_to_datafusion_plan` / `to_datafusion_plan` convert an `oxisql_parse::LogicalPlan` into a DataFusion `LogicalPlan` (Scan/Limit/Empty structurally, the rest via SQL round-trip). |
| `columnar` | `ParquetTableProvider` — scan Parquet files as DataFusion tables via `oxistore-columnar`. |
| `mysql` / `postgres` / `sqlite` | Wire up the corresponding OxiSQL backend (primarily for cross-backend testing). |

## Pushdown summary

Predicates and shaping operators the providers translate into the backing SQL query (the rest is handled by DataFusion):

| Operator | Pushed down as |
|----------|----------------|
| `=`, `<>`, `<`, `<=`, `>`, `>=` | SQL `WHERE` comparisons |
| `IS NULL` / `IS NOT NULL` | SQL `WHERE` null checks |
| Projection | `SELECT` of only the requested columns (instead of `SELECT *`) |
| Limit | `LIMIT N` |
| Sort | `ORDER BY` (live provider, via `with_sort`) |

Filters on the snapshot provider are reported as `Inexact`, so DataFusion still applies its own post-filter for correctness. The full `Value` ↔ Arrow type mapping covers all 13 `Value` variants (Null, Bool, I64, F64, Text, Blob, Timestamp, Date, Time, Uuid, Json, Decimal, Array). Range-based partitioning enables parallel scans.

## Test coverage

**67 tests pass** with default features and **87 tests pass** with `--all-features` (4 ignored: 2 live-server tests for the optional `postgres` backend, 2 for the optional `mysql` backend). The extra all-features tests are the `parse` plan-bridge suite, the `columnar` Parquet-provider suite, and one `sqlite` streaming test — all individually feature-gated. This crate is part of a workspace where **2,157 tests pass** in total (**2,651** with `--all-features`).

## Part of the OxiSQL workspace

`oxisql-datafusion` is one of 17 Pure-Rust crates in the OxiSQL project. See the [workspace README](../../README.md) for the facade, the embedded engine that backs most providers here, connection pooling, and the wire-protocol backends.

## License

Apache-2.0 © 2024–2026 COOLJAPAN OU (Team Kitasan).
