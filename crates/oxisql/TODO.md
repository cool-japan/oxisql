# oxisql (facade) — TODO

## Status

**Stable** · version 0.1.2 (in development, branch 0.1.2) · **80 tests**
(`--all-features`; 2 `#[ignore]`d are live-server-gated portability tests).

Unified Pure-Rust SQL facade: a single URI string selects the backend and the
caller receives a `Box<dyn Connection>`. Dispatches to embedded (GlueSQL),
persistent embedded (redb / fjall / sled), Postgres, MySQL, SQLite-compat
(oxisqlite — C-free fork of limbo), and DataFusion (OLAP). Re-exports the core
trait/type surface from `oxisql-core`.

## Done

### Connection entry points
- [x] `connect(uri) -> Box<dyn Connection>` with URI-based backend dispatch
- [x] `connect_or_create(uri)` — connect, creating the DB if absent (embedded always succeeds)
- [x] `connect_pooled(uri, max) -> Box<dyn ConnectionPool>` — type-erased pool
- [x] `connect_pool(uri, max) -> OxidbPool` — typed pool enum for backend-specific access
- [x] `connect_with_options(uri, ConnectOptions)` — timeout / pool size / TLS / auto-reconnect / statement cache
- [x] `connect_with_tls(uri, tls_cfg)` — explicit `rustls::ClientConfig` without backend imports
- [x] `connect_datafusion(uri) -> OxiSqlContext` — OLAP entry point; `connect()` returns a clear `UnsupportedUri` error pointing here
- [x] `ConnectOptions` builder — `new().timeout_ms(_).pool_size(_).require_tls(_)`

### Helpers and introspection
- [x] `ping(conn)` / `close(conn)` — backend-agnostic liveness and teardown
- [x] `introspect(conn) -> Vec<TableInfo>` — schema snapshot from any backend
- [x] `version() -> &'static str`
- [x] `backend_info_for_uri(uri) -> Option<BackendInfo>` — backend identity without connecting
- [x] `BackendInfo { name, version, features }`

### Middleware and fan-out
- [x] `MultiConnection` — fan a query out to several connections in parallel (`src/multi.rs`)
- [x] Facade-level `LoggingConnection` labelled logging wrapper (`src/logging.rs`)
- [x] Retry middleware re-export (`RetryConnection`, `RetryPolicy`, `RetryPredicate`)
- [x] Metrics middleware re-export (`MetricsConnection`, `MetricsSnapshot`)

### Re-exports and ergonomics
- [x] `prelude` module — traits, `Value`, `Row`, `RowSet`, `FromValue`, `ToSqlValue`, errors, schema + middleware types
- [x] Crate-root re-exports of all common `oxisql-core` types
- [x] Feature-gated module re-exports: `postgres`, `mysql`, `datafusion`, `pool`, `migrate`
- [x] Feature-gated connection types: `EmbeddedConnection`, `RedbEmbeddedConnection`, `FjallEmbeddedConnection`, `SledEmbeddedConnection`, `SqliteConnection`
- [x] `#[must_use]` on `connect` return type
- [x] Named-parameter support (`:name` / `$name` / `@name`) inherited from `oxisql-core` default methods on every backend

### REPL
- [x] `oxisql-repl` binary behind `repl` feature; connects to any URI (default `memory://`)
- [x] Dot commands: `.help`, `.tables`, `.schema <table>`, `.quit`
- [x] Multi-line SQL accumulation; tabular display for `SELECT`/`WITH`/`EXPLAIN`; row count for other statements; pipe-mode prompt suppression

### Testing
- [x] `connect("memory://")` end-to-end CRUD round-trip
- [x] Unknown / feature-disabled schemes return `UnsupportedUri` (`ftp://`, empty URI, `file://`, `datafusion://`)
- [x] `connect_or_create` — embedded success, unknown-scheme error, full CRUD
- [x] Facade re-export accessibility test (compile-time feature-gated symbol checks)
- [x] `connect_with_tls` through the facade
- [x] Transactions through `Box<dyn Connection>` (BEGIN / COMMIT / ROLLBACK)
- [x] DataFusion register-table + query through `oxisql::datafusion`
- [x] Migration runner up/down against the embedded backend
- [x] `LoggingConnection` execute/query/ping/label/into_inner
- [x] Live-server portability tests for Postgres / MySQL (`tests/portability.rs`, `#[ignore]`d)

