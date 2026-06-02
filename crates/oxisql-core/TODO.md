# oxisql-core TODO

## Status
Core traits and types are fully implemented. `Connection` and `Transaction` async traits with `execute`, `query`, `execute_named`, `query_named`, `execute_batch`, and `ping`. `Row` with named column access, `try_get<T: FromValue>`, `column_count`, `is_null`, `into_values`. `Value` enum with 13 variants: Null, Bool, I64, F64, Text, Blob, Timestamp, Date, Time, Uuid, Json, Decimal, Array. `RowSet` wrapper. `FromValue` trait with impls for primitives. `ToSqlValue` trait. `OxiSqlError` with 10 variants (includes `Params`). `Cursor` for incremental result-set traversal. `Migrator` async trait with `apply`, `rollback`, `status`, `pending`. `TypeRegistry` for SQL-type name mapping. `params` module with `rewrite_named_params` and `bind_named_params`. 104 tests passing. ~1100 SLOC.

## Core Implementation
- [x] Add connection pooling trait — `ConnectionPool` with `get_conn()`, `return_conn()`, `pool_size()`, `max_size()`, `idle_count()` (~40 SLOC)
- [x] Add prepared statement support — `PreparedStatement` struct with `execute(params)` and `query(params)` methods, reusing server-side prepared plans (~50 SLOC)
- [x] Add query builder — fluent API for constructing SQL queries programmatically: `Query::select(&["col"]).from("table").where_eq("id", &42).build()` (~120 SLOC)
- [x] Add schema introspection trait — `SchemaInspector` with `tables()`, `columns(table)`, `primary_key(table)`, `foreign_keys(table)`, `indexes(table)` (~60 SLOC)
- [x] Add migration engine trait — `Migrator` with `apply(migration)`, `rollback(migration)`, `status()`, `pending()` returning migration state (~70 SLOC)
- [x] Add type mapping registry — bidirectional mapping between SQL types (INTEGER, TEXT, BOOLEAN, TIMESTAMP, ...) and `Value` variants, extensible by backends (~50 SLOC)
- [x] Add `Value::Decimal(BigDecimal)` variant for exact decimal arithmetic (important for financial data) (~15 SLOC)
- [x] Add `Value::Timestamp(i64)` variant for Unix timestamp with microsecond precision (~10 SLOC)
- [x] Add `Value::Date(i32)` variant for date-only values (days since epoch) (~10 SLOC)
- [x] Add `Value::Time(i64)` variant for time-of-day values (microseconds since midnight) (~10 SLOC)
- [x] Add `Value::Uuid(u128)` variant for UUID primary keys (~10 SLOC)
- [x] Add `Value::Json(String)` variant for JSON/JSONB columns (~10 SLOC)
- [x] Add `Value::Array(Vec<Value>)` variant for array columns (Postgres arrays) (~10 SLOC)
- [x] Add `Row::try_get<T: FromValue>(col)` — type-safe extraction with automatic conversion (~30 SLOC)
- [x] Add `FromValue` trait — `fn from_value(Value) -> Result<T, OxiSqlError>` with impls for i32, i64, f64, String, bool, Vec<u8>, Option<T> (~60 SLOC)
- [x] Add `Connection::execute_batch(sql)` — execute multiple semicolon-separated statements (~10 SLOC)
- [x] Add `Connection::ping()` — lightweight connectivity check (~5 SLOC)
- [x] Add `Transaction::savepoint(name)` — nested transactions via savepoints (~15 SLOC)
- [x] Add streaming query support — `Connection::query_stream(sql, params)` returning `Stream<Item = Result<Row>>` for large result sets (~30 SLOC)
- [x] Add cursor support — `Cursor` struct for incremental result set traversal (~40 SLOC)
- [x] Add `RowSet` type — wrapping `Vec<Row>` with schema metadata, column count, and convenience methods (~30 SLOC)

