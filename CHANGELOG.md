# Changelog

All notable changes to OxiSQL will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-06-23

### Added

- **`FlamegraphProfiler` benchmark utility** (`oxisqlite-core`): new `benches/common/profiler.rs`
  implements a custom Criterion 0.8-compatible `Profiler` backed by `pprof`. It emits a
  `flamegraph.svg` into each benchmark's output directory when run with `--profile-time`. This
  avoids the criterion version conflict introduced by `pprof`'s bundled `PProfProfiler` (still
  pinned to criterion 0.5).

### Changed

- **`arrow` pinned to `58.3.0`** (workspace): downgraded from `59.0.0` to match the version
  DataFusion 54 re-exports; `arrow 59` has no compatible DataFusion release and
  `oxistore-columnar` also pins `58.3.0`.
- **`oxistore-columnar` updated to `0.2.0`** (workspace): pulls in the latest columnar
  Parquet-backend release.
- **`as_any()` overrides removed** (`oxisql-datafusion`): `TableProvider` and `ExecutionPlan`
  impls in `parquet.rs`, `provider.rs`, and `stream.rs` no longer override `as_any()` — the
  method was removed from both traits in DataFusion 54, so the overrides were dead code.

### Fixed

- **`rand` 0.9 API compatibility** (`oxisqlite-core` btree tests): replaced deprecated
  `Rng::gen()` with `RngExt::random()` in the B-Tree stress test.

## [0.3.0] - 2026-06-22

### Removed

- **`objc2-system-configuration` removed from default dependency closure** (`oxisqlite-core`): The
  macOS `SCDynamicStore` C binding previously pulled in transitively by `whoami`'s `std` feature
  has been excised. The default `cargo build --workspace` is now 100 % `objc2`-free on macOS.

### Added

- **`whoami-patched` vendored crate** (`crates/whoami-patched/`): Pure-Rust patch of `whoami`
  2.1.2 that drops `objc2-system-configuration` from the macOS code path; wired in via
  `[patch.crates-io]` in the workspace `Cargo.toml`. Not published to crates.io (vendored only).

### Changed

- **`oxisqlite-core` default I/O backend is now pure-Rust generic**: The native epoll/kqueue
  event-loop is now gated behind the `native-io` feature (opt-in). The `load-extension` feature
  (which pulls `libloading`) is likewise opt-in. Default builds remain 100 % C-free.
- **`oxitls` dependency bumped to `^0.2.0`**: Resolves the `PENDING-REPUBLISH` dependency block
  now that `oxitls 0.2.0` has been published to crates.io.

### Security

- Clears `PENDING-REPUBLISH` status: `objc2-system-configuration` (a macOS C/ObjC binding) no
  longer appears in the `--all-features` dependency closure. COOLJAPAN Pure Rust Policy v2 §3
  Role-A compliance restored for the macOS target.

## [0.2.1] - 2026-06-20

### Added

