# oxisql-sqlite-compat — TODO

## Status: Alpha (0.3.2)

Pure-Rust SQLite-compatible backend over the C-free **`oxisqlite`** engine (a
COOLJAPAN fork of limbo 0.0.22 with all C/C++ dependencies stripped). Implements
`oxisql_core::Connection` / `Transaction` / `PreparedStatement`. No `libsqlite3`, no
C/C++. `limbo` appears only as historical fork lineage; the live dependency is
`oxisqlite`, which OxiSQL owns and maintains.

**Tests: 61 pass, 1 ignored** (`test_foreign_keys_basic` — `oxisqlite` 0.0.22 does
not yet preserve FK DDL in `sqlite_master`; not a live-server gate).

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
- [x] `foreign_keys()` via `sqlite_master` DDL parsing
- [x] Statement-cache infrastructure (LRU, 128 slots, keyed by rewritten SQL)
- [x] DataFusion registration for SQLite tables

## Roadmap / next

All of the following are **OxiSQL-owned `oxisqlite` engine work** — we maintain the
engine, so these are our roadmap items rather than external blockers.

- [x] **Savepoints** — implement SAVEPOINT/RELEASE/ROLLBACK TO in oxisqlite, then surface through SqliteTransaction. (planned 2026-06-10 — see root TODO.md)
- [x] **Statement-cache activation** — fix Statement::reset() n_change, switch exec_rewritten to cached path + native change count. (planned 2026-06-10 — see root TODO.md)
- [x] **PRAGMA foreign_key_list** — add native FK metadata to oxisqlite engine + schema; rewrite foreign_keys() to use the pragma; un-ignore test_foreign_keys_basic. (planned 2026-06-10 — see root TODO.md)
- [x] **Richer type mapping** — extend limbo_to_core with declared-type hints for DATE/TIMESTAMP/TIME/UUID columns. (planned 2026-06-10 — see root TODO.md)
- [x] **Native affected-row counts** — read n_change natively after engine fix; drop SELECT changes() round-trip. (planned 2026-06-10 — part of statement-cache activation work)

## Known limitations

- `SAVEPOINT` / `RELEASE` / `ROLLBACK TO SAVEPOINT` return a clear "not supported yet" `OxiSqlError` (planned engine work). (DONE 2026-06-10)
- Foreign-key metadata is derived by parsing `sqlite_master` DDL text; no `PRAGMA foreign_key_list` yet, and FK DDL is not always preserved in `sqlite_master` (the one ignored test). (DONE 2026-06-10)
- The statement cache is populated but bypassed pending the `Statement::reset()` / `Program::n_change` engine fix. (DONE 2026-06-10)
- Date/time and UUID values are stored/returned as `TEXT` / `INTEGER` (no dedicated `Value` variants yet).
- Affected-row counts cost one extra `SELECT changes()` round-trip per DML statement. (DONE 2026-06-10)
