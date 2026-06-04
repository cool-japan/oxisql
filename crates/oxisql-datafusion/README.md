# oxisql-datafusion — Apache DataFusion integration for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-datafusion.svg)](https://crates.io/crates/oxisql-datafusion)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-datafusion` exposes OxiSQL-backed tables to [Apache DataFusion](https://datafusion.apache.org/) so that OLAP SQL queries can be planned and executed against OxiSQL data using the full DataFusion query engine.

## Installation

```toml
[dependencies]
oxisql-datafusion = "0.1.1"

# Optional features:
# oxisql-datafusion = { version = "0.1.1", features = ["columnar"] }  # Parquet support
# oxisql-datafusion = { version = "0.1.1", features = ["parse"] }     # Plan bridge
```

## Quick Start

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

let rows = vec![
    Row::new(
        vec!["id".into(), "name".into(), "score".into()],
        vec![Value::I64(1), Value::Text("Alice".into()), Value::F64(95.5)],
    ),
];

let provider = OxiSqlTableProvider::from_rows(rows, schema);
```

## API Overview

### `OxiSqlTableProvider`

A DataFusion `TableProvider` that serves a fixed snapshot of `oxisql_core::Row`s as a single Arrow `RecordBatch` partition.

| Method | Description |
|--------|-------------|
| `OxiSqlTableProvider::from_rows(rows, schema)` | Construct from a pre-collected row snapshot and Arrow schema |
| `OxiSqlTableProvider::from_connection(conn, table_name, schema)` | Execute `SELECT * FROM {table_name}` on `conn` to populate |
| `provider.refresh(conn, table_name)` | Re-query `conn` to replace the current snapshot |
| `provider.with_range_partition(key_col, n)` | Sort by `key_col` and split into `n` contiguous partitions for parallel scans |

Filter pushdown is supported for binary comparisons (`=`, `<>`, `<`, `<=`, `>`, `>=`) and `IS NULL` / `IS NOT NULL`. Filters are applied in-process; `Inexact` is reported so DataFusion still applies its own post-filter.

The provider is cheaply cloneable (`Arc` internally).

### `OxiSqlStreamProvider`

A live-streaming `TableProvider` that drives a real `oxisql_core::Connection` at scan time and yields batches incrementally.

| Method | Description |
|--------|-------------|
| `OxiSqlStreamProvider::new(conn, table_name, schema)` | Wrap a live connection |
| `provider.with_sort_order(order)` | Specify sort ordering for the stream |

`SortOrder` variants: `Ascending(col)`, `Descending(col)`.

### `OxiSqlContext`

A `DataFusion` `SessionContext` wrapper with convenience methods for registering OxiSQL-backed tables.

| Method | Description |
|--------|-------------|
| `OxiSqlContext::new()` | Create with default DataFusion settings |
| `OxiSqlContext::from_session_context(ctx)` | Wrap an existing `SessionContext` |
| `ctx.register_table(name, conn, schema)` | Register a live connection as a DataFusion table (uses `OxiSqlStreamProvider`) |
| `ctx.register_snapshot(name, rows, schema)` | Register a static row snapshot (uses `OxiSqlTableProvider`) |
| `ctx.execute_sql(sql)` | Execute SQL and return `Vec<RecordBatch>` |
| `ctx.to_dataframe(sql)` | Execute SQL and return a DataFusion `DataFrame` |
| `ctx.register_udf(name, func, arg_types, return_type)` | Register a scalar UDF |
| `ctx.register_udaf(name, factory, arg_types, return_type)` | Register an aggregate UDF |
| `ctx.explain(sql)` | Return the physical plan explanation string |
| `ctx.inner()` | Access the underlying `SessionContext` |

Free functions also available:

```rust
use oxisql_datafusion::{register_oxisql_table, register_embedded_table};

// Register any Connection-backed table
register_oxisql_table(&session_ctx, "users", conn_arc, schema)?;

// Convenience for EmbeddedConnection
register_embedded_table(&session_ctx, "products", embedded_conn_arc, schema)?;
```

### `OxiSqlFusionError`

Error type covering DataFusion and OxiSQL errors:

| Variant | Description |
|---------|-------------|
| `DataFusion(DataFusionError)` | DataFusion engine error |
| `OxiSql(String)` | OxiSQL backend error (string form) |
| `Arrow(ArrowError)` | Arrow conversion error |

## Features / Feature Flags

| Feature | Description |
|---------|-------------|
| (default) | `OxiSqlTableProvider`, `OxiSqlStreamProvider`, `OxiSqlContext` |
| `columnar` | `ParquetTableProvider` — scan Parquet files as DataFusion tables |
| `parse` | `plan_bridge` module — convert `oxisql_parse::LogicalPlan` to DataFusion `LogicalPlan` |

### `plan_bridge` (feature = `parse`)

```rust
use oxisql_datafusion::{sql_to_datafusion_plan, to_datafusion_plan};
use oxisql_parse::LogicalPlan;

// Convert an oxisql_parse LogicalPlan to a DataFusion LogicalPlan
let df_plan = to_datafusion_plan(oxisql_plan, &session_ctx)?;

// Parse SQL and produce a DataFusion plan in one step
let df_plan = sql_to_datafusion_plan("SELECT id FROM users WHERE id > 10", &session_ctx)?;
```

### `ParquetTableProvider` (feature = `columnar`)

```rust
use oxisql_datafusion::ParquetTableProvider;

let provider = ParquetTableProvider::open("/data/users.parquet").await?;
session_ctx.register_table("users", Arc::new(provider))?;
```

## Type Mapping

OxiSQL `Value` variants are converted to Arrow arrays via the `types` module:

| `Value` variant | Arrow type |
|-----------------|-----------|
| `Null` | null slot in the column array |
| `Bool` | `Boolean` |
| `I64` | `Int64` |
| `F64` | `Float64` |
| `Text` | `Utf8` |
| `Blob` | `Binary` |
| `Timestamp` | `Timestamp(Microsecond, UTC)` |
| `Date` | `Date32` |
| `Time` | `Time64(Microsecond)` |
| `Uuid` | `FixedSizeBinary(16)` |
| `Json` | `Utf8` |
| `Decimal` | `Utf8` |
| `Array` | `LargeList` |

## Test Status

As of 2026-05-30: **67 tests passing, 4 skipped**.

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
