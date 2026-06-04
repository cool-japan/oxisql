# OxiSQL TODO — v0.1.1

Last updated: 2026-06-04

## Released: 0.1.1 (2026-06-04)

10 crates fully implemented. 924 tests pass (64 skipped). Zero clippy warnings. Zero production stubs.
~33,902 lines of Rust across 145 source files, 202+ public API items.
Pure Rust: zero C/FFI in default build — all `*-sys` crates are feature-gated.

Second ultra pass (2026-05-31): named parameters (`execute_named`/`query_named`) across all
backends via `oxisql-core` default trait methods; `EmbeddedConnection` schema introspection
(tables/columns/indexes/foreign_keys) via GlueSQL catalog; honest ROLLBACK error in
`oxisql-sqlite-compat`; `OxiSqlError::Params` variant; 128-slot LRU statement cache
infrastructure in `oxisql-sqlite-compat`. 37 additional tests (852 → 889).

Third ultra pass (2026-06-03): CSV import/export for embedded connections
(`import_csv`/`export_table_to_csv` on `EmbeddedConnection`, pure-Rust RFC 4180 CSV
state-machine parser in `oxisql-embedded/src/csv.rs`); interactive SQL REPL binary
(`oxisql-repl` behind `repl` feature in `oxisql` crate, `.help`/`.tables`/`.schema`
commands, multi-line SQL accumulation, terminal-detection for pipe mode); workspace
dependency version fix (`oxistore-columnar` 0.1.1 → 0.1.0 to match crates.io).

Milestones derived from `../phase3/oxisql_blueprint.md` section Phased milestones.

## Milestones

- [x] **M0** — Skeleton. Workspace, `deny.toml`, `Dockerfile.ffi-audit`,
  `scripts/ffi-audit.sh`, `oxisql-core` traits + error enum, empty facade.
  - Gate: `cargo tree` shows zero `*-sys` in the default closure.
- [x] **M1** — Embedded. `oxisql-embedded` with gluesql wired against
  MemoryStorage; `oxisql-parse` (sqlparser facade) operational; basic
  `connect("memory://")` works.
- [x] **M2** — Postgres. `oxisql-postgres` with `tokio-postgres` + OxiTLS
  (rustcrypto). End-to-end query + transaction + prepared statement against a
  Postgres test container.
- [x] **M3** — `oxisql-mysql` (tokio + mysql_async + OxiTLS) (done 2026-05-25)
- [x] **M3 (Wave 38A)** — `oxisql-sqlite-compat`: pure-Rust SQLite backend via Limbo 0.0.22.
  `SqliteConnection`, `SqliteTransaction`, `SqlitePrepared`, full `Connection` trait impl,
  schema introspection via `sqlite_master`+`PRAGMA table_info`, `$N→?` param rewriter,
  `SqliteCompatPool` in `oxisql-pool` (feature: `sqlite-compat`), `sqlite://` wired in
  `oxisql` facade (feature: `sqlite`). 26 tests pass, 2 ignored (ROLLBACK pending limbo
  upstream). (done 2026-05-27)
- [x] **M4** — `oxisql-datafusion` (DataFusion TableProvider over oxisql Connection) (done 2026-05-25)
- [x] **M5** — `oxisql-pool` (deadpool + custom mysql Manager) + `oxisql-migrate` (sqlparser, sqlx-style filenames) (done 2026-05-25)

## Architecture
```
oxisql (facade)
  +-- oxisql-core         (traits: Connection, Transaction, Row, Value, OxiSqlError)
  +-- oxisql-parse           (SQL parsing via sqlparser, future query planner)
  +-- oxisql-embedded      (GlueSQL in-memory engine)
  +-- oxisql-postgres      (tokio-postgres wire protocol client)
  +-- oxisql-mysql          (mysql_async wire protocol client)
  +-- oxisql-datafusion     (DataFusion TableProvider bridge)
```

## Cross-Cutting Priorities

### P0 — Connection Pooling
- [x] Add `ConnectionPool` trait to `oxisql-core` with `get_conn()`, `pool_size()`, `idle_count()`
- [x] Implement pool for `oxisql-postgres` — bounded pool of `tokio_postgres::Client` instances with health checks
- [x] `oxisql-mysql` already uses `mysql_async::Pool` internally — expose pool configuration options
- [x] Add pool support at facade level — `oxisql::connect_pooled(uri, config)`

### P1 — Prepared Statements
- [x] Add `PreparedStatement` type to `oxisql-core` with `execute(params)` and `query(params)`
- [x] Implement for Postgres — cache `tokio_postgres::Statement` by SQL text hash
- [x] Implement for MySQL — use `mysql_async::Conn::prep` for server-side prepared statements
- [x] Implement for embedded — GlueSQL does not natively support prepared statements, use AST caching

