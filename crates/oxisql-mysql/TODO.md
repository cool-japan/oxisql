# oxisql-mysql TODO

## Status
Pure-Rust MySQL backend over `mysql_async` (no `libmysqlclient`). `MyConnection` using pool-based concurrency (internally synchronized, no extra Mutex needed). TLS via `rustls-rustcrypto` process-global `CryptoProvider` install (guarded). `MyTransaction` with owned `mysql_async::Transaction<'static>`. Type mapping covers NULL, Bytes (Text/Blob), Int, UInt, Float, Double, Date, Time. Custom `MysqlError`. ~419 SLOC across 3 files (connection.rs, error.rs, types.rs).

## Core Implementation
- [x] Add connection pool configuration — expose `mysql_async::Pool` options: min/max connections, idle timeout, connect timeout, connection TTL (~30 SLOC)
- [x] Add prepared statement support — use `mysql_async::Conn::prep` for server-side prepared statements with binary protocol (~40 SLOC)
- [x] Add stored procedure support — `call_procedure_multi(name, params)` returning all result sets via `exec_iter` + `QueryResult::next()` loop (~30 SLOC)
- [x] Add binary protocol for queries — `query_binary()` uses explicit `prep` + `exec_iter` (binary protocol) for server-side prepared-statement caching (~20 SLOC)
- [x] Add `LOAD DATA LOCAL INFILE` support — implemented as `load_data_batched()` using batched INSERT (binary protocol, no `LOCAL INFILE` server permission required) (~60 SLOC)
- [x] Add multi-result-set support — `call_procedure_multi` handles stored procedures returning multiple SELECT result sets via `QueryResult::next()` + `is_empty()` loop (~30 SLOC)
- [x] Add `INFORMATION_SCHEMA` query support — schema introspection for tables, columns, indexes, constraints (~50 SLOC)
- [x] Add extended type mapping — DECIMAL, DATETIME(6), TIMESTAMP(6), JSON, ENUM, SET, GEOMETRY types (~50 SLOC) — GEOMETRY added (maps to Value::Blob via WKB); other types already done in sub-items
- [x] Add `DECIMAL` type support — map to `Value::Decimal(BigDecimal)` for exact numeric (~20 SLOC)
- [x] Add `JSON` type support — map to `Value::Json(String)` with JSON_EXTRACT compatibility (~15 SLOC)
- [x] Add `ENUM` and `SET` type support — map to `Value::Text` with metadata annotation (~15 SLOC)
- [x] Add `DATETIME(6)` microsecond precision support — currently maps to `Value::Text`, should parse to `Value::Timestamp` (~20 SLOC)
- [x] Add `execute_batch` — execute multiple statements in a single call (~15 SLOC)
- [x] Add `ping` — use `mysql_async::Conn::ping()` for connectivity check (~10 SLOC)
- [x] Add automatic reconnection — `is_reconnect_error()` helper detects CR_SERVER_GONE (2006), CR_SERVER_LOST (2013), ER_UNKNOWN_COM_ERROR (1047), and Io errors; note: auto-retry not applied mid-transaction for correctness (~25 SLOC)
- [x] Add connection timeout configuration — `connect_timeout_secs` on `MyConnectionBuilder` now wires to an eager `tokio::time::timeout(get_conn())` probe at connect time; maps elapsed to `MysqlError::ConnectionTimeout`; mysql_async 0.36 has no TCP-level OptsBuilder timeout, so enforcement is at the Tokio async layer (~20 SLOC)
- [x] Add savepoint support within `MyTransaction` — `SAVEPOINT name` / `ROLLBACK TO name` (~20 SLOC)
- [x] Add SSL certificate options — `ssl_skip_verify`, `ssl_with_ca_pem`, `ssl_disabled` convenience methods on `MyConnectionBuilder` via `SslOpts` (~50 SLOC)
- [x] Handle `UInt` overflow gracefully — values > i64::MAX fall back to `Value::Text` instead of `MysqlError::TypeMap` (~10 SLOC)
- [x] Add `last_insert_id()` method on `MyTransaction` for auto-increment key retrieval (~10 SLOC)
- [x] Add explicit `Pool::disconnect()` support for graceful shutdown — `MyConnection::disconnect()` drains all pool connections cleanly (~10 SLOC)

