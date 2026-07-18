# oxisql-postgres — TODO

**Status: Stable** · v0.4.0 · MSRV 1.89 · edition 2021 · Apache-2.0

Pure-Rust PostgreSQL backend over `tokio-postgres` (Frontend/Backend Protocol v3),
no `libpq` and no `openssl-sys`. `PgConnection` implements `oxisql_core::Connection`
over an `Arc<Mutex<Client>>`. TLS is routed through OxiTLS + `rustls` +
`rustls-rustcrypto` (no `ring`). COPY, LISTEN/NOTIFY, pipeline batching, and an
extended type mapping covering all 13 `Value` variants are implemented. Logical
replication (via `pgoutput`) is available behind the optional `replication` feature.

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
- [x] `server_version()` — raw `server_version` runtime parameter captured from
      the connection handshake's `ParameterStatus` message (`None` for
      `from_client` connections); covered by `tests/server_version.rs`
- [x] Rich `PgError` (`ConstraintViolation`, `Timeout`, `PoolExhausted`, `Copy`, `Notify`, `Tls`, …)

### Ecosystem integration
- [x] `oxisql` facade — `oxisql::connect("postgres://…")` end-to-end
- [x] `oxisql-pool` — pooled access via `deadpool-postgres` (pooling lives in `oxisql-pool`, not duplicated here)
- [x] `oxisql-datafusion` — serve query results as a DataFusion table provider
- [x] `oxisql-parse` — `is_read_only_query` / `normalize_query` helpers
- [x] `oxitls` — `tls.rs` / `connection.rs` use `rustls_rustcrypto::provider()` + `oxitls::webpki_root_certs()`

### Logical replication (`replication` feature)
- [x] `PgReplicationConnection::connect` — a standalone replication-mode handshake
      (`replication=database` startup param, TCP/TLS + full auth incl. SCRAM-SHA-256)
      over `MaybeTlsStream`, reusing the crate's existing `parse_pg_conn_str` connection
      string parser (`src/replication/auth.rs`, `src/replication/mod.rs`). Not routed
      through `tokio-postgres` — it cannot negotiate `CopyBoth`/replication mode, so this
      drives the wire protocol directly via `postgres-protocol` + `fallible-iterator`.
- [x] `identify_system` / `create_replication_slot` / `drop_replication_slot` —
      `IDENTIFY_SYSTEM` / `CREATE_REPLICATION_SLOT … LOGICAL pgoutput` /
      `DROP_REPLICATION_SLOT` over the simple-query protocol (`src/replication/commands.rs`)
- [x] `start_logical_replication` → `ReplicationStream` — `START_REPLICATION`,
      `CopyBoth`-mode streaming via a background reader task plus a periodic Standby
      Status Update keepalive task; `ack` / `standby_status_update` for progress
      acknowledgment (`src/replication/stream.rs`)
- [x] `pgoutput` v1 message decoding — Begin/Commit/Origin/Relation/Type/Insert/Update/
      Delete/Truncate/Message (`src/replication/pgoutput.rs`); `Lsn` WAL-position type
      (`src/replication/lsn.rs`); `CopyBoth`/`XLogData`/keepalive wire framing
      (`src/replication/copyboth.rs`)
- [x] `ReplicationStream::decode_tuple` — maps decoded tuple cells to `oxisql_core::Value`
      via the per-stream cached `Relation` schema; both text and binary
      wire formats, plus PostgreSQL array-literal text decoding
      (`src/replication/tuple/mod.rs`, tests in `tuple/tests.rs`)
- [x] 339 unit tests (`cargo test -p oxisql-postgres --features replication --lib`);
      `tests/replication.rs` live-server integration suite (`identify_system` happy path,
      slot create/drop, full INSERT/UPDATE/DELETE round trip with tuple decoding, TRUNCATE,
      reconnect-and-resume from an acked LSN) — gated `integration-postgres,replication`,
      each test additionally `#[ignore]`d pending a real `wal_level=logical` server
- [x] `oxisql` facade re-exports — `oxisql::postgres::{PgReplicationConnection, …}` behind
      the facade's `postgres-replication` feature (`postgres` + `oxisql-postgres/replication`)

## Roadmap / next

- [ ] **Optional `system` / libpq feature** — a future opt-in feature that could
      link against the system `libpq` for environments that require it. **This
      feature does not exist today** and is not on the default path; the default
      build is and will remain 100% Pure Rust (`tokio-postgres` + RustCrypto TLS).
- [x] Expose query cancellation through the `CancelRequest` flow at the
      `Connection` trait level (currently a query is cancelled by dropping its future).