### P2 — Extended Type System
- [x] Add `Value::Decimal`, `Value::Timestamp`, `Value::Date`, `Value::Time`, `Value::Uuid`, `Value::Json`, `Value::Array` variants to `oxisql-core`
- [x] Add `FromValue` trait for type-safe row value extraction
- [x] Extend Postgres type mapping — DATE, TIMESTAMP, TIMESTAMPTZ, UUID, JSONB, NUMERIC, ARRAY
- [x] Extend MySQL type mapping — DECIMAL, DATETIME(6), JSON, ENUM
- [x] Extend embedded type mapping — map GlueSQL Date/Time/Uuid/Decimal variants to new Value types

### P3 — Schema Introspection and Migration
- [x] Add `SchemaInspector` trait — `tables()`, `columns(table)`, `indexes(table)`, `foreign_keys(table)`
- [x] Implement for Postgres via `information_schema` / `pg_catalog` queries
- [x] Implement for MySQL via `INFORMATION_SCHEMA` queries
- [x] Implement for embedded via GlueSQL metadata APIs
- [x] Add migration engine — `Migrator` trait with `apply`, `rollback`, `status`, `pending`

### P4 — Query Builder and SQL Analysis
- [x] Build fluent query builder in `oxisql-parse` — `QueryBuilder::select().from().where_eq().join().build()` in `builder.rs`
- [x] Add SQL query planner — transform AST into logical plan (Scan, Filter, Project, Join, Aggregate) in `oxisql-parse/src/lib.rs`
- [x] Add query optimizer — predicate pushdown, projection pruning, constant folding, join reordering in `oxisql-parse/src/optimizer/`
- [x] Add join algorithms — hash join, merge join, nested-loop join in `optimizer/join_algo.rs` (`JoinAlgorithmPass`)

### P5 — DataFusion Live Streaming
- [x] Replace snapshot-based `OxiSqlTableProvider` with live `OxiSqlStreamProvider` driving real connections — `stream.rs`
- [x] Implement filter pushdown — translate DataFusion Expr to SQL WHERE clauses — `stream.rs`
- [x] Implement projection pushdown — generate SELECT with only requested columns — `stream.rs`
- [x] Implement limit and sort pushdown — `stream.rs`
- [x] Add multi-table catalog registration for cross-table DataFusion queries — `OxiSqlContext` in `context.rs`

### P6 — Persistent Embedded Database
- [x] Implement GlueSQL `Store` trait backed by `oxistore-kv-fjall` — `FjallGlueStorage` in `crates/oxisql-embedded/src/fjall_storage.rs`; `FjallEmbeddedConnection` in `src/lib.rs`; feature gate `fjall-storage`; 213 tests green
- [x] Implement GlueSQL `Store` trait backed by `oxistore-kv-redb` — `RedbGlueStorage` in `crates/oxisql-embedded/src/redb_storage.rs` (redb 4.1.0, two `&[u8]→&[u8]` tables, order-preserving binary key encoding for all `Key` variants, auto-increment persisted in redb); `RedbEmbeddedConnection` in `src/lib.rs` (`redb_conn` module); feature gate `redb-storage`; 6 new tests; 213 tests green
- [x] Add WAL (Write-Ahead Log) mode for crash recovery — fjall uses an LSM journal (write-ahead log) for crash safety by default; redb is ACID with crash-safe B-trees; no additional implementation required
- [x] Add `EmbeddedConnection::open_file(path)` for durable embedded SQL — `FjallEmbeddedConnection::open(path)` and `RedbEmbeddedConnection::open(path)` provide persistent connection constructors
- [x] Fix M1 param substitution — replace string replacement with AST-level parameter binding — `oxisql-embedded/src/params.rs` (`bind_params` uses `sqlparser` AST walk with string fallback)

### P7 — Wire Protocol Enhancements
- [x] Postgres: `COPY` protocol for bulk data ingestion — `src/copy.rs`
- [x] Postgres: `LISTEN/NOTIFY` for real-time event subscription — `src/notify.rs`
- [x] Postgres: pipeline mode for batched queries in a single round-trip — `src/pipeline.rs`
- [x] MySQL: `LOAD DATA LOCAL INFILE` for bulk ingestion — implemented as `load_data_batched` in `oxisql-mysql/src/connection.rs`
- [x] MySQL: multi-result-set support for stored procedures — `call_procedure_multi` in `oxisql-mysql/src/connection.rs`
- [x] MySQL: binary protocol for all queries — `Connection::query` now delegates to explicit `prep()+exec()` via `query_internal`; `query_binary` is an alias

