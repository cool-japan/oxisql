# oxisql-core — Core traits and types for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-core.svg)](https://crates.io/crates/oxisql-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0)

> Core async traits, the 14-variant `Value` enum, error types, and middleware shared by all OxiSQL backends.

## What it is

`oxisql-core` defines the public API surface that every OxiSQL backend must
implement. It contains no storage logic — concrete backends live in
`oxisql-embedded`, `oxisql-postgres`, `oxisql-mysql`, `oxisql-sqlite-compat`,
and `oxisql-datafusion`. The crate is Pure Rust and contains **zero
`unsafe`**; its default build has no dependencies beyond `async-trait`,
`futures`, `log`, and `tokio` — `chrono`, `time`, and `tracing` support are
all opt-in feature flags.

## Installation

```toml
[dependencies]
oxisql-core = "0.3.3"
```

MSRV 1.89 · edition 2021 · Apache-2.0.

## Quick start

```rust,no_run
use oxisql_core::{Row, Value};

let row = Row::new(
    vec!["id".into(), "name".into()],
    vec![Value::I64(42), Value::Text("Alice".into())],
);

let id: i64 = row.try_get("id")?;
let name: String = row.try_get("name")?;
assert_eq!(id, 42);
assert_eq!(name, "Alice");
# Ok::<(), oxisql_core::OxiSqlError>(())
```

### Named parameters

`execute_named` / `query_named` are **default methods** on `Connection`, so
every backend inherits them automatically. Placeholders may use `:name`,
`$name`, or `@name`; repeated use of the same name reuses the same positional
value.

```rust,no_run
use oxisql_core::{Connection, ToSqlValue};

async fn demo(conn: &dyn Connection) -> Result<(), oxisql_core::OxiSqlError> {
    conn.execute_named(
        "INSERT INTO users (id, name) VALUES (:id, :name)",
        &[("id", &42i64 as &dyn ToSqlValue), ("name", &"Alice" as &dyn ToSqlValue)],
    ).await?;

    let rows = conn.query_named(
        "SELECT * FROM users WHERE id = :id",
        &[("id", &42i64 as &dyn ToSqlValue)],
    ).await?;
    let _ = rows;
    Ok(())
}
```

## Key API

### `Connection` trait

`Send + Sync`, fully async via `async_trait`.

| Method | Description |
|--------|-------------|
| `execute(sql, params)` | Execute DML/DDL, return rows-affected count |
| `query(sql, params)` | Execute SELECT, return `Vec<Row>` |
| `execute_named(sql, params)` | DML/DDL with named parameters (default method) |
| `query_named(sql, params)` | SELECT with named parameters (default method) |
| `transaction()` | Begin a transaction → `Box<dyn Transaction>` |
| `execute_batch(sql)` | Execute multiple semicolon-separated statements |
| `ping()` | Lightweight connectivity check (`SELECT 1`) |
| `prepare(sql)` | Compile a statement for repeated execution |
| `tables()` | List all tables |
| `columns(table)` | List columns of a named table |
| `indexes(table)` | List indexes on a named table |
| `foreign_keys(table)` | List FK constraints on a named table |
| `query_stream(...)` | SELECT → `Pin<Box<dyn Stream<Item = ...>>>` |
| `last_warnings()` | Non-fatal warnings from the last `execute`/`query` (default method: empty `Vec`; MySQL populates it via `SHOW WARNINGS`) |

Positional parameters use `$1`, `$2`, … with `params: &[&dyn ToSqlValue]`.
Named-parameter methods take `&[(&str, &dyn ToSqlValue)]`.

### `Transaction` trait

Obtained via `Connection::transaction()`. Dropped without commit → implicit rollback.

| Method | Description |
|--------|-------------|
| `execute(sql, params)` | DML/DDL inside the transaction |
| `query(sql, params)` | SELECT inside the transaction |
| `commit()` | Commit all changes |
| `rollback()` | Roll back all changes |
| `savepoint(name)` | Create a named savepoint |
| `release_savepoint(name)` | Release (discard) a savepoint |
| `rollback_to_savepoint(name)` | Roll back to a named savepoint |

### `ConnectionPool` trait

Object-safe (`Box<dyn ConnectionPool>` is valid).