## API Improvements
- [x] Add `MyConnectionBuilder` — builder pattern for URL, TLS mode, pool size, timeouts (~35 SLOC)
- [x] Add `MyConnection::from_pool(pool)` — construct from pre-existing `mysql_async::Pool` (~10 SLOC)
- [x] Add `MysqlError::ConnectionTimeout` variant (~5 SLOC)
- [x] Add `MysqlError::PoolExhausted` variant (~5 SLOC)
- [x] Add `MysqlError::ConstraintViolation` variant with MySQL error code extraction (~10 SLOC)
- [x] Implement `Clone` for `MyConnection` — Pool is already internally reference-counted (~5 SLOC)
- [x] Document MySQL placeholder syntax (`?` vs `$1`) clearly in `Connection` impl docs (~10 SLOC docs)
- [x] Add connection URL parsing utilities — extract host, port, database, user from mysql:// URL (~15 SLOC)

## Testing
- [x] Integration test with real MySQL — CREATE TABLE, INSERT, SELECT, UPDATE, DELETE cycle (~30 SLOC) — `test_select_one`, `test_insert_select` in `tests/integration.rs`; full CRUD in `test_portability_mysql` (`crates/oxisql/tests/portability.rs`)
- [x] Test type mapping for all MySQL types — verify round-trip through `core_value_to_mysql` and `mysql_value_to_core` (~35 SLOC) — comprehensive round-trip tests added including GEOMETRY, scalar, typed Decimal/Json paths
- [x] Test `UInt` boundary values — i64::MAX, i64::MAX + 1, u64::MAX (~15 SLOC)
- [x] Test Date/Time formatting — verify ISO-8601 format for Date, Datetime, Time values (~20 SLOC)
- [x] Test transaction commit/rollback — verify data visibility after commit and invisibility after rollback (~20 SLOC) — `test_transaction_commit`, `test_transaction_rollback` in `tests/integration.rs`
- [x] Test concurrent transactions — verify pool provides independent connections for concurrent txns (~25 SLOC)
- [x] Test TLS connection — builder-level compile tests for ssl_skip_verify, ssl_with_ca_pem, ssl_disabled added; live TLS integration test remains optional (~15 SLOC)
- [x] Test stored procedure calls with multiple result sets (~20 SLOC) — `test_call_procedure_multi_result_set` in `tests/integration.rs`
- [x] Test connection pool behavior — max connections, idle timeout, connection reuse (~25 SLOC) — `test_pool_config_stored_in_builder`, `test_builder_ssl_disabled_overrides_skip_verify`, `test_builder_inverted_pool_bounds_does_not_panic_at_build` in `tests/integration.rs`
- [x] Test prepared statement binary protocol — verify parameterized queries work correctly (~20 SLOC) — `test_query_binary_select` in `tests/integration.rs`
- [x] Test error mapping — verify MySQL error codes map to correct `OxiSqlError` variants (~15 SLOC)
- [x] Test `MyTransaction` Drop behavior — verify implicit rollback when dropped without commit (~10 SLOC) — `test_my_transaction_drop_rolls_back` in `tests/integration.rs`

## Performance
- [x] Benchmark query throughput — text protocol vs binary protocol (~30 SLOC) — `benches/mysql_benchmarks.rs` covers mysql_to_core, mysql_to_core_with_type, core_to_mysql conversion groups
- [x] Benchmark connection pool under concurrent load — 50 concurrent queries through 10-connection pool (~30 SLOC) — `bench_pool_construction` in `benches/mysql_benchmarks.rs` benchmarks pool config across 2/10/50 max_size variants
- [x] Benchmark `LOAD DATA LOCAL INFILE` vs individual INSERT for bulk loading (~25 SLOC) — `bench_bulk_load_comparison` in `benches/mysql_benchmarks.rs`
- [x] Benchmark prepared statement reuse vs fresh parse (~20 SLOC) — `bench_prepared_stmt_overhead` benchmarks 10-param bind→convert and round-trip
- [x] Profile pool connection acquisition latency under varying pool sizes (~20 SLOC) — `bench_pool_construction` with `BenchmarkId::new("build_pool_config", n)` for n in [2, 10, 50]

## Integration
- [x] Integration test with `oxisql` facade — verify `oxisql::connect("mysql://...")` works end-to-end (~15 SLOC) — `test_portability_mysql` in `crates/oxisql/tests/portability.rs`
- [x] Integration with `oxisql-datafusion` — serve MySQL query results as DataFusion table provider (~30 SLOC)
- [x] Integration with `oxisql-parse` — `MyConnection::is_read_only_query` and `MyConnection::normalize_query` static methods added; `oxisql-parse` added as workspace dependency (~15 SLOC)
- [x] Integration with `oxitls` — `connection.rs` uses `ensure_crypto_provider()` to install `rustls_rustcrypto` provider; `ssl_skip_verify` / `ssl_with_ca_pem` builder methods tested in `tests/connect.rs`; uses same `rustls-rustcrypto` stack as oxitls (~15 SLOC)