#### WITHOUT ROWID table support (`oxisqlite-core`)
- `CREATE TABLE … WITHOUT ROWID` now fully supported: uses an index-format B-Tree where the PRIMARY KEY columns are the B-Tree key and the full row is stored as the record payload.
- `Index::synthetic_for_without_rowid(table)` in `schema/index.rs` builds a synthetic index object (PK columns + all table columns) used to open cursors as `CursorType::BTreeIndex` with `has_rowid = false`.
- `translate_create_table` in `translate/schema.rs` detects `WITHOUT ROWID` and emits `CreateBtree` with `CreateBTreeFlags::new_index()` instead of `new_table()` — the pager initialises the root page as an index-leaf page.
- `validate_without_rowid_table` enforces: (1) an explicit PRIMARY KEY is present; (2) the PK column(s) occupy the first declared positions — required for correct B-Tree key comparison.
- `translate_insert_without_rowid` in `translate/insert.rs`: dedicated INSERT code path for WITHOUT ROWID tables; opens the cursor as `BTreeIndex`, populates all column registers, enforces NOT NULL on PK columns, emits `NoConflict` for the PK uniqueness check, supports `OR IGNORE` (skip) and `OR REPLACE` (delete + re-insert), then writes via `MakeRecord` + `IdxInsert`; multi-row (`VALUES(…),(…)`) and `INSERT … SELECT` use the standard coroutine path.
- `translate/plan.rs` updated: for WITHOUT ROWID tables without an explicit index hint, `CursorType::BTreeIndex(synthetic)` is allocated automatically so that `SELECT` / full-scans use the correct B-Tree page format.
- `crates/oxisqlite-core/tests/without_rowid.rs` — 397-line integration test suite (registered in `Cargo.toml` as the `without_rowid` test target) covering: CREATE success/failure, basic INSERT + SELECT round-trip, PK NOT NULL enforcement, PK uniqueness (ABORT / IGNORE / REPLACE), multi-row INSERT, text PK, composite PK, validation of missing PK, and validation of PK-column-not-first.

#### `BorrowedValue<'a>` — zero-allocation borrowed view of SQL values (`oxisql-core`)
- New `BorrowedValue<'a>` enum in `oxisql-core` provides a lifetime-parametric mirror of `Value` where `Text`, `Blob`, `Json`, and `Decimal` borrow from existing storage instead of owning heap allocations; all scalar variants (`Null`, `Bool`, `I64`, `F64`, `Timestamp`, `Date`, `Time`, `Uuid`) are copied inline.
- `BorrowedValue::to_owned(&self) -> Value` converts back to an owned `Value` by cloning borrowed bytes.
- `From<&'a Value> for BorrowedValue<'a>` allows zero-cost borrowing of any `Value`; `Array` / `TypedArray` fall back to `Null` (documented limitation, callers iterate `elems` manually).
- `BorrowedValue` implements `Debug`, `Clone`, `PartialEq`, `Display` (UUID formatted as `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`), and `type_name() -> &'static str`.
- Re-exported from `oxisql-core` root (`pub use value::{ArrayElementType, BorrowedValue, Value}`).
- 15 unit tests in `borrowed_value_tests` module covering: type names, is_null, text/blob zero-allocation round-trips, scalar round-trips, `From<&Value>` for all variants, Display output, and full owned round-trip.

### Fixed
- `INSERT` into WITHOUT ROWID tables previously returned `"INSERT into WITHOUT ROWID table is not supported"` at parse time; now correctly routed to the index-format insert path.
- `CHECK_automatic_pk_index_required` no longer returns an unsupported error for WITHOUT ROWID tables; instead returns `Ok(None)` (no separate auto-index needed — the table IS the index).

[0.2.1]: https://github.com/cool-japan/oxisql/releases/tag/v0.2.1

## [0.2.0] - 2026-06-17

### Added

#### ANALYZE statement (`oxisqlite-core`)
- Full `ANALYZE [target]` statement support that writes cardinality statistics to `sqlite_stat1` (`CREATE TABLE sqlite_stat1(tbl,idx,stat)`).
- `translate_analyze` in `translate/analyze.rs` generates bytecode that: creates `sqlite_stat1` if absent, clears prior rows for the target, walks each table/index b-tree via the new `Insn::IdxStat` opcode, inserts fresh `(tbl, idx, stat)` rows, bumps the schema cookie, and re-parses the schema.
- `ANALYZE` — bare, `ANALYZE main`, `ANALYZE <table>`, `ANALYZE <index>` — all forms supported with correct `ClearMode` semantics.
- `Insn::IdxStat { cursor_id, num_cols, dest }` opcode in `vdbe/insn.rs`; `op_idx_stat` handler in `vdbe/execute/txn_schema.rs` walks the b-tree and writes `"N a1 … ak"` statistics strings (NULL for empty tables/indexes so inserts are skipped).
- 6 integration tests in `crates/oxisqlite-core/tests/analyze.rs` covering: row-count write, empty-table skip, re-analyze replaces stale rows, named-table targeting, error on unknown table, and query-correctness check via the in-memory stats side-map.