- [x] Logical replication / Streaming Replication Protocol support — MVP shipped via
      the `pgoutput` plugin, gated behind the `replication` feature. See "Logical
      replication" under Done above for the full module breakdown. Follow-ups deferred
      out of this MVP's scope:
  - [ ] **Streaming support for large in-progress transactions** (`proto_version` 2
        with `streaming 'on'`) — near-must-have for production use (today a single
        large transaction only arrives as one giant burst at COMMIT); deferred here
        only for scope, not because it is optional long-term.
  - [x] **Binary-format tuples** — `pgoutput`'s `binary 'true'` negotiation;
        `TupleColumn::Binary` already recognizes the wire shape structurally but does
        not decode it into typed values. (planned 2026-07-12) (done 2026-07-12)
    - **Goal:** Decode `TupleColumn::Binary(Bytes)` cells in `replication/tuple.rs` into typed `oxisql_core::Value`s instead of rejecting them, mirroring `crate::types::extract_value`'s OID→Value conventions (bool/int2/int4/int8/float4/float8/text/bytea/date/timestamp/timestamptz/time/uuid/json/jsonb/numeric/interval, plus binary arrays), via `<T as FromSql>::from_sql(&Type, raw)` since there is no live `Row` to call `try_get` on.
    - **Design:** New `binary_to_value(type_oid: u32, raw: &[u8]) -> Result<Value, PgError>` called from `decode_cell`'s `TupleColumn::Binary(raw)` arm. NUMERIC/INTERVAL byte decoders are re-derived locally (not shared with `types.rs`, per this module's documented text/binary separation policy). JSONB binary strips its leading `0x01` version byte before UTF-8 decode. Binary arrays decode via `Vec<Option<T>>::from_sql` into `Value::TypedArray`.
    - **Files:** `crates/oxisql-postgres/src/replication/tuple.rs` only.
    - **Prerequisites:** none (all deps already present: tokio-postgres FromSql/Type/Kind, bytes, time, uuid).
    - **Tests:** inline unit tests per scalar type + binary arrays in tuple.rs; update the existing `cell_binary_not_yet_supported` test to assert the new decoded value instead of an error.
    - **Risk:** JSONB version-byte handling; keep `tuple.rs` under 2000 lines (currently 1105).
  - [ ] **Two-phase commit** (`proto_version` 3) — `Begin Prepare` / `Prepare` /
        `Commit Prepared` / `Rollback Prepared` messages.
  - [ ] **Parallel streaming** (`proto_version` 4) — concurrent streaming of a single
        large transaction across multiple workers.
  - [ ] **Physical replication** — explicitly out of scope: a different protocol and
        use case (byte-for-byte WAL shipping) from logical/`pgoutput` decoding.
  - [x] **Array-typed column text-decoding** in `tuple.rs` — PostgreSQL's array-literal
        text syntax (`{...}`, with its own quoting/escaping rules) is not yet parsed;
        array-typed columns are currently rejected with `PgError::TypeConversion`. (planned 2026-07-12) (done 2026-07-12)
    - **Goal:** Decode PostgreSQL array-literal text format (e.g. `{1,2,3}`, `{}`, `{NULL,2}`, quoted/escaped elements, nested multi-dimensional arrays, optional `[l:u]=` dimension prefix) in `text_to_value` instead of rejecting `Kind::Array`.
    - **Design:** New `decode_text_array(elem_ty: &Type, s: &str) -> Result<Value, PgError>` replacing the reject at the `Kind::Array` branch. A real brace/quote/escape-aware scanner (not naive `split(',')`). Each leaf element recurses through `text_to_value(elem_ty.oid(), elem)` to reuse all existing scalar parsers. 1-D known-element-type arrays become `Value::TypedArray`; multi-dimensional or unknown-element arrays become nested `Value::Array`.
    - **Files:** `crates/oxisql-postgres/src/replication/tuple.rs` only.
    - **Prerequisites:** none.
    - **Tests:** inline unit tests covering `{1,2,3}`, `{}`, `{NULL,2}`, quoted/escaped elements, nested arrays, dimension-prefixed arrays, malformed literals (unbalanced braces → error not panic); update the existing `array_type_rejected` test to assert the new decoded value instead of an error.
    - **Risk:** array-literal parsing correctness (quoting/escaping/nesting) is the hardest part of this item.
- [x] Native binary decoding for `ARRAY` element types beyond the current mapping.

## Known limitations

- Live-server integration tests are `#[ignore]`-gated (they need a real
  PostgreSQL instance): TLS live connect, COPY IN/OUT, the four LISTEN/NOTIFY
  tests, and (behind `integration-postgres`) a 30-test CRUD / isolation /
  reconnect / pooling / prepared-statement / type-mapping suite, plus (behind
  `integration-postgres,replication`) the 5-test `tests/replication.rs` logical
  replication suite (needs `wal_level=logical`). Without any extra features,
  `cargo test -p oxisql-postgres` reports 68 passed (including 15 doctests)
  with the base 6 live-server tests skipped; `--all-features` compiles in all
  41 `#[ignore]`d tests.
- `LISTEN` notifications are unavailable on `from_client` connections (no
  background notification driver is spawned in that path).
- PostgreSQL protocol v2 (pre-7.4 servers) is not supported.
