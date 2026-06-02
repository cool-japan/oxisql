# oxisql-postgres TODO

## Status
Pure-Rust PostgreSQL backend over `tokio-postgres` (no `libpq`). `PgConnection` with `Connection` impl using `Arc<Mutex<Client>>`. TLS via `rustls` + `rustls-rustcrypto` (no `ring`) through oxitls. `PgTransaction` with owned mutex guard and explicit commit/rollback (no async Drop). Type mapping covers BOOL, INT2/4/8, FLOAT4/8, TEXT/VARCHAR/BPCHAR/NAME, BYTEA, NULL. Custom `PgError` and `PgTls` wrapper types. ~446 SLOC across 4 files (connection.rs, error.rs, tls.rs, types.rs).

## Core Implementation
- [x] Add connection pooling — implemented via `oxisql-pool::postgres::OxidbPgPool` using `deadpool-postgres`; `oxisql-postgres` intentionally does not duplicate pooling logic (~120 SLOC)
- [x] Add prepared statement caching — `PgPreparedStatement` wrapping `tokio_postgres::Statement` with automatic cache keyed by SQL text hash (~50 SLOC)
- [x] Add `COPY` protocol support — `copy_in(table, columns, rows)` for bulk data ingestion — `src/copy.rs` with `copy_in_text` / `copy_out_text`
- [x] Add `LISTEN/NOTIFY` support — `listen(channel)` returning an async `Stream<Notification>` for real-time event subscription — `src/notify.rs`
- [x] Add binary format support — `PgConnection::query_binary` uses `tokio_postgres::Client::query_typed` which requests binary result encoding (format code 1) from the server; parameter types declared explicitly (BOOL/INT8/FLOAT8/TEXT/BYTEA/UNKNOWN) (~50 SLOC)
- [x] Add `pg_catalog` query support — schema introspection via `information_schema.tables`, `information_schema.columns`, `pg_indexes` (~60 SLOC)
- [x] Add pipeline mode — batch multiple queries into a single round-trip via `PgPipeline` — `src/pipeline.rs`
- [x] Add extended type mapping — DATE, TIME, TIMESTAMP, TIMESTAMPTZ, INTERVAL, UUID, JSONB, NUMERIC, ARRAY types to `Value` variants (~80 SLOC)
- [x] Add `NUMERIC/DECIMAL` type support — map to `Value::Decimal(BigDecimal)` for exact decimal arithmetic (~25 SLOC)
- [x] Add `UUID` type support — map to `Value::Uuid(u128)` or `Value::Text` with UUID formatting (~15 SLOC)
- [x] Add `JSONB` type support — map to `Value::Json(String)` with json extraction utilities (~20 SLOC)
- [x] Add `ARRAY` type support — map Postgres array columns (`INT[]`, `TEXT[]`, etc.) to `Value::Array(Vec<Value>)` via binary array decoding; currently returns `Value::Text("<opaque:int4>")` for array OIDs (~30 SLOC)
- [x] Add `TIMESTAMP/TIMESTAMPTZ` support — map to `Value::Timestamp(i64)` with timezone handling (~25 SLOC)
- [x] Add `INTERVAL` type support — map to `Value::Text` or a custom interval representation (~15 SLOC)
- [x] Add `execute_batch` — use `Client::batch_execute` for multi-statement execution (~10 SLOC)
- [x] Add `ping` — use `Client::simple_query("")` or a lightweight probe (~10 SLOC)
- [x] Add automatic reconnection — `reconnect_uri` + `reconnect_tls` stored on `PgConnection`; `reconnect()` returns a fresh connection from stored URI/TLS (~40 SLOC)
- [x] Add connection timeout configuration — `connect_with_timeout(uri, tls, Duration)` wraps `connect` with `tokio::time::timeout`, returns `PgError::Timeout` on expiry (~20 SLOC)
- [x] Fix async Drop issue in `PgTransaction` — `guard` is now `Option<OwnedMutexGuard<…>>`; `Drop` schedules `ROLLBACK` via `tokio::runtime::Handle::try_current().spawn(…)` if transaction was not explicitly terminated (~30 SLOC)
- [x] Add savepoint support — `savepoint`/`rollback_to_savepoint`/`release_savepoint` on `Transaction` trait; `savepoint_pg`/`rollback_to_savepoint_pg`/`release_savepoint_pg` inherent methods returning `PgError`; `validate_savepoint_name` prevents SQL injection (~25 SLOC)
- [x] Add SSL certificate verification options — `TlsMode::skip_verify()` (NoCertVerifier) and `TlsMode::with_ca_pem(pem)` (custom CA) constructors; `PgConnection::connect_skip_verify` and `PgConnection::connect_with_ca` convenience methods (~80 SLOC)
- [x] Add `row_description` for column type introspection without executing the query — `describe(sql)` returning `Vec<ColumnDescription>` via `client.prepare()` (~30 SLOC)