| Method | Description |
|--------|-------------|
| `get()` | Check out a `Box<dyn Connection + Send>` |
| `pool_size()` | Maximum pool capacity |
| `idle_count()` | Connections available for checkout |
| `active_count()` | Connections currently checked out |
| `health_check()` | Verify the pool is healthy |
| `close()` | Drain idle connections, prevent new checkouts |

### `Value` enum — 14 variants

| Variant | Rust type | SQL type |
|---------|-----------|----------|
| `Null` | — | NULL |
| `Bool(bool)` | `bool` | BOOLEAN |
| `I64(i64)` | `i64` | INTEGER / BIGINT |
| `F64(f64)` | `f64` | REAL / DOUBLE PRECISION |
| `Text(String)` | `String` | TEXT / VARCHAR |
| `Blob(Vec<u8>)` | `Vec<u8>` | BYTEA / BLOB |
| `Decimal(String)` | exact decimal as string | NUMERIC / DECIMAL |
| `Timestamp(i64)` | microseconds since epoch (UTC) | TIMESTAMP / TIMESTAMPTZ |
| `Date(i32)` | days since Unix epoch | DATE |
| `Time(i64)` | microseconds since midnight | TIME |
| `Uuid(u128)` | 128-bit UUID | UUID |
| `Json(String)` | UTF-8 JSON string | JSON / JSONB |
| `Array(Vec<Value>)` | ordered collection | e.g. INTEGER[] |
| `TypedArray { element_type, values }` | `ArrayElementType` + `Vec<Value>` | array with the nominal element type preserved, e.g. Postgres `int4[]` |

`Value` implements `Display`, `PartialOrd` (cross-type comparisons return
`None`), and `From<bool/i32/i64/f64/String/&str/Vec<u8>/Option<T>>`.

`ArrayElementType` (the `TypedArray` element tag, `#[non_exhaustive]`) —
`Bool`, `Int2`, `Int4`, `Int8`, `Float4`, `Float8`, `Text`, `Bytea`, `Date`,
`Time`, `Timestamp`, `TimestampTz`, `Uuid`, `Json`, `Jsonb`, `Decimal`.

### `Row` and `RowSet`

`Row` stores column names and values with an O(1) `HashMap` index for lookups.

| Method | Description |
|--------|-------------|
| `Row::new(columns, values)` | Construct from parallel vecs |
| `get(col)` | Look up by name → `Option<&Value>` |
| `get_by_index(i)` | Look up by zero-based index |
| `try_get::<T: FromValue>(col)` | Type-safe extraction by name |
| `try_get_by_index::<T>(i)` | Type-safe extraction by index |
| `column_count()` | Number of columns |
| `is_null(col)` | True if the column exists and is NULL |
| `into_values()` | Consume → `Vec<Value>` |
| `columns()` | Ordered column names |

`RowSet` wraps `Vec<Row>`: `len()`, `is_empty()`, `column_count()`,
`columns()`, `rows()`, `into_rows()`, `from_rows(Vec<Row>)`.

### `FromValue` / `ToSqlValue`

`FromValue` extracts typed values from `Value`, returning
`OxiSqlError::TypeMismatch` on a mismatch. Impls: `bool`, `i32` (range-checked),
`i64`, `f64` (accepts I64 coercion), `String` (accepts Text/Json/Decimal/Uuid),
`Vec<u8>`, `u128` (from Uuid), `Option<T>` (`Ok(None)` for Null).

`ToSqlValue` converts Rust types to `Value` for query parameters. Impls: `i64`,
`i32`, `f64`, `str`, `String`, `bool`, `Vec<u8>`, `Option<T>`, `&T` (blanket),
`Value` (pass-through).

### `params` module

The named-parameter plumbing behind the default `Connection` methods, also
usable directly in custom backends or middleware.

| Function | Description |
|----------|-------------|
| `rewrite_named_params(sql)` | Rewrite `:name`/`$name`/`@name` to positional `$1`/`$2`/… and return the ordered name list |
| `bind_named_params(names, params)` | Map the name list to a positional `Vec<&dyn ToSqlValue>`, erroring with `OxiSqlError::Params` on a missing name |

### `OxiSqlError` — 11 variants

