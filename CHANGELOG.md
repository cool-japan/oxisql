# Changelog

All notable changes to OxiSQL will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1] - 2026-06-04

### Added

#### CSV Import / Export (`oxisql-embedded`)
- `EmbeddedConnection::import_csv(table_name, csv_data)` — import RFC 4180 CSV directly into a new table; first row is treated as the header, column names are sanitised (spaces/hyphens → underscores, leading digits get `col_` prefix), empty fields become `NULL`, all values stored as `TEXT`
- `EmbeddedConnection::export_table_to_csv(table_name)` — export any table to RFC 4180-compliant CSV (CRLF line endings); values containing commas, double-quotes, or newlines are properly quoted; `NULL` exports as an empty field
- `oxisql_embedded::csv` module (public) — standalone CSV utilities: `parse_csv`, `build_csv_output`, `value_to_csv_field`, `sanitise_column_name`, `build_create_table_sql`, `build_insert_sql`, `quote_csv_field`; zero external `csv`-crate dependency, hand-rolled state-machine parser handles quoted fields, `""` escapes, bare LF, CRLF, and embedded newlines

#### Interactive SQL REPL (`oxisql` facade — `repl` feature)
- `oxisql-repl` binary — interactive Read-Eval-Print Loop over any OxiSQL backend; supports `memory://`, `postgres://`, `mysql://`, and `sqlite://` URIs; multi-line statement accumulation (flush on `;` or blank line); tabular result rendering with auto-sized columns and truncation
- Dot commands: `.help`, `.tables`, `.schema <table>`, `.quit` / `.exit` / `.q`
- New `repl` feature flag in `oxisql/Cargo.toml` (activates `embedded` + `tokio` + `anyhow`); binary is conditionally compiled (`required-features = ["repl"]`)

### Fixed
- `unique_test_dir` helper in `oxisql-embedded/tests/memory_persistent.rs` is now guarded with `#[cfg(any(feature = "fjall-storage", feature = "redb-storage"))]`, eliminating the `dead_code` warning when building with default features

## [0.1.0] - 2026-06-01

### Added

#### Core (`oxisql-core`)
- `Connection` trait — unified async database connection abstraction
- `Transaction` trait — ACID transaction management with commit/rollback
- `PreparedStatement` trait — parameterized query execution
- `ConnectionPool` trait — generic connection pool abstraction
- `Migrator` trait — schema migration lifecycle management
- `Value` enum with 13 variants: `Integer`, `Float`, `Bool`, `Text`, `Blob`, `Null`, `Decimal`, `Timestamp`, `Date`, `Time`, `Uuid`, `Json`, `Array`
- `Row` and `RowSet` types for query result representation
- `FromValue` trait for ergonomic value extraction
- `SchemaInfo`, `ColumnInfo`, `IndexInfo`, `ForeignKeyInfo` for schema introspection
- `Middleware` trait and query middleware pipeline for cross-cutting query concerns

#### Embedded Backend (`oxisql-embedded`)
- GlueSQL in-memory engine with full `Connection` + `Transaction` support
- `export_as_sql()` / `import_from_sql()` for portable data serialization
- Zero external native dependencies — 100% Pure Rust

#### PostgreSQL Backend (`oxisql-postgres`)
- Pure-Rust `tokio-postgres` driver with `rustls` TLS (no `libpq` dependency)
- Extended type mapping: `DATE` → `Value::Date`, `TIMESTAMP` / `TIMESTAMPTZ` → `Value::Timestamp`, `UUID` → `Value::Uuid`, `JSONB` / `JSON` → `Value::Json`, `NUMERIC` → `Value::Decimal`, `ARRAY` → `Value::Array`
- Async connection and transaction support via Tokio

#### MySQL Backend (`oxisql-mysql`)
- Pure-Rust `mysql_async` driver with `rustls` TLS (no `libmysqlclient` dependency)
- Extended type mapping: `DATE`, `DATETIME`, `TIMESTAMP`, `DECIMAL`, `JSON` → proper `Value` variants
- Async connection and transaction support via Tokio

#### SQLite-Compatible Backend (`oxisql-sqlite-compat`)
- Pure-Rust SQLite-compatible engine backed by Limbo (no `libsqlite3` dependency)
- `foreign_keys()` — introspect foreign key constraints via DDL parsing
- `indexes()` — introspect indexes via DDL parsing

#### Connection Pooling (`oxisql-pool`)
- `OxidbPgPool` — PostgreSQL connection pool
- `MysqlPool` — MySQL connection pool
- `EmbeddedPool` — GlueSQL in-memory connection pool
- `SqliteCompatPool` — SQLite-compatible connection pool
- All pools implement the `ConnectionPool` trait
- `connect_pooled(uri, size)` — URI-scheme-based pool dispatch (auto-selects backend)

#### SQL Parsing & Planning (`oxisql-parse`)
- `QueryBuilder` — programmatic query construction
- Query planner with predicate pushdown optimization
- Join algorithm selection
- Aggregate processing pipeline
- Statement validation and normalization
- LRU parse cache for repeated query patterns

#### Migrations (`oxisql-migrate`)
- `MigrationRunner` — file-based migration execution
- 14-digit timestamp migration filenames for deterministic ordering
- `run_with_pool()` / `run_pooled()` for pooled execution
- `status()` — report applied vs. pending migrations
- `pending()` — list unapplied migration files

#### DataFusion Integration (`oxisql-datafusion`)
- `OxiSqlTableProvider` — expose any OxiSQL backend as a DataFusion `TableProvider`
- `OxiSqlContext` — unified OLAP query context over all backends
- Enables analytical SQL (window functions, complex aggregations) across all supported engines

#### Unified Facade (`oxisql`)
- `connect(uri)` — single entry point; dispatches to the correct backend by URI scheme
- `connect_pooled(uri, size)` — pooled variant with configurable pool size
- `connect_pool(uri)` — returns a type-erased `ConnectionPool`
- Feature flags: `postgres`, `mysql`, `embedded`, `sqlite`, `sqlite-compat`, `pool-postgres`, `pool-mysql`, `pool-embedded`, `pool-sqlite`, `datafusion`
- All backends are 100% Pure Rust with no C/C++/Fortran native dependencies

### Added (second ultra pass — 2026-05-30)

#### Named Parameters (`oxisql-core`)
- `Connection::execute_named` and `Connection::query_named` — default trait methods
  providing named-placeholder support (`:name`, `$name`, `@name`) across all backends
  with zero per-backend code. Implemented in `oxisql-core::params`.
- `OxiSqlError::Params` — new error variant returned on named-parameter binding failures.
- Available via `use oxisql::prelude::*` or `use oxisql_core::Connection`.

#### EmbeddedConnection Schema Introspection (`oxisql-embedded`)
- `EmbeddedConnection` now fully implements `tables()`, `columns()`, `indexes()`, and
  `foreign_keys()` via the GlueSQL catalog. Previously these returned `Err("not supported")`.

#### SQLite-compat improvements (`oxisql-sqlite-compat`)
- `SqliteTransaction::rollback()` now returns a clear
  `OxiSqlError::Other("ROLLBACK is not supported by the limbo 0.0.22 engine…")` instead
  of a cryptic parse error.
- Statement cache infrastructure: 128-slot LRU cache keyed by rewritten SQL text is in
  place; activates once limbo fixes the `Statement::reset()` / `Program::n_change` bug.

[Unreleased]: https://github.com/cool-japan/oxisql/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/cool-japan/oxisql/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cool-japan/oxisql/releases/tag/v0.1.0
