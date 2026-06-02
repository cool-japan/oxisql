# oxisql-core — Core traits and types for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-core.svg)](https://crates.io/crates/oxisql-core)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

`oxisql-core` defines the public API surface that every OxiSQL backend must implement. It contains no storage logic — concrete backends live in `oxisql-embedded`, `oxisql-postgres`, `oxisql-mysql`, etc.

## Installation

```toml
[dependencies]
oxisql-core = "0.1.0"
```

## Quick Start

```rust
use oxisql_core::{Row, Value};

let row = Row::new(
    vec!["id".into(), "name".into()],
    vec![Value::I64(42), Value::Text("Alice".into())],
);
let id: i64 = row.try_get("id").unwrap();
let name: String = row.try_get("name").unwrap();
assert_eq!(id, 42);
assert_eq!(name, "Alice");
```

### Named parameters

All backends automatically inherit `execute_named` and `query_named` as
default methods on `Connection`. Placeholders may use `:name`, `$name`, or
`@name` syntax; repeated use of the same name reuses the same positional value.

```rust
// execute_named — DML/DDL with named params
conn.execute_named(
    "INSERT INTO users (id, name) VALUES (:id, :name)",
    &[("id", &42i64 as &dyn ToSqlValue), ("name", &"Alice" as &dyn ToSqlValue)],
).await?;

// query_named — SELECT with named params
let rows = conn.query_named(
    "SELECT * FROM users WHERE id = :id",
    &[("id", &42i64 as &dyn ToSqlValue)],
).await?;
```

## API Overview

### `Connection` trait

All backends implement `Connection` (from `traits.rs`). It is `Send + Sync` and fully async via `async_trait`.

| Method | Description |
|--------|-------------|
| `execute(&self, sql, params)` | Execute DML/DDL, return rows-affected count |
| `query(&self, sql, params)` | Execute SELECT, return `Vec<Row>` |
| `execute_named(&self, sql, params)` | Execute DML/DDL with named parameters (default method) |
| `query_named(&self, sql, params)` | Execute SELECT with named parameters (default method) |
| `transaction(&self)` | Begin a transaction, return `Box<dyn Transaction>` |
| `execute_batch(&self, sql)` | Execute multiple semicolon-separated statements |
| `ping(&self)` | Lightweight connectivity check (`SELECT 1`) |
| `prepare(&self, sql)` | Compile a statement for repeated execution |
| `tables(&self)` | List all tables (schema introspection) |
| `columns(&self, table)` | List columns of a named table |
| `indexes(&self, table)` | List indexes on a named table |
| `foreign_keys(&self, table)` | List FK constraints on a named table |
| `query_stream(...)` | Execute SELECT, return `Pin<Box<dyn Stream<Item=...>>>` |

Positional parameters use `$1`, `$2`, … notation. The `params` argument is `&[&dyn ToSqlValue]`.

Named-parameter methods accept `&[(&str, &dyn ToSqlValue)]`. Placeholders
`:name`, `$name`, and `@name` are all supported; the same name may appear
multiple times and will always bind to the same value. Both methods are
**default** implementations on the `Connection` trait — every backend
inherits them automatically with no additional code.

### `Transaction` trait

Obtained via `Connection::transaction()`. Dropped without commit → implicit rollback.

| Method | Description |
|--------|-------------|
| `execute(&mut self, sql, params)` | DML/DDL inside the transaction |
| `query(&mut self, sql, params)` | SELECT inside the transaction |
| `commit(self: Box<Self>)` | Commit all changes |
| `rollback(self: Box<Self>)` | Roll back all changes |
| `savepoint(&mut self, name)` | Create a named savepoint |
| `release_savepoint(&mut self, name)` | Release (discard) a savepoint |
| `rollback_to_savepoint(&mut self, name)` | Roll back to a named savepoint |

### `ConnectionPool` trait

Object-safe (`Box<dyn ConnectionPool>` is valid).

| Method | Description |
|--------|-------------|
| `get(&self)` | Check out a `Box<dyn Connection + Send>` |
| `pool_size(&self)` | Maximum pool capacity |
| `idle_count(&self)` | Connections available for checkout |
| `active_count(&self)` | Connections currently checked out |
| `health_check(&self)` | Verify pool is healthy |
| `close(&self)` | Drain idle connections, prevent new checkouts |

### `Value` enum — 13 variants