## API Improvements
- [x] Add `PgConnectionBuilder` — builder pattern for connection string, TLS mode, pool size, timeouts (~40 SLOC)
- [x] Add `PgConnection::from_client(client)` — construct from pre-existing `tokio_postgres::Client` (~10 SLOC)
- [x] Add `PgError::ConstraintViolation` variant with constraint name extraction (~10 SLOC)
- [x] Add `PgError::Timeout` variant (~5 SLOC)
- [x] Add `PgError::PoolExhausted` variant (~5 SLOC)
- [x] Implement `Clone` for `PgConnection` — the inner `Arc<Mutex<Client>>` already supports this (~5 SLOC)
- [x] Document Postgres wire protocol version (v3) compliance and limitations — `# Wire protocol compliance` section in `src/lib.rs` crate-level docs covering extended-query/simple-query/binary-format/pipeline/describe paths and known limitations (~30 SLOC docs)
- [x] Add connection string parsing utilities — extract host, port, dbname, user from conn string (~20 SLOC)

## Testing
- [x] Integration test with real Postgres — CREATE TABLE, INSERT, SELECT, UPDATE, DELETE cycle (~30 SLOC)
- [x] Test prepared statement reuse — verify same SQL text returns cached statement (~20 SLOC)
- [x] Test extended types — DATE, TIMESTAMP, UUID, JSONB, NUMERIC round-trips (`test_extended_type_round_trip`) (~30 SLOC)
- [x] Test transaction isolation — verify uncommitted changes are invisible to other connections using two separate connections (~25 SLOC)
- [x] Test error mapping — verify invalid SQL maps to `OxiSqlError::Execution` (~20 SLOC)
- [x] Test reconnect method — `test_reconnect_method` verifies `reconnect()` produces a working new connection (~15 SLOC)
- [x] Test `describe()` method — `test_describe_query` verifies column names and type names (~15 SLOC)
- [x] Test `LISTEN/NOTIFY` — verify notifications are received on the correct channel (~25 SLOC) — `test_listen_notify` in `tests/notify.rs`
- [x] Test `COPY` protocol — bulk insert 10k rows and verify count (~20 SLOC) — `test_copy_in_and_out` in `tests/copy.rs`
- [x] Test TLS connection via oxitls — compile-time builder tests (`tls_skip_verify`, `tls_with_ca_pem`) in `tests/connect.rs`; live-server portion covered by `#[ignore]` `test_pg_tls_live_connection` stub (~35 SLOC)
- [x] Test `PgTransaction` Drop behavior — verify connection is usable after transaction drop without commit (~15 SLOC) — `test_pg_transaction_drop_rolls_back` in `tests/integration.rs` (`#[ignore]`, requires live PG)
- [x] Test connection pooling under load — 100 concurrent queries through a 10-connection pool (~25 SLOC)
- [x] Test automatic reconnection after server restart (~20 SLOC)
- [x] Test type mapping for all supported Postgres types — ARRAY round-trips (~40 SLOC) — `test_int_array`, `test_text_array`, `test_float_array`, `test_null_array` in `tests/types.rs`

## Performance
- [x] Benchmark query throughput — simple SELECT, parameterized SELECT, JOIN queries (~40 SLOC)
- [x] Benchmark binary vs text format encoding/decoding — `postgres_benchmarks.rs` benchmarks `value_to_param` for I64, Text, F64, Bool, and Null variants (~35 SLOC, no live server required)
- [x] Benchmark connection pool vs single connection under concurrent workload (~30 SLOC)
- [x] Benchmark COPY protocol vs individual INSERT for bulk loading (~25 SLOC)
- [x] Profile `Arc<Mutex<Client>>` contention under high concurrency (~20 SLOC)
- [x] Benchmark prepared statement cache hit rate under realistic workloads (~20 SLOC)

## Integration
- [x] Integration test with `oxisql` facade — verify `oxisql::connect("postgres://...")` works end-to-end (~15 SLOC) — `test_portability_postgres` in `crates/oxisql/tests/portability.rs`
- [x] Integration with `oxisql-datafusion` — serve Postgres query results as DataFusion table provider (~30 SLOC)
- [x] Integration with `oxitls` — `tls.rs` and `connection.rs` use `rustls_rustcrypto::provider()` + `oxitls::webpki_root_certs()`; `PgConnectionBuilder::tls_skip_verify` / `tls_with_ca_pem` added for API symmetry with MySQL; `TlsMode::Rustls` accepts any `Arc<ClientConfig>` built from oxitls (~15 SLOC)
- [x] Integration with `oxisql-parse` — `PgConnection::is_read_only_query` and `PgConnection::normalize_query` static methods added; `oxisql-parse` added as workspace dependency (~15 SLOC)