### Performance
- [x] Facade dispatch-overhead benchmark (`Box<dyn Connection>` vs direct backend)
- [x] Per-backend connection-establishment benchmark
- [x] Pooled vs unpooled throughput benchmark

## Roadmap / next
- [ ] `system` feature for opt-in libpq legacy parity (Pure Rust stays the default)
- [x] Auto-create for Postgres / MySQL in `connect_or_create` (issue `CREATE DATABASE` when missing) (planned 2026-06-15)
  - **Goal:** `oxisql::connect_or_create("postgres://…/appdb")` (and `mysql://…/appdb`) creates `appdb` if absent, then connects — matching the create-if-absent semantics embedded backends already have.
  - **Design:** In `crates/oxisql/src/lib.rs::connect_or_create` (currently a pass-through to `connect`), for `postgres`/`mysql` schemes: attempt `connect`; if it fails with "database does not exist" (PG SQLSTATE `3D000`; MySQL error 1049 `ER_BAD_DB_ERROR`), parse the target db name from the URI, build a maintenance URI (PG → dbname `postgres`; MySQL → no database), connect, run `CREATE DATABASE "<name>"` with proper identifier quoting (PG double-quote, MySQL backtick; reject/escape embedded quotes — no injection), then connect to the original target. `CREATE DATABASE` runs outside a transaction (PG) via a simple/autocommit query. Embedded/sqlite behavior unchanged. Add pure helpers (`split_db_name`, `maintenance_uri`, `create_database_stmt`) unit-testable without a server.
  - **Files:** `crates/oxisql/src/lib.rs`; tests in `crates/oxisql/tests/` (new `auto_create.rs` or extend `connect.rs`).
  - **Tests:** unit — `pg_maintenance_uri`, `mysql_maintenance_uri`, `create_database_stmt_quotes_identifier`, `create_database_stmt_rejects_bad_identifier`, `split_db_name_*`. Integration (live server, `#[ignore]`-gated like the crate's existing pattern): `auto_create_pg_creates_then_connects`, `auto_create_mysql_creates_then_connects`.
  - **Risk:** portable "db missing" detection across driver error types; `CREATE DATABASE` privilege + no-transaction constraint. Match the specific SQLSTATE/error code; document the privilege requirement. Live tests stay `#[ignore]`d; unit tests cover all server-independent logic.
- [x] Connection-string query-parameter parsing (e.g. `?sslmode=require&pool_max=8`) folded into `ConnectOptions` (done 2026-06-10)
  - **Goal:** A URI like `sqlite://path?pool_max=8&connect_timeout=5` auto-configures the matching `ConnectOptions` fields; unknown keys are collected into an `extra` map.
  - **Design:** In `src/lib.rs`, after parsing the scheme/host/path, split the query string (`?key=val&…`) and match recognized keys (`sslmode`, `pool_max`, `connect_timeout`, `application_name`, …) onto the `ConnectOptions` builder. Unknown keys go into `ConnectOptions::extra: HashMap<String,String>`. No new runtime deps — pure string splitting.
  - **Files:** `src/lib.rs`, `tests/connect.rs`
  - **Tests:** `query_string_multi_param`, `query_string_unknown_key`, `query_string_empty`, `query_string_sslmode_require`, `query_string_application_name` — all pass.
- [x] Backend health/readiness surfaced through `BackendInfo` after handshake (populate `version`) (done 2026-06-10)
  - **Goal:** `BackendInfo.version` is non-empty for embedded/sqlite backends after connection; PG/MySQL stays `None`/`"unknown"` (live-handshake-gated, deferred).
  - **Design:** `BackendInfo::postgres()` and `::mysql()` now explicitly document `version: None` with a `// TODO: populate from server handshake` comment; `::sqlite_compat()` reports `"oxisqlite 0.1.0"` (the OxiSQLite engine). All embedded variants already used `env!("CARGO_PKG_VERSION")`.
  - **Files:** `src/lib.rs`, `tests/connect.rs`
  - **Tests:** `backend_info_embedded_has_version`, `backend_info_postgres_version_is_none`, `backend_info_mysql_version_is_none`, `backend_info_redb_has_version`, `backend_info_fjall_has_version`, `backend_info_sqlite_compat_has_version` — all pass.
- [x] Richer REPL: history, output formats (CSV / JSON), `.timer`, `.read <file>` (done 2026-06-10)
  - **Goal:** REPL gains in-memory history (up/down), `.mode table|csv|json`, `.timer on|off`, and `.read <file>` dot-commands alongside existing `.help/.tables/.schema/.quit`.
  - **Design:** `Mode` enum (`Table, Csv, Json`); hand-rolled CSV/JSON formatters; `ReplState` tracks mode + timer flag + `Vec<String>` history; `.history` dot-command prints it; `.read <file>` submits each non-empty, non-comment line as SQL; `.timer on|off` wraps `Instant::now()` around query execution. All under existing `repl` feature; no new deps.
  - **Files:** `src/bin/repl.rs`
  - **Tests:** `csv_format_rows`, `csv_format_comma_in_field`, `csv_format_empty`, `json_format_rows`, `json_format_empty`, `json_format_null_value`, `json_format_escaped_string`, `dot_read_tempfile`, `timer_toggle`, `mode_from_str_valid`, `mode_from_str_invalid`, `history_accumulates`, `history_skips_blank_lines` — all pass.

## Known limitations
None at the facade level. The facade is a thin, stable dispatch layer; backend
caveats (e.g. SQLite-compat ROLLBACK / savepoints, live-server-gated Postgres /
MySQL tests) live in the respective backend crates and the workspace README,
not here.

## Planned — oxisqlite engine ANALYZE + splitrs (2026-06-16)

- [x] **Slice 1: ANALYZE writes `sqlite_stat1`** (planned 2026-06-16)
  - **Goal:** `ANALYZE`, `ANALYZE main`, `ANALYZE <table>`, `ANALYZE <index>` create `sqlite_stat1` (if absent), clear the relevant prior rows, and write `(tbl, idx, stat)` rows where `stat = "N a1 a2 … ak"`. A no-index table yields `(tbl, NULL, "N")`; an empty table yields no row. Re-ANALYZE replaces, never duplicates.
  - **Files:** `crates/oxisqlite-core/vdbe/insn.rs`, `vdbe/explain.rs`, `vdbe/execute.rs`, `storage/btree.rs`, `translate/analyze.rs` (new), `translate/mod.rs`; new `tests/analyze.rs` + `[[test]]` in `Cargo.toml`

- [x] **Slice 2: Load `sqlite_stat1` + feed real stats into the System-R cost model** (planned 2026-06-16)
  - **Goal:** After ANALYZE, the optimizer uses real per-table row counts and index selectivity instead of `ESTIMATED_HARDCODED_ROWS_PER_TABLE = 1_000_000`. Un-analyzed DBs are bit-for-bit unchanged.
  - **Files:** `crates/oxisqlite-core/statistics.rs` (new), `schema.rs`, `util.rs`, `lib.rs`, `vdbe/execute.rs`, `translate/optimizer/{mod,cost,access_method,constraints,join}.rs`

- [x] **Slice 3: `splitrs` split `schema.rs` (2022 lines → under 2000)** (planned 2026-06-16)
  - **Goal:** `schema.rs` and every product module < 2000 lines; public API unchanged; all tests + clippy green.
  - **Files:** `crates/oxisqlite-core/schema.rs` → new `schema/` module tree

- [x] **Slice 4: `splitrs` split `vdbe/execute.rs` (8467 lines → under 2000)** (planned 2026-06-16)
  - **Goal:** `vdbe/execute.rs` and every product module < 2000 lines; op_* handler dispatch intact.
  - **Files:** `crates/oxisqlite-core/vdbe/execute.rs` → new `vdbe/execute/` module tree

- [x] **Slice 5: `splitrs` split `storage/btree.rs` (8864 lines → under 2000)** (done 2026-06-19)
  - **Goal:** `storage/btree.rs` and every product module < 2000 lines; BTreeCursor API intact.
  - **Files:** `crates/oxisqlite-core/storage/btree.rs` → `btree/` module tree (6 files, all under 2000 ln except tests.rs at 2028 ln). 636 tests pass, 0 warnings.
