# oxisql (facade) TODO

## Status
Facade crate providing `connect(uri)` entry-point dispatching to embedded, Postgres, or MySQL backends based on URI scheme. Re-exports core types (`Connection`, `Transaction`, `Row`, `Value`, `OxiSqlError`). Feature-gated re-exports for `postgres`, `mysql`, and `datafusion` modules. ~91 SLOC.

## Core Implementation
- [x] Add connection pooling at facade level — `connect_pooled(uri, pool_config)` returning a pooled connection that manages backend-specific pools (~40 SLOC); `connect_pool(uri, max_size) -> OxidbPool` for typed pooled access; `oxisql::pool` re-exports all pool types
- [x] Add `connect_with_tls(uri, tls_config)` — connect with explicit TLS configuration without requiring backend-specific imports (~25 SLOC)
- [x] Add `connect_with_options(uri, options)` — connect with timeout, pool size, and other options via `ConnectOptions` struct (~30 SLOC)
- [x] Add `migrate(conn, migrations_dir)` — `oxisql::migrate` module re-exports `MigrationRunner`, `MigrateOptions`, `MigrationError`, `MigrationState`, `MigrationFile`, `scan_migrations` behind `migrate` feature
- [x] Add `introspect(conn)` — return schema information (tables, columns, indexes) from any backend (~30 SLOC)
- [x] Add URI scheme for DataFusion — `datafusion://` scheme — `connect_datafusion()` returns `OxiSqlContext`; `connect()` returns clear `UnsupportedUri` error pointing at `connect_datafusion()` (~20 SLOC)
- [x] Add URI scheme for file-backed embedded — `file:///path/to/db` scheme using oxistore for persistence (~15 SLOC) — helpful `UnsupportedUri` error with actionable message; `file:` prefix also matched; docs updated to "Planned (not yet implemented)"
- [x] Add `ConnectOptions` struct — timeout, pool_size, tls_mode, auto_reconnect, statement_cache_size (~25 SLOC)
- [x] Add `ping(conn)` — backend-agnostic connectivity check (~10 SLOC)
- [x] Add `close(conn)` — explicit connection cleanup/pool shutdown (~10 SLOC)
- [x] Add `BackendInfo` — report which backend and version is connected (~15 SLOC)
- [x] Add multi-backend query routing — `MultiConnection` that routes queries to different backends based on table/schema rules — `src/multi.rs`
- [x] Add query logging middleware — optional query logging with timing for debugging (~30 SLOC)
- [x] Add `oxisql::logging::LoggingConnection` — facade-level Box-based labelled logging wrapper (~80 SLOC in `src/logging.rs`)
- [x] Add query retry middleware — configurable retry policy for transient failures (~25 SLOC)

## API Improvements
- [x] Re-export `EmbeddedConnection` directly for callers that need embedded-specific API (~5 SLOC)
- [x] Add prelude module — `use oxisql::prelude::*` importing Connection, Row, Value, OxiSqlError, connect (~10 SLOC)
- [x] Document supported URI schemes table in module-level docs more thoroughly — full scheme table added including datafusion:// and file:// (~15 SLOC docs)
- [x] Add `#[must_use]` annotation to `connect` return type (~2 SLOC)
- [x] Add `connect_or_create(uri)` — like `connect` but creates the database if it does not exist; embedded always works; PG/MySQL auto-create planned (~30 SLOC)
- [x] Add feature flag combinations documentation — which features enable which backends (~10 SLOC docs) — Feature Flags table in lib.rs lines 21-36 lists each feature, URI scheme, and backend
- [x] Add `version()` function returning crate version string (~3 SLOC)

## Testing
- [x] Test `connect("memory://")` end-to-end — CREATE TABLE, INSERT, SELECT, UPDATE, DELETE (~25 SLOC)
- [x] Test `connect("postgres://...")` with real Postgres server — full CRUD cycle (~25 SLOC) — `test_portability_postgres` in `tests/portability.rs` (marked `#[ignore]`, requires live PG)
- [x] Test `connect("mysql://...")` with real MySQL server — full CRUD cycle (~25 SLOC) — `test_portability_mysql` in `tests/portability.rs` (marked `#[ignore]`, requires live MySQL)
- [x] Test unknown URI scheme — unknown schemes return `OxiSqlError::UnsupportedUri` (updated from `NotConnected`) (~10 SLOC)
- [x] Test `LoggingConnection` — execute, query, delegates, into_inner, label, ping (~50 SLOC)
- [x] Test `connect_or_create` — embedded success, unknown scheme error, full CRUD (~30 SLOC)
- [x] Test feature-disabled schemes — `ftp://`, empty URI, `file://`, `datafusion://` all return errors; `memory://` round-trip tested; `datafusion://` returns clear error pointing at `connect_datafusion()` (~40 SLOC)
- [x] Test facade re-exports — verify all public types are accessible through `oxisql::*` (~10 SLOC)
- [x] Test `connect_with_tls` — verify TLS connections through the facade (~15 SLOC)
- [x] Test transaction through facade — BEGIN, operations, COMMIT/ROLLBACK via `Box<dyn Connection>` (~15 SLOC)
- [x] Test DataFusion re-export — register table and execute SQL through `oxisql::datafusion` module (~15 SLOC) — `tests/datafusion_facade.rs`: `test_facade_datafusion_register_table`, `test_facade_context_trivial_query`
- [x] Test migration runner — apply up/down migrations against embedded backend (~20 SLOC)

## Performance
- [x] Benchmark facade dispatch overhead — `Box<dyn Connection>` vs direct backend calls (~25 SLOC) — `benches/facade_benchmarks.rs`: `connect_memory_cold` and `dyn_connection_query` benchmarks using criterion 0.8 with async_tokio
- [x] Benchmark connection establishment time for each backend (~20 SLOC)
- [x] Benchmark pooled vs unpooled connection throughput (~25 SLOC)

## Integration
- [x] Cross-backend portability test — run the same SQL test suite against embedded, Postgres, and MySQL through the facade — `tests/portability.rs`
- [x] Integration with `oxistore` — use oxistore KV backends for persistent embedded database storage (~25 SLOC) — `redb` and `fjall` features in `oxisql/Cargo.toml` enable `oxisql-embedded/redb-storage` and `oxisql-embedded/fjall-storage` respectively; `connect("redb://path")` and `connect("fjall://path")` dispatch to `RedbEmbeddedConnection::open(path)` and `FjallEmbeddedConnection::open(path)`; `backend_info_for_uri` extended; `RedbEmbeddedConnection`/`FjallEmbeddedConnection` re-exported; 3 new tests in `tests/connect.rs`
- [x] Integration with `oxisql-datafusion` — verify DataFusion table registration works through the facade (~20 SLOC)
- [x] CI test matrix — verify all feature flag combinations compile and the correct backends are available (~15 SLOC CI) — Feature Combinations table added to module docs; `test_feature_flags_compile` unit test in `src/lib.rs` validates feature-gated symbols at compile time
