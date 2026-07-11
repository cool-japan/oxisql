# oxisql-core — TODO

## Status

**Stable** · version 0.3.2 · **114 tests**.

Core traits and types are fully implemented and shared by every OxiSQL backend.
`Connection` / `Transaction` / `ConnectionPool` / `PreparedStatement` async
traits; the 13-variant `Value` enum; `Row` / `RowSet`; `FromValue` /
`ToSqlValue`; `OxiSqlError` (11 variants); `Cursor`; `Migrator`; `TypeRegistry`
+ `SqlType`; the `params` named-parameter module; query builders; and the
logging / metrics / retry middleware. **No feature flags. Zero `unsafe`.**

## Done

### Traits
- [x] `Connection` async trait — `execute`, `query`, `execute_named`, `query_named`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream`
- [x] `Transaction` async trait — `execute`, `query`, `commit`, `rollback`, `savepoint`, `release_savepoint`, `rollback_to_savepoint`
- [x] `ConnectionPool` object-safe trait — `get`, `pool_size`, `idle_count`, `active_count`, `health_check`, `close`
- [x] `PreparedStatement` — compiled statement for repeated execution
- [x] `Migrator` async trait — `apply`, `rollback`, `status`, `pending`
- [x] Trait object-safety verified for `Connection` across embedded / postgres / mysql backends

### Value model
- [x] `Value` enum, 13 variants: `Null`, `Bool`, `I64`, `F64`, `Text`, `Blob`, `Decimal`, `Timestamp`, `Date`, `Time`, `Uuid`, `Json`, `Array`
- [x] `Display`, `PartialOrd` (cross-type → `None`), and `From<…>` ergonomic constructors for `Value`
- [x] `Row` with O(1) `HashMap` column index, `try_get::<T: FromValue>`, `get` / `get_by_index`, `is_null`, `column_count`, `into_values`, `columns`
- [x] `RowSet` wrapper with `len` / `is_empty` / `column_count` / `columns` / `rows` / `into_rows` / `from_rows`
- [x] `FromValue` impls: `bool`, `i32` (range-checked), `i64`, `f64`, `String`, `Vec<u8>`, `u128`, `Option<T>`
- [x] `ToSqlValue` impls: `i64`, `i32`, `f64`, `str`, `String`, `bool`, `Vec<u8>`, `Option<T>`, `&T` blanket, `Value` pass-through

### Errors
- [x] `OxiSqlError`, 11 variants: `Parse`, `Execution`, `NotConnected`, `TypeMismatch { expected, got }`, `ConstraintViolation`, `Timeout`, `ConnectionPool`, `Migration`, `UnsupportedUri`, `Params`, `Other`
- [x] `Display` for every error variant

### Named parameters
- [x] `execute_named` / `query_named` default methods on `Connection`
- [x] `params` module: `rewrite_named_params`, `bind_named_params`
- [x] `:name`, `$name`, `@name` placeholder syntax; repeated names reuse one positional value
- [x] `OxiSqlError::Params` for missing/invalid names; all backends inherit named-param support automatically

### Schema, types, cursors, builders
- [x] Schema types: `TableInfo`, `TableType` (`Base` / `View` / `Other`), `ColumnInfo`, `IndexInfo`, `ForeignKeyInfo`
- [x] `TypeRegistry` + `SqlType` enum — case-insensitive, alias-aware SQL-type mapping
- [x] `Cursor` — forward-only traversal (`advance`, `peek`, `reset`, `skip_by`); implements `Iterator`
- [x] Query builders `SelectBuilder` / `InsertBuilder` / `UpdateBuilder` / `DeleteBuilder` → `BuiltQuery`

### Middleware
- [x] `LoggingConnection` — logs every SQL operation with timing
- [x] `MetricsConnection` (+ `MetricsSnapshot`) — per-operation counters and latencies
- [x] `RetryConnection` (+ `RetryPolicy`, `RetryPredicate`) — retry transient failures

### Testing & performance
- [x] Unit tests for all `ToSqlValue` / `FromValue` conversions (valid + type-mismatch)
- [x] `Row::get` / `get_by_index` edge-case tests
- [x] Query-builder SQL-generation tests
- [x] Property-based `Value` round-trip tests (`ToSqlValue → Value → FromValue`)
- [x] `OxiSqlError` `Display` formatting tests
- [x] Benchmarks: `Row::get` by name vs index, `Value` clone cost for Text/Blob; O(1) name lookup via `HashMap`; `into_values()` zero-copy extraction

## Roadmap / next
- [x] `FromValue` for chrono / time crate types behind an optional feature (keeping the default Pure-Rust, dependency-light) (done 2026-06-10)
  - **Goal:** `chrono::NaiveDate/NaiveTime/NaiveDateTime/DateTime<Utc>` and `time::Date/Time/PrimitiveDateTime/OffsetDateTime` implement `FromValue`; default build unchanged.
  - **Design:** Add optional `chrono` and `time` features in `Cargo.toml` (latest versions, `*.workspace = true`). Implement `FromValue` by matching `Value::Date`, `Value::Time`, `Value::Timestamp` and fallbacks from `Value::Text`/`Value::I64` (epoch). Compile only when the respective feature is enabled via `#[cfg(feature = "chrono")]` / `#[cfg(feature = "time")]` guards in `src/value.rs`.
  - **Files:** `src/value.rs`, `Cargo.toml` (workspace root + crate)
  - **Tests:** round-trip `Value::Date → chrono::NaiveDate → Value::Date`; same for `time::Date`; error on type mismatch.
  - **Risk:** Low — purely additive, feature-gated. Verify no C dep enters (chrono/time are Pure Rust).