| Variant | Rust type | SQL type |
|---------|-----------|----------|
| `Null` | — | NULL |
| `Bool(bool)` | `bool` | BOOLEAN |
| `I64(i64)` | `i64` | INTEGER / BIGINT |
| `F64(f64)` | `f64` | REAL / DOUBLE PRECISION |
| `Text(String)` | `String` | TEXT / VARCHAR |
| `Blob(Vec<u8>)` | `Vec<u8>` | BYTEA / BLOB |
| `Timestamp(i64)` | microseconds since epoch (UTC) | TIMESTAMP / TIMESTAMPTZ |
| `Date(i32)` | days since Unix epoch | DATE |
| `Time(i64)` | microseconds since midnight | TIME |
| `Uuid(u128)` | 128-bit UUID | UUID |
| `Json(String)` | UTF-8 JSON string | JSON / JSONB |
| `Decimal(String)` | exact decimal as string | NUMERIC / DECIMAL |
| `Array(Vec<Value>)` | ordered collection | e.g. INTEGER[] |

`Value` implements `Display`, `PartialOrd` (cross-type comparisons return `None`), and `From<bool/i32/i64/f64/String/&str/Vec<u8>/Option<T>>`.

### `Row` and `RowSet`

`Row` stores column names and values with an O(1) HashMap index for column lookups.

| Method | Description |
|--------|-------------|
| `Row::new(columns, values)` | Construct from parallel vecs |
| `row.get(col)` | Look up by column name → `Option<&Value>` |
| `row.get_by_index(i)` | Look up by zero-based index |
| `row.try_get::<T>(col)` | Type-safe extraction via `FromValue` |
| `row.try_get_by_index::<T>(i)` | Type-safe extraction by index |
| `row.column_count()` | Number of columns |
| `row.is_null(col)` | True if column exists and is NULL |
| `row.into_values()` | Consume, return `Vec<Value>` |
| `row.columns()` | Ordered column names |

`RowSet` wraps `Vec<Row>` with convenience methods: `len()`, `is_empty()`, `column_count()`, `columns()`, `rows()`, `into_rows()`, `from_rows(Vec<Row>)`.

### `FromValue` trait

Extracts typed values from `Value`. Returns `OxiSqlError::TypeMismatch` on mismatch.

Implementations: `bool`, `i32` (range-checked), `i64`, `f64` (accepts I64 coercion), `String` (accepts Text/Json/Decimal/Uuid), `Vec<u8>`, `u128` (from Uuid), `Option<T>` (Ok(None) for Null).

### `ToSqlValue` trait

Converts Rust types to `Value` for use as query parameters.

Implementations: `i64`, `i32`, `f64`, `str`, `String`, `bool`, `Vec<u8>`, `Option<T>`, `&T` (blanket), `Value` (pass-through).

### `params` module

The `params` module exposes the named-parameter plumbing used by the default
`Connection` methods, but is also available for direct use in custom backends
or middleware.

| Function | Description |
|----------|-------------|
| `rewrite_named_params(sql)` | Rewrite `:name`/`$name`/`@name` placeholders to positional `$1`/`$2`/… and return the ordered name list |
| `bind_named_params(names, params)` | Map the ordered name list to a positional `Vec<&dyn ToSqlValue>` slice, returning `OxiSqlError::Params` if a name is missing |

### `OxiSqlError` variants

| Variant | Description |
|---------|-------------|
| `Parse(String)` | SQL could not be parsed |
| `Execution(String)` | Statement failed during execution |
| `NotConnected` | No connection available |
| `TypeMismatch { expected, got }` | Value had unexpected type |
| `ConstraintViolation(String)` | Unique or FK constraint violated |
| `Timeout(String)` | Connection or query timeout |
| `ConnectionPool(String)` | Pool exhausted or build failure |
| `Migration(String)` | Migration error |
| `UnsupportedUri(String)` | URI scheme not supported by backend |
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

### Other public types

- `Cursor` — forward-only row cursor with `advance()`, `peek()`, `reset()`, `skip_by()`, implements `Iterator<Item=Row>`
- `PreparedStatement` — compiled statement for repeated execution
- `SelectBuilder`, `InsertBuilder`, `UpdateBuilder`, `DeleteBuilder` — type-safe query builders
- `TypeRegistry` — maps SQL type name strings to `SqlType` enum; case-insensitive, supports aliases (e.g. `INT4` → `Integer`, `JSONB` → `Json`)
- `SqlType` — `Integer`, `BigInt`, `SmallInt`, `Float`, `Double`, `Decimal`, `Text`, `VarChar(Option<u32>)`, `Blob`, `Boolean`, `Timestamp`, `Date`, `Time`, `Uuid`, `Json`, `Array(Box<SqlType>)`, `Unknown(String)`

## License

Apache-2.0 — COOLJAPAN OU (Team Kitasan)