### P8 — Observability
- [x] Query logging middleware with timing — `oxisql/src/logging.rs` (`LoggingConnection`)
- [x] Query retry middleware for transient failures — `oxisql-core/src/middleware.rs` (`RetryConnection`, `RetryPolicy`, `RetryPredicate`)
- [x] Connection pool statistics (acquired, released, timeouts, active) — `PoolMetrics` gains `acquired_total: u64`, `released_total: u64`, `timeout_count: u64`; `EmbeddedPool` tracks via `Arc<AtomicU64>` atomics: `acquired` incremented on both inherent and trait `get()` paths, `released` incremented via `EmbeddedPool::checkin()` + `on_checkin` hook; `active`/`idle` already tracked; postgres/mysql/sqlite: `0` (deadpool exposes no cumulative counters)
- [x] Backend info reporting (version, feature set) — `BackendInfo` struct + `backend_info_for_uri` in `oxisql/src/lib.rs`

## Testing Priorities
- [x] Cross-backend portability tests — `crates/oxisql/tests/portability.rs`
- [x] Transaction isolation tests across all backends
- [x] Type round-trip tests — Value -> SQL param -> DB -> Result -> Value for all types and backends — `crates/oxisql/tests/type_roundtrip.rs`
- [x] Concurrent query stress tests for pooled connections
- [x] TLS connection tests with oxitls for Postgres and MySQL — compile-time builder tests in `crates/oxisql-postgres/tests/connect.rs` and `crates/oxisql-mysql/tests/connect.rs`; `#[ignore]` live-server stub in postgres tests
- [x] DataFusion integration tests — register tables from all backends, execute OLAP queries — `crates/oxisql/tests/datafusion_facade.rs`

## Subcrate TODOs
See individual TODO.md files in each crate directory:
- `crates/oxisql-core/TODO.md`
- `crates/oxisql-parse/TODO.md`
- `crates/oxisql-embedded/TODO.md`
- `crates/oxisql-postgres/TODO.md`
- `crates/oxisql-mysql/TODO.md`
- `crates/oxisql-datafusion/TODO.md`
- `crates/oxisql/TODO.md`

## Open Items (blocked on upstream)

These items cannot be resolved without upstream changes in Limbo:

- [~] **oxisql-sqlite-compat: ROLLBACK support** — Limbo 0.0.22 does not
  implement transaction ROLLBACK. Two tests are `#[ignore]`d. `rollback()` now
  returns a clear, honest `Err(OxiSqlError::Other("ROLLBACK is not supported …"))`
  instead of a raw parse error (Phase E done). Full rollback will unblock when
  Limbo merges the relevant PR.
- [ ] **oxisql-sqlite-compat: Savepoints** — No savepoint API in Limbo 0.0.22.
  Requires Limbo 0.1+ API stabilisation.
- [~] **oxisql-sqlite-compat: Named parameters & prepared-statement cache** —
  Statement-cache infrastructure (LRU, capacity 128, keyed by rewritten SQL) is
  now in place (Phase B done). Full parse-skip optimisation blocked on limbo
  fixing `Statement::reset()` to clear `Program::n_change`.
  [x] Named-parameter translation (`:name`, `$name`, `@name` → positional `$N`)
  is now implemented at the `oxisql-core` layer via `Connection::execute_named`
  and `Connection::query_named` default methods — all backends inherit this
  with zero per-backend code (Phase C done). SQLite-compat driver-level named
  params still blocked on Limbo 0.1+ API stabilisation.
- [ ] **oxisql-sqlite-compat: PRAGMA foreign_key_list** — FK metadata currently
  retrieved via DDL parsing of `sqlite_master` because PRAGMA is not yet
  supported by Limbo 0.0.22.
  - **BLOCKED: Limbo 0.0.22 does not implement PRAGMA commands; unblocks when Limbo 0.1+ is released**

## Open Questions

1. **limbo cutoff.** What is the minimum limbo version we ship in M3, and what
   is the documented SQLite-feature support matrix at that pin? Do we wait for
   limbo v0.1, or ship earlier with a feature-gap manifest?
2. **gluesql dialect exposure.** Do we expose gluesql's full non-standard SQL
   surface (more capability, less portability) or restrict the facade to a
   documented standard subset?
3. **DataFusion as hard or soft dep.** Should `oxisql-datafusion` be a hard
   sub-crate or live in a separate `oxisql-datafusion-ext` repository to keep
   facade compile times low?
4. **Pool implementation.** Wrap `deadpool` (already Pure, already mature) or
   implement an OxiSQL-native pool to avoid the dependency? Trade-off: rolling
   our own buys API freedom, costs maintenance.
5. **Migration tool format compatibility.** Do we target sqlx-migration
   filename/directory format for ecosystem familiarity, or define a COOLJAPAN-
   native format with its own runner?