- [x] `Value::Array` element-type metadata for richer Postgres array round-trips
- [x] Streaming `Transaction::query_stream` to mirror `Connection::query_stream` (done 2026-06-10)
  - **Goal:** Callers can iterate a `Transaction`'s results as a `Stream` with the same API as `Connection::query_stream`.
  - **Design:** Add `fn query_stream<'a>(&'a mut self, sql: &'a str, params: &'a [&'a dyn ToSqlValue]) -> Pin<Box<dyn Stream<Item=Result<Row, OxiSqlError>> + Send + 'a>>` to the `Transaction` trait with a default body identical to `Connection::query_stream` (traits.rs:248-259) but using `self.query(...)`. Object-safe: lifetime-only generic, `&mut self`, no `Self` in sig. All 9 `impl Transaction` sites inherit the default — zero backend edits.
  - **Files:** `src/traits.rs`
  - **Tests:** `Transaction::query_stream` yields the same rows as `Transaction::query` on an embedded/sqlite txn.
  - **Risk:** Low — additive default method. Object-safety is preserved (same pattern proven on Connection).
- [x] Optional borrowed `Value<'a>` to reduce allocation on large Text/Blob result sets (done 2026-06-19)
  - **Goal:** `BorrowedValue<'a>` zero-allocation view type with `Text(&'a str)` / `Blob(&'a [u8])` / `Json(&'a str)` / `Decimal(&'a str)` variants; all scalars copied inline; `to_owned()` → `Value`; `From<&'a Value> for BorrowedValue<'a>`; `Display`.
  - **Files:** `src/value.rs` (added `BorrowedValue<'a>` + impl + 15 tests); `src/lib.rs` (re-exported `BorrowedValue`).
  - **Tests:** 15 unit tests covering type_name, is_null, Text/Blob zero-alloc, scalar roundtrips, From<&Value>, Display, UUID format, roundtrip for all 12 non-Array variants.
- [x] Tracing-based middleware variant alongside the `log`-based `LoggingConnection` (done 2026-06-10)
  - **Goal:** `TracingConnection<C>` emits `tracing` spans/events instead of `log` records; identical delegation to the existing `LoggingConnection`.
  - **Design:** Add `tracing` as an **optional** dep (`tracing = { workspace = true, optional = true }`) behind a new `tracing` feature (add to workspace Cargo.toml if not present, latest version). In `src/middleware.rs`, mirror `LoggingConnection`'s full `Connection + Transaction` delegation for `TracingConnection<C>`. Re-export from `src/lib.rs` behind `#[cfg(feature = "tracing")]`.
  - **Files:** `src/middleware.rs`, `src/lib.rs`, `Cargo.toml` (workspace root + crate)
  - **Tests:** `TracingConnection` delegates `execute`/`query` and records a span (capture via a `tracing_subscriber::fmt` test subscriber with in-memory writer).
  - **Risk:** Low — additive feature gate. `tracing` is Pure Rust.

## Known limitations
None. `oxisql-core` is the stable foundation crate: no feature flags, zero
`unsafe`, and a complete trait/type surface. Backend-specific caveats live in
the respective backend crates, not here.