#### System-R optimizer with real ANALYZE statistics (`oxisqlite-core`)
- `SchemaStats` side-map (`statistics.rs`) — in-memory mirror of `sqlite_stat1`, loaded after schema parsing; exposes `num_rows(table)` and `index_stats(table, index)`.
- `parse_stat1_line` utility parses SQLite's `"N a1 … ak"` format (tolerates trailing non-integer tokens such as `unordered`); 8 unit tests inline.
- `Schema` gains a `stats: SchemaStats` field; `load_persistent_stats()` on `Connection` populates it by scanning `sqlite_stat1` after the schema is loaded — completely backwards compatible (empty map preserves the old hardcoded-estimate code path).
- `estimate_cost_for_scan_or_seek` in `translate/optimizer/cost.rs` updated to accept `base_row_count: f64` and `index_stats: Option<&[i64]>`; when stats are present the equality-prefix selectivity is derived from `avg_rows_per_distinct / base_row_count` instead of the per-column selectivity product, giving the System-R planner real cardinality estimates.
- `optimize_table_access` passes `schema.stats` down through `constraints_from_where_clause` to the cost estimator; databases without an ANALYZE run are unaffected (stats are `None` → hardcoded estimates preserved bit-for-bit).
- `db_tests` module in `statistics.rs` provides an end-to-end proof that `ANALYZE` populates `conn.schema.stats` with the correct row count.

#### `application_id` and `synchronous` pragmas (`oxisqlite-core`)
- `PRAGMA application_id [= N]` — read and set the 32-bit application-ID header field (cookie offset 68); new `Cookie::ApplicationId` variant in `vdbe/insn.rs`.
- `PRAGMA synchronous [= N]` — read/set WAL synchronous-mode flag; now registered in the pragma table.

#### Schema module split via splitrs (`oxisqlite-core`)
- `schema.rs` (1 920 lines) replaced by a 7-module sub-tree: `schema/mod.rs`, `schema/bootstrap.rs`, `schema/column.rs`, `schema/container.rs`, `schema/index.rs`, `schema/table.rs`, `schema/tests.rs`. All public types re-exported from `schema/mod.rs`; no API breakage.

#### VDBE execute module split via splitrs (`oxisqlite-core`)
- `vdbe/execute.rs` (8 361 lines) replaced by a 10-module sub-tree: `execute/mod.rs`, `execute/aggregate.rs`, `execute/arith_logic.rs`, `execute/cursor.rs`, `execute/function.rs`, `execute/mutate.rs`, `execute/numeric.rs`, `execute/txn_schema.rs`, `execute/values.rs`, `execute/tests.rs`.
- `values.rs` consolidates all `Value::exec_*` methods (`exec_lower`, `exec_upper`, `exec_length`, `exec_octet_length`, `exec_sign`, `exec_soundex`, regex operations, math functions, etc.) as inherent `Value` methods — previously scattered inline across `execute.rs`.
- `txn_schema.rs` consolidates transaction, savepoint, cookie, checkpoint, `ParseSchema`, `IntegrityCheck`, and the new `op_idx_stat` opcode handlers.

#### UPSERT `ON CONFLICT DO UPDATE` (`oxisqlite-core`)
- `translate/upsert.rs` — `emit_upsert_do_update` helper extracted from `translate/insert.rs` to keep both files under the 2000-line workspace policy.
- Integration tests in `crates/oxisqlite-core/tests/upsert.rs` and `crates/oxisql-sqlite-compat/tests/` for UPSERT and schema versioning scenarios.

#### Conflict-clause handling (`oxisqlite-core`)
- 5 integration tests in `crates/oxisqlite-core/tests/conflict.rs`: `INSERT OR FAIL`, `INSERT OR ABORT`, `INSERT OR ROLLBACK`, `INSERT OR IGNORE`, and the default-ABORT behaviour.