| Variant | Description |
|---------|-------------|
| `Parse(String)` | SQL could not be parsed |
| `Execution(String)` | Statement failed during execution |
| `NotConnected` | No connection available |
| `TypeMismatch { expected, got }` | Value had an unexpected type |
| `ConstraintViolation(String)` | Unique or FK constraint violated |
| `Timeout(String)` | Connection or query timeout |
| `ConnectionPool(String)` | Pool exhausted or build failure |
| `Migration(String)` | Migration error |
| `UnsupportedUri(String)` | URI scheme not supported by the backend |
| `Params(String)` | Missing or invalid named parameter |
| `Other(String)` | Any other error |

### Schema types

| Type | Key fields |
|------|-----------|
| `TableInfo` | `name`, `schema`, `table_type: TableType` |
| `TableType` | `Base`, `View`, `Other(String)` |
| `ColumnInfo` | `name`, `ordinal_position`, `data_type`, `nullable`, `default`, `max_length`, `numeric_precision`, `numeric_scale` |
| `IndexInfo` | `name`, `columns: Vec<String>`, `unique`, `primary` |
| `ForeignKeyInfo` | `constraint_name`, `column`, `foreign_table`, `foreign_column` |

### Type registry

- `TypeRegistry` — maps SQL type-name strings to the `SqlType` enum;
  case-insensitive, alias-aware (e.g. `INT4` → `Integer`, `JSONB` → `Json`).
- `SqlType` — `Integer`, `BigInt`, `SmallInt`, `Float`, `Double`, `Decimal`,
  `Text`, `VarChar(Option<u32>)`, `Blob`, `Boolean`, `Timestamp`, `Date`,
  `Time`, `Uuid`, `Json`, `Array(Box<SqlType>)`, `Unknown(String)`.

### Middleware

Composable wrappers over any `Connection`:

- `LoggingConnection` — logs every SQL operation with timing.
- `MetricsConnection` — per-operation counters and latencies, snapshot via `MetricsSnapshot`.
- `RetryConnection` — retries transient failures with a configurable `RetryPolicy` (`RetryPredicate` decides which errors are retryable).
- `TracingConnection` — mirrors `LoggingConnection` but emits `tracing` spans/events instead of `log` records (requires the `tracing` feature).

### Other public types

- `Cursor` — forward-only row cursor (`advance`, `peek`, `reset`, `skip_by`); implements `Iterator<Item = Row>`.
- `PreparedStatement` — compiled statement for repeated execution.
- `SelectBuilder` / `InsertBuilder` / `UpdateBuilder` / `DeleteBuilder` — type-safe query builders producing a `BuiltQuery`.
- `Migrator` — async migration trait (`apply`, `rollback`, `status`, `pending`); `status`/`pending` report `MigrationInfo` (`version`, `name`, `status: MigrationStatus`, `applied_at`).
- `SqlWarning` / `SqlWarningLevel` — non-fatal SQL warnings surfaced via `Connection::last_warnings()`; `parse_warning_level(&str)` parses a `SHOW WARNINGS` level string (unrecognised input defaults to `Warning`).

## Feature flags

| Feature | Adds | Description |
|---------|------|--------------|
| `chrono` | `dep:chrono` | `FromValue` for `NaiveDate` / `NaiveTime` / `NaiveDateTime` / `DateTime<Utc>` |
| `time` | `dep:time` | `FromValue` for `time::Date` / `Time` / `PrimitiveDateTime` / `OffsetDateTime` |
| `tracing` | `dep:tracing` | `TracingConnection<C>` middleware — `tracing` spans/events instead of `log` records |

All three are additive and off by default; the default build stays
dependency-light and Pure Rust.

## Test coverage

**155 tests pass** (142 unit + 13 doc), 0 failed, with `--all-features`.

## Part of the OxiSQL workspace

`oxisql-core` is the foundation crate of the OxiSQL workspace (17 crates: 10
facade/driver crates plus a 7-crate C-free `oxisqlite-*` engine). See the
[workspace README](../../README.md) for the full architecture and the 2,157
workspace tests (2,651 with `--all-features`).

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan).
Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).
Repository: <https://github.com/cool-japan/oxisql>
