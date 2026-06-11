# oxisql-mysql — TODO

**Status: Stable** · v0.1.2 (in development, branch `0.1.2`) · MSRV 1.89 · edition 2021 · Apache-2.0

Pure-Rust MySQL backend over `mysql_async`, no `libmysqlclient` and no
`openssl-sys`. `MyConnection` implements `oxisql_core::Connection` over an
internal `mysql_async::Pool` (internally synchronized — no extra `Mutex`). Every
query uses `prep()` + `exec()` on the binary protocol (server-side prepared
statements). TLS is provided by `rustls` + `rustls-rustcrypto` via a guarded
process-global `CryptoProvider` install (no `ring`).

## Done

### Core protocol
- [x] `Connection` impl: `execute`, `query`, `execute_batch`, `ping`, `query_stream`
- [x] Binary protocol everywhere — `prep()` + `exec()` / `exec_iter()` with server-side prepared-statement caching
- [x] `load_data_batched(table, cols, rows, batch_size)` — batched multi-row `INSERT` (binary protocol; no `LOCAL INFILE` server permission required)
- [x] `call_procedure_multi(name, params)` — stored procedures returning multiple result sets, via `exec_iter` + `QueryResult::next()`
- [x] Schema introspection — `tables` / `columns` / `indexes` / `foreign_keys` via `INFORMATION_SCHEMA`
- [x] Automatic-reconnect detection — `is_reconnect_error()` (CR_SERVER_GONE 2006, CR_SERVER_LOST 2013, ER_UNKNOWN_COM_ERROR 1047, I/O); not auto-applied mid-transaction for correctness
- [x] Connection timeout — `connect_timeout_secs` enforced via `tokio::time::timeout` at connect → `MysqlError::ConnectionTimeout`
- [x] `disconnect()` — drain all pool connections for graceful shutdown

### Transactions & types
- [x] `MyTransaction` over an owned `mysql_async::Transaction<'static>`; implicit rollback on drop without commit
- [x] Savepoints — `savepoint` / `release_savepoint` / `rollback_to_savepoint`
- [x] `last_insert_id()` on `MyTransaction` for auto-increment key retrieval
- [x] Type mapping — TINYINT(1)→Bool, TINYINT/SMALLINT/INT/BIGINT→I64, FLOAT/DOUBLE→F64, TEXT/VARCHAR/CHAR→Text, BLOB/BINARY/VARBINARY→Blob, DATETIME/TIMESTAMP→Timestamp, DATE→Date, TIME→Time, DECIMAL/NUMERIC→Decimal, JSON→Json (plus ENUM/SET→Text, GEOMETRY→Blob via WKB)
- [x] Unsigned overflow handled — values > `i64::MAX` fall back to `Value::Text` (no panic)

### Connection setup & TLS
- [x] `MyConnection::connect(url, tls)` for `mysql://user:pass@host:port/db`
- [x] `from_pool(pool)` — wrap an existing `mysql_async::Pool`
- [x] `MyConnectionBuilder` — `host` / `port` / `user` / `password` / `dbname` / `connect_timeout_secs` / `pool_min` / `pool_max` / `pool_idle_timeout` / `pool_ttl` → `connect`
- [x] TLS builder methods — `ssl_disabled` / `ssl_skip_verify` / `ssl_with_ca_pem` via `SslOpts`
- [x] `mysql_url_parts(url)` → `MysqlUrlParts` (`host` / `port` / `dbname` / `user`)
- [x] Rich `MysqlError` (`ConnectionTimeout`, `PoolExhausted`, `ConstraintViolation`, …)

### Ecosystem integration
- [x] `oxisql` facade — `oxisql::connect("mysql://…")` end-to-end
- [x] `oxisql-pool` — pooled access via a custom `deadpool` Manager over `mysql_async::Conn`
- [x] `oxisql-datafusion` — serve query results as a DataFusion table provider
- [x] `oxisql-parse` — `is_read_only_query` / `normalize_query` helpers
- [x] `oxitls` — `ensure_crypto_provider()` installs the `rustls_rustcrypto` provider (same stack as oxitls)

## Roadmap / next

- [ ] Accept a pre-built `rustls::ClientConfig` directly once `mysql_async`
      supports it (today it builds its own config from `SslOpts`, so
      `TlsMode::Rustls` is used only to install the process-global provider).
- [ ] Optional native `LOAD DATA LOCAL INFILE` path for callers that have the
      server permission and want maximal throughput beyond `load_data_batched`.
- [x] Surface MySQL warnings / multi-statement results beyond stored procedures.

## Known limitations

- Live-server integration tests are `#[ignore]`-gated (they require a real
  MySQL 8.x server): CRUD cycles, transaction commit/rollback, concurrent
  transactions, stored-procedure multi-result-sets, binary-protocol prepared
  statements, pool behaviour, and the live TLS connect test. Run them with a
  server up and the `integration-mysql` feature. Without one,
  `cargo test -p oxisql-mysql` reports 95 passed with the live tests skipped.
- Auto-reconnect is detected but deliberately not applied mid-transaction, to
  preserve transactional correctness.
- `TlsMode::Rustls(_)` installs the RustCrypto provider but does not pass a
  custom `ClientConfig` through to `mysql_async`; use the `ssl_*` builder
  methods for CA / verification control.