#### Correlated sub-query tests (`oxisqlite-core`)
- 19 integration tests in `crates/oxisqlite-core/tests/correlated.rs` covering scalar, `EXISTS`, `NOT EXISTS`, `IN`, `NOT IN`, nested, and multi-subquery patterns including an arithmetic-context regression.

#### Durability & WAL tests (`oxisqlite-core`)
- `crates/oxisqlite-core/tests/durability.rs` — file-backed durability tests exercising WAL commit/crash-recovery.

#### Schema-cookie and LIMIT/params tests (`oxisqlite-core`)
- `crates/oxisqlite-core/tests/schema_cookie.rs` — schema-cookie bump and reprepare lifecycle tests.
- `crates/oxisqlite-core/tests/limit_params.rs` — LIMIT/OFFSET with bound parameters.

#### `CREATE INDEX IF NOT EXISTS` (`oxisqlite-core`)
- `translate_create_index` now respects the `IF NOT EXISTS` flag: silently succeeds when the index already exists rather than raising a parse error.

#### Schema-change cookie emission (`oxisqlite-core`)
- `program.emit_schema_change()` added after DDL operations in `translate/alter.rs` (ADD COLUMN, RENAME COLUMN, RENAME TABLE, DROP COLUMN), `translate/index.rs` (CREATE INDEX, DROP INDEX), and `translate/analyze.rs` — ensures the schema cookie is bumped and cached compiled statements are invalidated consistently.

#### Transparent schema re-prepare in statement cache (`oxisql-sqlite-compat`)
- `exec_rewritten` in `oxisql-sqlite-compat/src/connection.rs` now catches `SchemaChanged` errors from the engine, discards the stale compiled program, re-prepares against the refreshed schema, and retries exactly once — replacing the fragile `is_ddl` keyword-prefix heuristic that failed on comment-prefixed DDL and left DML statements stale after schema changes.
- `schema_reprepare.rs` test suite (`crates/oxisql-sqlite-compat/tests/schema_reprepare.rs`) with tests for: comment-prefixed DDL replay, DML reuse after schema change, `CREATE INDEX` invalidation, and `ALTER TABLE` invalidation.

#### `connect_or_create` — auto-create missing databases (`oxisql`)
- New `connect_or_create(uri)` façade function connects to the target URI; when the database does not yet exist on a wire-protocol backend (PostgreSQL/MySQL) it issues `CREATE DATABASE <name>`, then connects to the freshly created database.
- `split_db_name` helper parses any `scheme://authority/db?query` URI into `(authority, db_name)`.
- `CreateScheme` enum classifies `postgres://` / `postgresql://` vs `mysql://` schemes.
- Integration tests in `crates/oxisql/tests/auto_create.rs` (ignored by default; require a running server).

#### Blocking connection API (`oxisql-sqlite-compat`)
- `BlockingSqliteConnection` — synchronous (non-async) wrapper around `SqliteConnection` via a single-threaded `tokio` runtime; exposes `execute`, `query`, `begin`, `commit`, `rollback`.
- `blocking.rs` test suite covering basic CRUD, transaction commit/rollback, and multi-row queries.

#### Orphaned WAL protection (`oxisqlite-core`)
- `maybe_init_database_file` now returns `bool` indicating whether the database file was freshly created; `Database::open_file` passes this flag to `WalFileShared::open_shared_inner`.
- `WalFileShared::open_shared_inner` discards (truncates) any pre-existing WAL when the main database file was freshly created, preventing stale WAL frames from a previous database incarnation from being replayed.

#### WAL header refresh on open (`oxisqlite-core`)
- `conn.pager.refresh_header_from_wal()` called during `Database::open_*` after WAL recovery to ensure the in-memory header reflects the latest committed cookie values (e.g. `application_id`, `user_version`) that may have been committed to the WAL without a checkpoint.

