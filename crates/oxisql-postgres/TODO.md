# oxisql-postgres — TODO

**Status: Stable** · v0.3.2 · MSRV 1.89 · edition 2021 · Apache-2.0

Pure-Rust PostgreSQL backend over `tokio-postgres` (Frontend/Backend Protocol v3),
no `libpq` and no `openssl-sys`. `PgConnection` implements `oxisql_core::Connection`
over an `Arc<Mutex<Client>>`. TLS is routed through OxiTLS + `rustls` +
`rustls-rustcrypto` (no `ring`). COPY, LISTEN/NOTIFY, pipeline batching, and an
extended type mapping covering all 13 `Value` variants are implemented.

## Done

### Core protocol
- [x] `Connection` impl: `execute`, `query`, `execute_batch`, `ping`, `query_stream`
- [x] COPY bulk load/unload — `copy_in_text(table, cols, rows)` / `copy_out_text(table, cols)` (`src/copy.rs`)
- [x] LISTEN/NOTIFY — `listen(channel)` → `NotificationStream`, plus `notify(channel, payload)` (`src/notify.rs`)
- [x] Pipeline batching — `PgPipeline` with `add_execute` / `add_query` / `finish` (→ `PipelineResult`) in one round-trip (`src/pipeline.rs`)
- [x] Binary-format results — `query_binary` with explicit type OIDs (format code 1)
- [x] Prepared-statement caching — `PgPrepared` keyed by SQL-text hash
- [x] `describe(sql)` → `Vec<ColumnDescription>` (Parse + Describe, no Execute)
- [x] Schema introspection — `tables` / `columns` / `indexes` / `foreign_keys` via `information_schema` + `pg_indexes`
- [x] Automatic reconnection — `reconnect()` rebuilds from stored URI + TLS mode
- [x] Connection timeout — `connect_with_timeout(conn_str, tls, Duration)` → `PgError::Timeout`

### Transactions & types
- [x] `PgTransaction` with explicit commit/rollback; `Drop` schedules a `ROLLBACK` (no async Drop)
- [x] Savepoints — `savepoint` / `release_savepoint` / `rollback_to_savepoint` (+ name validation against injection)
- [x] Extended type mapping — BOOL, INT2/4/8, FLOAT4/8, TEXT/VARCHAR/BPCHAR, BYTEA, TIMESTAMP, TIMESTAMPTZ, DATE, TIME, UUID, JSONB, NUMERIC, ARRAY (all 13 `Value` variants round-trip)

### Connection setup & TLS
- [x] `connect(conn_str, tls)` accepts libpq `key=value` strings **and** `postgres://` / `postgresql://` URLs
- [x] `parse_pg_conn_str(s)` → `PgConnParts`
- [x] `PgConnectionBuilder` — `host` / `port` / `user` / `password` / `dbname` / `connect_timeout_secs` / `tls_mode` (and `tls_skip_verify` / `tls_with_ca_pem`) → `connect`
- [x] `from_client(client)` — wrap an existing `tokio_postgres::Client`
- [x] `TlsMode::skip_verify()` and `TlsMode::with_ca_pem(pem)` constructors (default to the RustCrypto provider)
- [x] `connect_skip_verify` / `connect_with_ca` convenience methods
- [x] Rich `PgError` (`ConstraintViolation`, `Timeout`, `PoolExhausted`, `Copy`, `Notify`, `Tls`, …)

### Ecosystem integration
- [x] `oxisql` facade — `oxisql::connect("postgres://…")` end-to-end
- [x] `oxisql-pool` — pooled access via `deadpool-postgres` (pooling lives in `oxisql-pool`, not duplicated here)
- [x] `oxisql-datafusion` — serve query results as a DataFusion table provider
- [x] `oxisql-parse` — `is_read_only_query` / `normalize_query` helpers
- [x] `oxitls` — `tls.rs` / `connection.rs` use `rustls_rustcrypto::provider()` + `oxitls::webpki_root_certs()`

## Roadmap / next

- [ ] **Optional `system` / libpq feature** — a future opt-in feature that could
      link against the system `libpq` for environments that require it. **This
      feature does not exist today** and is not on the default path; the default
      build is and will remain 100% Pure Rust (`tokio-postgres` + RustCrypto TLS).
- [x] Expose query cancellation through the `CancelRequest` flow at the
      `Connection` trait level (currently a query is cancelled by dropping its future).
- [ ] Logical replication / Streaming Replication Protocol support.
- [x] Native binary decoding for `ARRAY` element types beyond the current mapping.

## Known limitations

- Live-server integration tests are `#[ignore]`-gated (they need a real
  PostgreSQL instance): TLS live connect, COPY IN/OUT, the four LISTEN/NOTIFY
  tests, and the CRUD / isolation / reconnect / pooling integration suites. Run
  them with a server up and the `integration-postgres` feature; without one,
  `cargo test -p oxisql-postgres` reports 61 passed (including doctests) with
  these skipped.
- `LISTEN` notifications are unavailable on `from_client` connections (no
  background notification driver is spawned in that path).
- PostgreSQL protocol v2 (pre-7.4 servers) is not supported.