## API Improvements
- [x] Named parameter support — `execute_named` / `query_named` default trait methods on `Connection`, `params` module with `rewrite_named_params` / `bind_named_params`, `OxiSqlError::Params` variant; placeholders `:name`, `$name`, `@name` supported; all backends inherit automatically
- [x] Add `OxiSqlError::ConnectionPool` variant for pool-specific errors (exhausted, timeout) (~5 SLOC)
- [x] Add `OxiSqlError::Migration` variant for migration failures (~5 SLOC)
- [x] Add `OxiSqlError::Timeout` variant for query/connection timeouts (~5 SLOC)
- [x] Add `OxiSqlError::ConstraintViolation` variant for unique/foreign key violations (~5 SLOC)
- [x] Implement `Clone` for `Value` (already derived), add `PartialOrd` for sorting (~10 SLOC)
- [x] Implement `From<&str>` and `From<i32>` etc. for `Value` for ergonomic construction (~20 SLOC)
- [x] Add `Row::column_count()` method (~3 SLOC)
- [x] Add `Row::is_null(col)` method (~5 SLOC)
- [x] Add `Display` impl for `Value` — human-readable formatting (~15 SLOC)
- [x] Add `Display` impl for `Row` — tabular formatting (~15 SLOC)

## Testing
- [x] Unit tests for all `ToSqlValue` impls — verify each type converts correctly (~30 SLOC)
- [x] Unit tests for `Row::get` and `Row::get_by_index` with edge cases (~20 SLOC)
- [x] Unit tests for `FromValue` conversions — valid conversions and type mismatch errors (~30 SLOC)
- [x] Unit tests for query builder SQL generation — SELECT, INSERT, UPDATE, DELETE with WHERE, JOIN, ORDER BY (~40 SLOC)
- [x] Property-based tests for `Value` round-trip: `ToSqlValue -> Value -> FromValue` — 8 proptest tests in `tests/value_proptest.rs` covering i64, f64, bool, String, Option<i64>, Vec<u8>, i32 (~75 SLOC)
- [x] Test `OxiSqlError` Display formatting for all variants (~15 SLOC)

## Performance
- [x] Benchmark `Row::get` by name vs `Row::get_by_index` — measure column name lookup overhead (~20 SLOC) — `benches/value_benchmarks.rs`: `row_get_by_name_hit`, `row_get_by_name_miss`, `row_get_by_index_hit`
- [x] Add column name indexing with `HashMap` for O(1) `Row::get(col_name)` instead of linear scan (~20 SLOC)
- [x] Benchmark `Value` clone cost for large Text/Blob variants (~15 SLOC) — `benches/value_benchmarks.rs`: `value_text_clone`, `value_blob_clone`, `value_i64_clone`, `value_null_clone`
- [x] Add `Row::into_values()` for zero-copy value extraction (~10 SLOC)

## Integration
- [x] Ensure `Connection` trait is compatible with all backends (embedded, postgres, mysql) — **verified trait object safety**: `Connection` has no generic methods; all async methods use `&self`/`&str`/`&[&dyn ToSqlValue]` which are dyn-compatible; `async_trait` macro erases futures into `Box<dyn Future>`; all three backends compile and implement the trait. The `query_stream` method is a regular (non-async) fn returning `Pin<Box<dyn Stream>>` which is also dyn-safe. (~10 SLOC)
- [x] Verify `Value` enum covers all types needed by postgres (`types.rs`) and mysql (`types.rs`) backends — **verified**: Postgres uses BOOL→Bool, INT2/4/8→I64, FLOAT4/8→F64, TEXT/VARCHAR→Text, BYTEA→Blob, DATE→Date, TIMESTAMP/TZ→Timestamp, TIME→Time, UUID→Uuid, JSON/JSONB→Json, NUMERIC→Decimal, arrays→Array — all 13 Value variants are in use. MySQL uses Null, I64, F64, Text, Blob, Date, Timestamp, Time, Decimal, Json — subset of the 13 variants. No gaps found. (~15 SLOC audit)
- [x] Ensure `ToSqlValue` blanket impls work seamlessly with `oxisql-datafusion` type mapping — **verified**: `ToSqlValue` is implemented for all primitive Rust types (i64, i32, f64, bool, str, String, Vec<u8>, Option<T>, &T, Value) and the blanket `&T: ToSqlValue` impl propagates correctly; oxisql-datafusion converts OxiSQL `Value` variants to DataFusion `ScalarValue` independently in `types.rs`, no friction with ToSqlValue. (~10 SLOC)
- [x] Coordinate `Value` extensions (Decimal, Timestamp, Uuid, Json) with backend type conversion modules — **verified**: all four extended types are handled consistently across embedded (`glue_value_to_oxisql`), postgres (`extract_value`), and mysql (`mysql_value_to_core_with_type`); oxisql-datafusion maps these to DataFusion scalar types in `types.rs`. No coordination gaps found. (~30 SLOC per backend)