#### `checkpoint_truncate` API (`oxisqlite-core`)
- `Connection::checkpoint_truncate()` exposes a TRUNCATE-mode WAL checkpoint, resetting the WAL file to empty.
- `Connection::close()` is now idempotent (guarded by a `closed: Cell<bool>` flag).

### Changed
- **Version bump 0.1.2 → 0.2.0** across the entire workspace (`[workspace.package].version` in root `Cargo.toml`); all intra-workspace dependency version strings updated accordingly.
- `Schema` struct gains a `stats: SchemaStats` field (default-constructed; zero-cost when ANALYZE has never run).
- `optimize_table_access` signature changed from `available_indexes: &HashMap<…>` to `schema: &Schema` to pass statistics through to the cost estimator.
- `estimate_cost_for_scan_or_seek` signature extended with `base_row_count: f64` and `index_stats: Option<&[i64]>` parameters; all callers updated.
- `Transaction { write }` instruction now carries `schema_cookie` for correct cookie-mismatch detection in `BEGIN IMMEDIATE` / `EXCLUSIVE`.

### Fixed
- `CREATE INDEX … IF NOT EXISTS` no longer raises a parse error when the index already exists.
- `ALTER TABLE` (ADD COLUMN, RENAME COLUMN, RENAME TABLE, DROP COLUMN) and `CREATE/DROP INDEX` now correctly bump the schema cookie, preventing stale cached statements from being reused across DDL boundaries.
- Comment-prefixed DDL statements (e.g. `/* migration 0001 */ CREATE TABLE …`) no longer silently corrupt the statement cache — the new SchemaChanged-based re-prepare path handles them correctly.
- Opening a new database file alongside an orphaned `-wal` file no longer replays stale WAL frames from a previous database; the orphaned WAL is discarded and a fresh WAL is started.
- WAL-committed `PRAGMA application_id` / `PRAGMA user_version` changes are now visible immediately after open (previously required a checkpoint to become visible via `PRAGMA` reads).

---

## [0.1.2] - 2026-06-10

### Added

#### C-free `oxisqlite-*` engine fork (Wave 1)
- Replaced the C-pulling `limbo` dependency with a 7-crate pure-Rust fork of limbo 0.0.22
  (`oxisqlite`, `oxisqlite-core`, `oxisqlite-ext`, `oxisqlite-macros`,
  `oxisqlite-sqlite3-parser`, `oxisqlite-time`, `oxisqlite-uuid`).
- Removed all 3 C touchpoints: `mimalloc` allocator, `lemon.c` parser generator,
  and `built`/`git2` build-info crates.
- `CC=/usr/bin/false cargo build --workspace` → exit 0 (C-free proven).
- Inline pure-Rust Julian-day helper in `oxisqlite-core` (replaces GPL-licensed
  `julian_day_converter`).

#### Full-transaction ROLLBACK support (Wave 2, `oxisql-sqlite-compat`)
- `BEGIN / INSERT / ROLLBACK` now correctly discards changes; `COMMIT` persists them.
- WAL integrity preserved. Ported rollback machinery from `turso_core` 0.7.0-pre.5 (MIT).
- New `oxisql-sqlite-compat/tests/rollback.rs` (5 tests), `savepoint.rs`,
  `change_counts.rs`, `type_mapping.rs`, `rollback_error.rs` (updated).

#### TLS security patch (Wave 3)
- Vendored `rustls-rustcrypto-patched` crate fixes RUSTSEC-2026-0104 (CRL-parsing panic
  in `rustls-webpki 0.102.x`) via `[patch.crates-io]`.
- Root `NOTICE` file created recording full fork lineage.
- `deny.toml` allowlist extended: Zlib, Unicode-3.0, MPL-2.0, CDLA-Permissive-2.0.

