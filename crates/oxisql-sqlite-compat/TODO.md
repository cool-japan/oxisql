# oxisql-sqlite-compat — TODO

## Status: Alpha (0.4.0)

Pure-Rust SQLite-compatible backend over the C-free **`oxisqlite`** engine (a
COOLJAPAN fork of limbo 0.0.22 with all C/C++ dependencies stripped). Implements
`oxisql_core::Connection` / `Transaction` / `PreparedStatement`. No `libsqlite3`, no
C/C++. `limbo` appears only as historical fork lineage; the live dependency is
`oxisqlite`, which OxiSQL owns and maintains.

**Tests: 85 pass, 0 ignored** with default features (`cargo nextest run`); **94
pass, 0 ignored** with `cargo nextest run --all-features` — the `blocking` feature
adds 9 (5 pre-existing in `tests/blocking.rs` + 4 new in
`tests/open_from_bytes_blocking.rs`). `test_foreign_keys_basic` — formerly the
crate's one ignored test — now passes: `foreign_keys()` is backed by the engine's
native `PRAGMA foreign_key_list`.

## Done

- [x] Create crate over the `oxisqlite` engine (C-free fork of limbo 0.0.22)
- [x] `error.rs` — `SqliteCompatError` mapping to `OxiSqlError`
- [x] `types.rs` — value conversion + quote-aware `$N → ?` parameter rewriter
- [x] `connection.rs` — `SqliteConnection`, `SqliteTransaction`, `SqlitePrepared`
- [x] `Connection` trait impl (`execute`, `query`, `transaction`, `execute_batch`, `ping`, `prepare`, `tables`, `columns`, `indexes`, `foreign_keys`, `query_stream`)
- [x] Integration tests (CRUD, transactions, schema introspection, file persistence)
- [x] `SqliteCompatPool` added to `oxisql-pool` (`sqlite` feature; `sqlite-compat` alias)
- [x] Wired into the `oxisql` facade (`sqlite` feature)
- [x] **ROLLBACK fully supported** — ported from `turso_core` 0.7.0-pre.5 (MIT). `SqliteTransaction::rollback()` discards all pending changes; `BEGIN`/`INSERT`/`ROLLBACK` leaves 0 rows; `COMMIT` persists; WAL integrity preserved; `rollback()` also fires on drop as a safety net. Covered by 5 tests in `tests/rollback.rs`.
- [x] Named parameters (`$name` / `:name` / `@name`) handled at the `oxisql-core` layer via `execute_named` / `query_named`, rewriting to positional `?` before the engine
- [x] `execute_batch` via a token-aware state-machine split (handles `;` inside literals, identifiers, and comments)
- [x] `foreign_keys()` via the engine's native `PRAGMA foreign_key_list` (superseded the original `sqlite_master` DDL-parsing approach — see Roadmap below)
- [x] Statement cache (LRU, 128 slots, keyed by rewritten SQL) — **active** for every DML/DDL statement, with a transparent re-prepare-and-retry on `SchemaChanged` (see Roadmap below)
- [x] DataFusion registration for SQLite tables
- [x] `blocking` feature — synchronous wrappers (`SqliteConnectionBlocking`, `SqliteBlockingTransaction`, `SqliteBlockingPrepared`) in `src/blocking.rs`, each driving the async API via a fresh `current_thread` Tokio runtime per call (since 0.2.0)
- [x] **`open_from_bytes`** (0.3.3) — `SqliteConnection::open_from_bytes(bytes: &[u8])` (async, `src/connection.rs`) and `SqliteConnectionBlocking::open_from_bytes(bytes: &[u8])` (sync, `blocking` feature, `src/blocking.rs`) open a database directly from an in-memory byte buffer — e.g. `include_bytes!`, `VACUUM INTO`, or `sqlite3_serialize()` output — with no temporary file, enabling WASI/browser/read-only-filesystem use. Mirrors SQLite's `sqlite3_deserialize()`; malformed input (too short, wrong magic, invalid page size) returns a typed error and never panics. 5 tests in `tests/open_from_bytes.rs` (not feature-gated) + 4 in `tests/open_from_bytes_blocking.rs` (`blocking` feature).

## Roadmap / next

All of the following are **OxiSQL-owned `oxisqlite` engine work** — we maintain the
engine, so these are our roadmap items rather than external blockers.

- [x] **Savepoints** — implemented `SAVEPOINT`/`RELEASE`/`ROLLBACK TO SAVEPOINT` in `oxisqlite`, reachable by issuing that SQL text through `execute()` (verified: 8 tests in `tests/savepoint.rs`, including nested savepoints and autocommit-mode `RELEASE`). (planned 2026-06-10 — see root TODO.md) **Note:** the dedicated `oxisql_core::Transaction::savepoint()` / `release_savepoint()` / `rollback_to_savepoint()` trait methods are *not* overridden on `SqliteTransaction` and still return the default "not supported by this backend" error — only the raw-SQL path works today; wiring the trait methods through remains open.
- [x] **Statement-cache activation** — fix Statement::reset() n_change, switch exec_rewritten to cached path + native change count. (planned 2026-06-10 — see root TODO.md)
- [x] **PRAGMA foreign_key_list** — add native FK metadata to oxisqlite engine + schema; rewrite foreign_keys() to use the pragma; un-ignore test_foreign_keys_basic. (planned 2026-06-10 — see root TODO.md)
- [x] **Richer type mapping** — extend limbo_to_core with declared-type hints for DATE/TIMESTAMP/TIME/UUID columns. (planned 2026-06-10 — see root TODO.md)
- [x] **Native affected-row counts** — read n_change natively after engine fix; drop SELECT changes() round-trip. (planned 2026-06-10 — part of statement-cache activation work)

## Known limitations

- `SqliteTransaction`'s dedicated `savepoint()` / `release_savepoint()` / `rollback_to_savepoint()` trait methods are not overridden and still return "not supported by this backend"; the equivalent SQL (`SAVEPOINT` / `RELEASE` / `ROLLBACK TO SAVEPOINT` via `execute()`) works correctly, including nested savepoints. (SQL-level support DONE 2026-06-10; trait-method wiring still open)
- Foreign-key metadata now comes from the engine's native `PRAGMA foreign_key_list`, not `sqlite_master` DDL parsing. (DONE 2026-06-10)
- The statement cache is active (LRU, 128 slots) with a transparent re-prepare-and-retry on `SchemaChanged`. (DONE 2026-06-10)
- Date/time and UUID values get dedicated `Value::Date` / `Value::Timestamp` / `Value::Time` / `Value::Uuid` variants when the column has a matching declared SQL type; columns without one still return generic `TEXT` / `INTEGER` mapping. (Declared-type mapping DONE 2026-06-10)
- Affected-row counts are read natively via `conn.changes()` — no `SELECT changes()` SQL round-trip. (DONE 2026-06-10)
- Index metadata (`indexes(table)`) is still derived by parsing `sqlite_master` DDL text; `PRAGMA index_list` / `PRAGMA index_info` are not yet implemented in the engine.