#### Query cancellation (`oxisql-postgres`)
- `PostgresCancelToken` — cancel a running query without closing the connection.
- `PgConnection::cancel_token()` returns a token usable from any async context.
- New `PgError::ConnectionError` variant for connection-level failures.
- `TypedArray` replaces raw array handling in `Value` for richer type representation.

#### Advisory migration locking (`oxisql-migrate`)
- `MigrationLock` trait with `NoopMigrationLock` and `PostgresAdvisoryLock`
  implementations — prevents concurrent schema migrations.
- Migration `rechecksum` support and `--recheck-hash` CLI flag.
- Migration directives: `-- oxi:no-tx`, `-- oxi:skip-if-exists`, `-- oxi:require-version`.
- `lock.rs` module and `tracker_generic.rs` for backend-agnostic migration tracking.

#### SQL optimizer enhancements (`oxisql-parse`)
- `decorrelate.rs` — correlated-subquery decorrelation pass.
- `explain.rs` — `EXPLAIN`-compatible query plan renderer.
- `optimizer/cse.rs` — Common Sub-expression Elimination (CSE) pass.
- `optimizer/join_reorder.rs` — cost-based join reordering (842 lines).
- `optimizer/simplify.rs` — constant folding and predicate simplification (833 lines).
- `parameterize.rs` — SQL literal parameterization for LRU plan cache.
- `plan_cache.rs` — `PlanCache` struct with schema-invalidation for repeated queries.
- `planner.rs` — extended logical planner with new plan node types.

#### DataFusion bridge improvements (`oxisql-datafusion`)
- `plan_bridge.rs` — structural lowering of `Filter` and `Project` nodes.
- `stream.rs` — async streaming rowset adapter refactored and extended.
- New tests: `plan_bridge_structural.rs` (503 lines), `pushdown_extra.rs` (335 lines),
  `query_provider.rs` (47 lines).
- TPC-H benchmark queries (Q3, Q5–Q9, Q19) added under `crates/perf/tpc-h/`.

#### Connection options (`oxisql-core` / `oxisql-postgres`)
- `ConnectOptions` now parses query-string parameters from connection URIs
  (`application_name`, `sslmode`, extra KV pairs).
- `BackendInfo` documents that server versions for PostgreSQL/MySQL are not known
  until after the connection handshake; SQLite-compat backend reports a static version.
- `Middleware` trait and `LoggingConnection`/`MetricsConnection`/`RetryConnection`
  wrappers added to `oxisql-core`.
- `Warning` type for server-side diagnostic messages (MySQL warning forwarding).

#### SQLite-compat type mapping (`oxisql-sqlite-compat`)
- Columns typed `DATE`, `TIMESTAMP`, `TIME`, `UUID` are now mapped to rich `Value`
  variants; plain TEXT/INTEGER columns are not false-retyped.
- `change_count()` on connection reflects `changes()` from the underlying engine.

### Changed
- `oxisql-sqlite-compat` dependency changed from `limbo` to in-tree `oxisqlite`
  workspace path (`crates/oxisqlite`).
- `oxisql-pool/sqlite_rusqlite.rs` removed; `sqlite_compat.rs` extended in its place.
- `cfg_block` dependency dropped (unused).

### Fixed
- Removed `unsafe transmute` in `const_concat_slices` macro — replaced with safe
  const-generic array construction.
- `oxisql-embedded` `memory_complex.rs` integration test — added missing `NULL`
  assertion edge cases.
- LEMON parser template file (`lemon.c`-generated artefact) removed from tree.

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

[0.3.1]: https://github.com/cool-japan/oxisql/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/cool-japan/oxisql/releases/tag/v0.3.0
[0.2.1]: https://github.com/cool-japan/oxisql/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/cool-japan/oxisql/compare/v0.1.2...v0.2.0
[0.1.2]: https://github.com/cool-japan/oxisql/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/cool-japan/oxisql/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/cool-japan/oxisql/releases/tag/v0.1.0
