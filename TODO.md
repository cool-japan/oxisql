# OxiSQL TODO — v0.2.0

Last updated: 2026-06-17

MSRV 1.89 · License Apache-2.0 · 19 workspace crates (10 facade/drivers + 7 C-free
oxisqlite-* engine + 2 ancillary) · ~132,777 lines of Rust across 371 `.rs` files
(≈34.9k facade/drivers + ≈81.2k engine + ≈2.1k vendored TLS patch) · 1,997 tests
passing (nextest), 0 failing (≈85 skipped, mostly live-server-gated) · 0 build warnings ·
C-free proven (`CC=/usr/bin/false cargo build --workspace` → EXIT 0) ·
`cargo deny` licenses/bans/sources PASS (3 pre-existing advisories: paste,
rsa Marvin, rustls-pemfile).

## Release History

- [x] **0.1.0** released — initial public availability.
- [x] **0.1.1** released (2026-06-04) — CSV import/export, interactive REPL,
  named parameters, statement-cache infrastructure, schema introspection.
- [x] **0.1.2** released (2026-06-10) — three big waves: C-free oxisqlite engine fork,
  full-transaction ROLLBACK, Apache-2.0 compliance + TLS advisory fix.
- [x] **0.2.0** released (2026-06-17) — ANALYZE/System-R optimizer, UPSERT, execute/schema module splits, schema-cookie invalidation, blocking API, correlated subqueries, 1,997 tests.

## Done in 0.1.2

The headline of 0.1.2: OxiSQL **forked limbo into the in-tree `oxisqlite-*`
engine and now OWNS the SQLite path end-to-end**. The remaining gaps below
are OxiSQL's own roadmap, not "blocked on limbo upstream".

- [x] **Wave 1 — C-free `oxisqlite-*` engine fork.** Replaced the C-pulling
  `limbo` dependency with a 7-crate pure-Rust fork of limbo 0.0.22
  (commit `e59c5185`, MIT). Removed all 3 C touchpoints, making the SQLite
  path genuinely Pure Rust:
  - [x] `mimalloc` allocator dropped (no C allocator).
  - [x] `lemon.c` parser generator eliminated — pre-generated `parse.rs` /
    `keywords.rs` committed into the tree.
  - [x] `built` / `git2` build-info dropped → hardcoded consts.
- [x] **Wave 2 — Full-transaction ROLLBACK.** Ported from `turso_core`
    0.7.0-pre.5 (MIT). `BEGIN`/`INSERT`/`ROLLBACK` discards changes;
    `COMMIT` persists; WAL integrity preserved. This port proves the team can
    extend the engine itself.
  - Files: `oxisqlite-core/.../translate/rollback.rs` (new),
    `vdbe/execute.rs`, `storage/wal.rs`, `storage/pager.rs`,
    `oxisql-sqlite-compat/src/connection.rs`.
  - Tests: 5 in `oxisql-sqlite-compat/tests/rollback.rs`.
- [x] **Wave 3 — Apache-2.0 compliance + TLS advisory fix.**
  - [x] GPL `julian_day_converter` replaced by inline pure-Rust
    `oxisqlite-core/.../functions/julian_day.rs`.
  - [x] `cfg_block` dependency dropped.
  - [x] `deny.toml` allowlist extended: Zlib, Unicode-3.0, MPL-2.0,
    CDLA-Permissive-2.0.
  - [x] Root `/NOTICE` created.
  - [x] `rustls-rustcrypto-patched` fixes RUSTSEC-2026-0104 via
    `[patch.crates-io]` (vendored TLS patch).

## Done in 0.2.0

- [x] **ANALYZE statement.** `ANALYZE`, `ANALYZE main`, `ANALYZE <table>`, `ANALYZE <index>` create/update `sqlite_stat1` with `(tbl, idx, stat)` cardinality rows. `Insn::IdxStat` opcode. 6 integration tests in `tests/analyze.rs`.
- [x] **System-R optimizer with real ANALYZE statistics.** `SchemaStats` side-map (`statistics.rs`) mirrors `sqlite_stat1`; `estimate_cost_for_scan_or_seek` uses real selectivity; un-analyzed DBs unchanged (backwards compatible).
- [x] **Schema module split via splitrs.** `schema.rs` (1,920 lines) split into 7-file `schema/` subtree: `mod.rs`, `bootstrap.rs`, `column.rs`, `container.rs`, `index.rs`, `table.rs`, `tests.rs`.
- [x] **VDBE execute module split via splitrs.** `vdbe/execute.rs` (8,361 lines) split into 10-file `vdbe/execute/` subtree; `values.rs` consolidates `Value::exec_*` methods; `txn_schema.rs` consolidates transaction/savepoint/cookie handlers.
- [x] **UPSERT `ON CONFLICT DO UPDATE / DO NOTHING`.** All forms including `excluded.*`, multi-row, `DO NOTHING`, chained targets, `OR x` coexistence. `index_experimental` unique-index path.
- [x] **Schema-cookie bump + `SchemaChanged` detection.** DDL operations (`CREATE/DROP TABLE/INDEX`, `ALTER TABLE`) bump the schema cookie; `op_transaction` verifies the cookie at execute time; stale statements raise `LimboError::SchemaChanged`.
- [x] **Transparent re-prepare on `SchemaChanged` in compat layer.** `exec_rewritten` catches `SchemaChanged`, discards the stale program, re-prepares, and retries once — replacing the fragile `is_ddl` keyword-prefix heuristic. 4 tests in `tests/schema_reprepare.rs`.
- [x] **Statement cache activated.** `Statement::reset()` clears `Program::n_change`; cached statements used via the re-prepare path; `SELECT changes()` round-trip eliminated.
- [x] **`application_id` and `synchronous` pragmas.** `PRAGMA application_id [= N]` and `PRAGMA synchronous [= N]` fully implemented.
- [x] **Orphaned WAL protection.** Fresh database creation discards any pre-existing stale `-wal` file.
- [x] **WAL header refresh on open.** In-memory header reflects latest committed cookie values after WAL recovery.
- [x] **`checkpoint_truncate` API.** `Connection::checkpoint_truncate()` exposed; `Connection::close()` is now idempotent.
- [x] **`BlockingSqliteConnection`.** Synchronous wrapper via single-threaded tokio runtime; `execute`, `query`, `begin`, `commit`, `rollback`.
- [x] **`connect_or_create(uri)`.** Auto-creates missing PostgreSQL/MySQL databases before connecting.
- [x] **Correlated subqueries.** 19 tests in `tests/correlated.rs` covering scalar, EXISTS, NOT EXISTS, IN, NOT IN, nested patterns.
- [x] **Conflict-clause handling.** `INSERT OR FAIL/ABORT/ROLLBACK/IGNORE` + default-ABORT; 5 tests in `tests/conflict.rs`.
- [x] **Durability & WAL tests.** `tests/durability.rs` covers file-backed WAL commit/crash-recovery.
- [x] **LIMIT/OFFSET with bound parameters.** `LIMIT $1 OFFSET $2` works; 8 tests in `tests/limit_params.rs`.
- [x] **`CREATE INDEX IF NOT EXISTS`.** Silently succeeds when index already exists.
- [x] **IN/NOT IN three-valued logic.** `x NOT IN (set)` when set contains NULL correctly returns NULL; 7 regression tests in `tests/in_null.rs`.

## Milestones

- [x] **M0** — Skeleton: workspace, `deny.toml`, `Dockerfile.ffi-audit`,
  `scripts/ffi-audit.sh`, `oxisql-core` traits + error enum, empty facade.
- [x] **M1** — Embedded: `oxisql-embedded` (gluesql over MemoryStorage),
  `oxisql-parse` (sqlparser facade), `connect("memory://")`.
- [x] **M2** — Postgres: `oxisql-postgres` (`tokio-postgres` + OxiTLS),
  end-to-end query + transaction + prepared statement.
- [x] **M3** — MySQL (`oxisql-mysql`, tokio + mysql_async + OxiTLS) +
  SQLite-compat (`oxisql-sqlite-compat`, pure-Rust backend; `sqlite://`
  wired in facade; `$N→?` param rewriter; schema introspection).
- [x] **M4** — `oxisql-datafusion` (DataFusion TableProvider over a Connection).
- [x] **M5** — `oxisql-pool` (deadpool + custom managers) + `oxisql-migrate`
  (sqlparser, 14-digit timestamp filenames).
- [x] **M6** — C-free `oxisqlite` fork (Wave 1 above): 7-crate pure-Rust
  engine replaces the C-pulling limbo dep; all 3 C touchpoints removed.
- [x] **M7** — Full-transaction ROLLBACK (Wave 2 above): ported from
  turso_core; BEGIN/INSERT/ROLLBACK discards, COMMIT persists, WAL intact.
- [x] **M8** — Apache-2.0 compliance + TLS advisory fix (Wave 3 above):
  GPL code removed, NOTICE added, RUSTSEC-2026-0104 patched.

## Architecture
```
oxisql (facade)
  +-- oxisql-core         (traits: Connection, Transaction, Row, Value, OxiSqlError)
  +-- oxisql-parse        (SQL parsing via sqlparser, query planner/optimizer)
  +-- oxisql-embedded     (GlueSQL in-memory + fjall/redb persistent engines)
  +-- oxisql-postgres     (tokio-postgres wire protocol client)
  +-- oxisql-mysql        (mysql_async wire protocol client)
  +-- oxisql-sqlite-compat (pure-Rust SQLite over the in-tree oxisqlite engine)
  +-- oxisql-datafusion   (DataFusion TableProvider bridge)
  +-- oxisql-pool         (connection pooling)
  +-- oxisql-migrate      (migration runner)
  +-- oxisqlite-*         (7-crate C-free engine: forked from limbo 0.0.22)
```

## Cross-Cutting Priorities (historical — all complete)

- [x] **P0 — Connection Pooling.** `ConnectionPool` trait + per-backend pools +
  facade `connect_pooled(uri, config)`.
- [x] **P1 — Prepared Statements.** `PreparedStatement` type; server-side prep
  for Postgres/MySQL; AST caching for embedded.
- [x] **P2 — Extended Type System.** Decimal/Timestamp/Date/Time/Uuid/Json/Array
  `Value` variants, `FromValue` trait, full per-backend type mapping.
- [x] **P3 — Schema Introspection & Migration.** `SchemaInspector` trait
  (tables/columns/indexes/foreign_keys) per backend + `Migrator` engine.
- [x] **P4 — Query Builder & SQL Analysis.** Fluent builder, logical planner,
  optimizer (predicate pushdown, projection pruning, const folding, join
  reorder), join algorithms.
- [x] **P5 — DataFusion Live Streaming.** `OxiSqlStreamProvider` with
  filter/projection/limit/sort pushdown + multi-table catalog.
- [x] **P6 — Persistent Embedded Database.** GlueSQL `Store` over
  `oxistore-kv-fjall` and `oxistore-kv-redb`, WAL/ACID durability,
  `open_file(path)`, AST-level param binding.
- [x] **P7 — Wire Protocol Enhancements.** Postgres COPY/LISTEN-NOTIFY/pipeline;
  MySQL LOAD DATA LOCAL INFILE / multi-result-set / binary protocol.
- [x] **P8 — Observability.** Query logging + retry middleware, pool statistics,
  backend info reporting.

## Testing Priorities (historical — all complete)

- [x] Cross-backend portability tests (`oxisql/tests/portability.rs`).
- [x] Transaction isolation tests across all backends.
- [x] Type round-trip tests (`oxisql/tests/type_roundtrip.rs`).
- [x] Concurrent query stress tests for pooled connections.
- [x] TLS connection tests for Postgres and MySQL.
- [x] DataFusion integration tests (`oxisql/tests/datafusion_facade.rs`).

## Roadmap → 0.2.0+

Now that OxiSQL owns the engine, the following are **OxiSQL-owned engine work**,
not upstream blockers:

- [x] **SAVEPOINT / RELEASE / ROLLBACK TO SAVEPOINT** in oxisqlite (currently
  returns a clear "not supported yet" error). Next natural extension after the
  full-transaction ROLLBACK port. (planned 2026-06-10)
  - **Goal:** Full SQLite-compatible nested savepoint semantics: SAVEPOINT opens a named rollback point; ROLLBACK TO restores only post-savepoint pages; RELEASE commits to the parent scope. SAVEPOINT in autocommit starts a transaction; RELEASE of the outermost savepoint commits it.
  - **Design:** Add `SavepointOp{Begin,Release,RollbackTo}` + `Insn::Savepoint{op,name}` to vdbe/insn.rs; translate/rollback.rs gets `translate_savepoint`/`translate_release`; op_savepoint handler in execute.rs (single-pager, Rc<RefCell>, no MVCC); Pager gets a `savepoints: RefCell<Vec<SavepointFrame>>` stack (name, wal_max_frame, wal_checksum, db_size, dirty_pages, page_preimages — a fork-native in-memory subjournal); WAL rollback_to_frame generalizes the existing txn_start_max_frame rollback. Use the SQLite TCL test suite as semantic spec for savepoint semantics.
  - **Files:** `crates/oxisqlite-core/`: translate/mod.rs, translate/rollback.rs, vdbe/insn.rs, vdbe/execute.rs, storage/pager.rs, storage/wal.rs, connection.rs; `crates/oxisql-sqlite-compat/src/connection.rs`; new `crates/oxisql-sqlite-compat/tests/savepoint.rs`.
  - **Tests:** single rollback; nested (inner rollback preserves outer); RELEASE merges to parent; ROLLBACK TO partial undo preserving earlier in-txn changes; SAVEPOINT-outside-txn; RELEASE-outermost commits; interleave with full COMMIT/ROLLBACK; DB-growth-during-savepoint.
  - **Risk:** Page pre-image store is the hard part — ROLLBACK TO cannot clear_page_cache() wholesale; must restore only post-savepoint dirty pages. Wrong implementation → silent corruption. Mitigation: thorough test suite; subagent returns `deviated` rather than ship a corruption-prone half-impl.
- [x] **Activate the statement cache** — fix oxisqlite `Statement::reset()` to also
  clear `Program::n_change`, then switch execution from the per-call `conn.execute()`
  fallback to the cached prepared path (no compat-layer changes needed once the engine
  is fixed). (planned 2026-06-10)
  - **Goal:** Cached statement reuse (parse-skip on cache hit) and correct change counts on every execution, eliminating both the re-prepare overhead and the `SELECT changes()` extra round-trip.
  - **Design:** Engine: `Statement::reset()` adds `self.program.n_change.set(0)` (n_change is Cell<i64> on Program via Rc — interior mutable, safe). Compat: rewrite `exec_rewritten` to execute the cached Statement on hit and read change count natively; eliminate SELECT changes() round-trip.
  - **Files:** `crates/oxisqlite-core/lib.rs`; `crates/oxisql-sqlite-compat/src/connection.rs`.
  - **Tests:** Cached-reuse N-vs-1 regression; INSERT/UPDATE/DELETE affected-row counts; DDL/TCL return 0.
  - **Risk:** Low. The fix is a 1-line engine change; verified against source.
- [x] **PRAGMA foreign_key_list** in oxisqlite — today FK metadata is parsed
  from `sqlite_master` DDL text; implement the native PRAGMA. (planned 2026-06-10)
  - **Goal:** `PRAGMA foreign_key_list(table)` returns SQLite's 8-column shape (id, seq, table, from, to, on_update, on_delete, match); `foreign_keys()` in the compat layer uses it instead of hand-rolling DDL-text parsing; `test_foreign_keys_basic` un-ignored.
  - **Design:** Parser: add `ForeignKeyList` to `PragmaName` enum + string mapping. Engine/schema: add `foreign_keys: Vec<ForeignKeyDef>` to `BTreeTable`; populate from DDL by capturing `ColumnConstraint::ForeignKey`/`TableConstraint::ForeignKey` (currently discarded with `_ => {}`); round-trip through sqlite_master reload. translate/pragma.rs: new `PragmaName::ForeignKeyList` arm mirroring `TableInfo`. Compat: rewrite `foreign_keys()` to query the pragma; drop `parse_foreign_keys` text scanner.
  - **Files:** `crates/oxisqlite-sqlite3-parser/src/parser/ast/mod.rs`; `crates/oxisqlite-core/schema.rs`, `translate/pragma.rs`; `crates/oxisql-sqlite-compat/src/connection.rs`.
  - **Tests:** Single/multi-column FKs, ON DELETE/UPDATE, composite id/seq, reload from disk.
  - **Risk:** Medium-high. Schema struct change affects DDL load path; must verify reload correctness.
- [x] **Fix the two hanging btree balancing tests** in oxisqlite-core
  (`test_drop_page_in_balancing_issue_1203` and `_1203_2`) — root cause was
  `pragma_update()`/`pragma()` in `lib.rs` using `_ => break` which swallowed
  `StepResult::IO`, preventing WAL commit and leaving the write lock permanently
  held; fixed with an explicit `StepResult::IO => { stmt.run_once()?; }` arm.
  Both tests now pass in ~0.02s each. oxisqlite-core: 540 passed (was 538).
- [x] **Clean the lone test-profile warning** — unused import `CreateBTreeFlags`
  at `oxisqlite-core/.../storage/btree.rs:6506` — fixed by moving the import
  behind `#[cfg(feature = "index_experimental")]`; workspace is now warning-free.
- [x] **Richer date/time/UUID Value mapping** for the sqlite-compat path. (planned 2026-06-10)
  - **Goal:** Columns declared DATE/TIMESTAMP/DATETIME/TIME/UUID come back as `Value::Date`/`Value::Timestamp`/`Value::Time`/`Value::Uuid` instead of I64/Text; round-trip with the existing outbound `core_to_limbo` formatting is symmetric.
  - **Design:** Extend `limbo_to_core` to take an optional declared-type hint; thread the column's declared type from `stmt.columns()` into `query_rewritten`; add `decl_type()` to the engine's Column metadata if not already exposed (IMPLEMENT POLICY). Mirror the outbound formatting (Timestamp→µs, Date→days, Time→µs, Uuid→u128).
  - **Files:** `crates/oxisql-sqlite-compat/src/types.rs`, `src/connection.rs`; `crates/oxisqlite-core/` if Column metadata needs extending.
  - **Tests:** Round-trip per typed variant; non-temporal TEXT stays Text (no false retyping).
  - **Risk:** Low. Pure conversion logic; no engine storage change.
- [ ] **Optional `system` libpq feature** for legacy parity (does NOT exist yet —
  aspirational; would be feature-gated to preserve the Pure-Rust default).
- [ ] **Decide whether to publish / further rename the `oxisqlite-*` crates**
  (currently internal workspace members).

## Known Limitations / Open Items

- [x] **ROLLBACK** — DONE in 0.1.2 (Wave 2 / M7). Full-transaction rollback now
  works; the previously `#[ignore]`d tests are live.
- [x] **SAVEPOINT** — DONE in this wave; SAVEPOINT/RELEASE/ROLLBACK TO now fully implemented in oxisqlite with WAL-based page-state restoration. Full nested savepoint semantics working.
- [x] **Statement-cache activation** — DONE in this wave; Statement::reset() now clears Program::n_change; exec_rewritten uses cached statements with native change counts; SELECT changes() round-trip eliminated.
- [x] **PRAGMA foreign_key_list** — DONE in this wave; ForeignKeyDef metadata added to BTreeTable schema; PRAGMA foreign_key_list(T) emits the 8-column SQLite shape; foreign_keys() in compat layer uses the pragma; test_foreign_keys_basic un-ignored and passing.
- [x] **Two hanging btree balancing tests** in oxisqlite-core — RESOLVED. Both
  `test_drop_page_in_balancing_issue_1203` and `_1203_2` now pass; root cause
  was `StepResult::IO` swallowed by `_ => break` in `pragma_update()`/`pragma()`
  holding the WAL write lock permanently. Fixed with explicit IO arm in `lib.rs`.
- [~] **Live-server-gated tests** — Postgres/MySQL integration tests that require
  a running server are `#[ignore]`d (≈35 ignored total, mostly these).

## Open Questions

1. **gluesql dialect exposure.** Expose gluesql's full non-standard SQL surface
   (more capability, less portability) or restrict the facade to a documented
   standard subset?
2. **DataFusion as hard or soft dep.** Keep `oxisql-datafusion` as an in-tree
   sub-crate or split into a separate `oxisql-datafusion-ext` to keep facade
   compile times low?
3. **Migration filename format.** sqlx-style vs COOLJAPAN-native — RESOLVED:
   chose 14-digit timestamps; revisit only if ecosystem familiarity demands the
   exact sqlx directory layout.
4. ~~**limbo cutoff / which limbo version to ship.**~~ RESOLVED in 0.1.2 — we
   forked limbo 0.0.22 into the in-tree `oxisqlite-*` engine and now own it.

## Subcrate TODOs

See the per-crate `TODO.md` files for fine-grained tracking:
- `crates/oxisql-core/TODO.md`
- `crates/oxisql-parse/TODO.md`
- `crates/oxisql-embedded/TODO.md`
- `crates/oxisql-postgres/TODO.md`
- `crates/oxisql-mysql/TODO.md`
- `crates/oxisql-datafusion/TODO.md`
- `crates/oxisql/TODO.md`

## oxisqlite engine follow-ups (2026-06-12)

Known gaps and workarounds accumulated during consumer integration (0.2.0 era):

- [x] Correlated subqueries panic "not yet implemented" (`crates/oxisqlite-core/translate/expr.rs` ~:2111) — consumers had to restructure into separate COUNT queries. (planned 2026-06-15)
  - **Goal:** Scalar subqueries in expression position (`SELECT (SELECT …)`, `WHERE x = (SELECT …)`) and correlated `EXISTS (…)` execute correctly, including subqueries referencing outer-scope columns (re-evaluated per outer row). `IN (SELECT …)` works at least non-correlated (materialized), correlated if feasible. No more panic.
  - **Design:** Reuse the coroutine machinery (`subquery.rs::emit_subquery`: `Insn::InitCoroutine`/`Yield`/`EndCoroutine`). Extend `Resolver` (`emitter.rs:33–64`) with an outer-scope reference so an inner column unresolved in the inner scope resolves against the outer scope and emits `Insn::Column` reading the current outer row. Scalar subquery: result register, inner SELECT writes single result, halt after first row (NULL if none); emit inline when correlated, hoist+`Insn::Once` when not. EXISTS: reg ← 1 on first inner row else 0. IN (SELECT): materialize into ephemeral btree/index (non-correlated), re-scan per outer row (correlated). Handle `Expr::Subquery`/`Expr::Exists`/`Expr::InSelect` arms in `expr.rs`.
  - **Files:** `crates/oxisqlite-core/translate/expr.rs`, `translate/subquery.rs`, `translate/emitter.rs`, `translate/main_loop.rs`, `vdbe/execute.rs`, `vdbe/insn.rs`.
  - **Tests:** `scalar_subquery_uncorrelated`, `scalar_subquery_correlated`, `correlated_in_where`, `exists_correlated`/`not_exists_correlated`, `in_subquery_uncorrelated`, `scalar_subquery_no_row_is_null`, plus a regression that the prior `todo!()` query no longer panics.
  - **Risk:** Hard — outer-column resolution + per-row re-evaluation is the crux. `expr.rs` (3121 ln) and `execute.rs` (8378 ln) are already over the 2000-ln policy; add code without splitting them (split is a separate follow-up). If correlated `IN` is too large, deliver scalar + EXISTS + non-correlated IN and report a precise done/not-done list.
- [x] Positional-parameter bug: params bind NULL when a column has explicit NOT NULL combined with a table-level PRIMARY KEY clause — consumers worked around with `INTEGER PRIMARY KEY` column-level form. (planned 2026-06-15)
  - **Goal:** `CREATE TABLE t(a INTEGER NOT NULL, PRIMARY KEY(a)); INSERT INTO t VALUES (?)` binds the parameter value (not NULL). Column-level and table-level PK behave identically. Eliminates a silent data-loss / NOT NULL-violation bug.
  - **Design:** Root cause in `translate/schema.rs` (~:460–645): a column's `primary_key` flag is set retroactively for table-level `PRIMARY KEY(col)` only when the column was parsed after the constraint. Fix: after all columns and table constraints are parsed, do a final pass setting `column.primary_key = true` (+ rowid-alias bookkeeping) for every name in `primary_key_columns`, order-independent. Defense-in-depth in `translate/insert.rs` (~:999/:1062): the `is_nullable` decision consults the table's `primary_key_columns` (case-insensitive), not just the per-column bool, so an unmapped PK column never silently emits `Insn::Null`.
  - **Files:** `crates/oxisqlite-core/translate/schema.rs`, `translate/insert.rs`.
  - **Tests:** `table_level_pk_param_not_null`, `pk_column_before_constraint`, `pk_column_after_constraint`, `composite_table_level_pk_params`, `column_level_int_pk_still_works` (no regression); verify the bound value round-trips via `SELECT`.
  - **Risk:** Could affect rowid-alias detection (`INTEGER PRIMARY KEY`). Guard the rowid-alias path and cover with the no-regression test.
- [ ] WITHOUT ROWID table inserts unsupported.
- [x] `PRAGMA synchronous` rejected as invalid pragma. (planned 2026-06-15 — Slice A: durability & WAL lifecycle, implemented together with the three items below)
  - **Goal:** (A1) `PRAGMA synchronous = OFF|NORMAL|FULL|EXTRA` (and `0|1|2|3`) accepted, stored, returned on read, gates fsync. (A2/A3) On clean connection close the WAL is checkpointed AND truncated/removed so a byte-level consumer reads the `.db` immediately without a manual `PRAGMA wal_checkpoint`. (A4) `read_entire_wal_dumb` returns a `Corrupt`/IO error on a malformed WAL instead of panicking.
  - **Design:** (A4) Convert the `panic!` sites + six `try_into().unwrap()` (~:1458–1465) in `storage/sqlite3_ondisk.rs::read_entire_wal_dumb` into `Err(LimboError::Corrupt(...))` with bounds checks; keep the graceful salt-mismatch `break`; propagate the `Result` through `wal.rs` callers. (A1) Add `PragmaName::Synchronous` to the parser iff absent; read/write arms in `translate/pragma.rs`; a `SynchronousMode` on the pager gating the fsync calls in `pager.rs`/`wal.rs` (OFF → skip WAL fsync; NORMAL → fsync on checkpoint; FULL/EXTRA → fsync WAL on commit + DB file after checkpoint). (A2/A3) Implement the real `Truncate` checkpoint mode in `wal.rs` (resolve `TODO(pere): truncate wal file here` ~:873): `file.truncate(0)` + reset `max_frame`; route `PRAGMA wal_checkpoint(TRUNCATE)`; add a panic-free best-effort `Drop for Connection` in `lib.rs` (Truncate checkpoint + WAL truncate; swallow+`tracing::warn!` on error) and have `close()` use Truncate too.
  - **Files:** `crates/oxisqlite-core/storage/sqlite3_ondisk.rs`, `storage/wal.rs`, `storage/pager.rs`, `lib.rs`, `translate/pragma.rs`; iff needed `crates/oxisqlite-sqlite3-parser` `PragmaName`.
  - **Tests** (`std::env::temp_dir()`): `pragma_synchronous_roundtrip`, `synchronous_off_still_durable_within_process`, `wal_truncated_on_close`, `wal_checkpoint_truncate_mode`, `drop_checkpoints_without_explicit_close`, `malformed_wal_returns_err_not_panic`.
  - **Risk:** `Drop` doing I/O must be panic-free + not double-close (guard flag, best-effort). Truncate only when no other active readers else fall back to Passive. fsync gating changes only the sync calls, not ordering.
- [x] Parameterized LIMIT/OFFSET (`LIMIT $1`) unsupported — consumers inline integer literals. (DONE — 0.2.0: `parse_limit_full`/`LimitValue` in planner.rs; `init_limit` extended with runtime expr support; NULL→-1, 0→IfNot skip, negative→unlimited; 8 integration tests in limit_params.rs)
- [x] No checkpoint-on-close: consumers must run `PRAGMA wal_checkpoint` before handing the .db file to another reader (byte-level file consumers like GeoPackage). (planned 2026-06-15 — Slice A durability; see the PRAGMA synchronous plan block above)
- [x] `UPDATE OR REPLACE` still bails (`crates/oxisqlite-core/translate/update.rs` ~:101). (DONE — 0.2.0 Slice 1: OR-conflict threading removed; UPDATE OR ABORT/FAIL/ROLLBACK/IGNORE/REPLACE all work via plan.or_conflict)
- [x] **`INSERT … ON CONFLICT (target) DO NOTHING` + the upsert target-matching infrastructure** (planned 2026-06-16)
  - **Goal:** Stop discarding the `Upsert` payload; build the conflict-target resolver; implement `DO NOTHING` end-to-end.
  - **Design:** Thread `Option<Upsert>` out of `InsertBody::Select` at `insert.rs:125`. New `resolve_upsert_targets()` helper produces `UpsertPlan { rowid_action, index_actions, catch_all_nothing }`. At each conflict fall-through (rowid `NotExists` :468, unique `NoConflict` :632), emit `Goto next_record_label` for `DO NOTHING` / catch-all targets. Validation walk rejects unknown `excluded.<col>` typos before emission. Rejects: target-less DO UPDATE, partial-index targets, no-matching-constraint targets.
  - **Files:** `translate/insert.rs`; new `tests/upsert.rs`; `crates/oxisqlite-core/Cargo.toml` (`[[test]] name="upsert"`).
  - **Tests:** `do_nothing_skips_rowid_conflict`, `do_nothing_target_omitted_skips`, `do_nothing_multirow_continues`, `plain_insert_and_or_ignore_replace_unaffected`, negative: `do_update_target_omitted_errors`, `on_conflict_no_matching_constraint_errors`; `#[cfg(feature="index_experimental")]`: `do_nothing_unique_index_target`.
- [x] **`INSERT … ON CONFLICT (target) DO UPDATE SET … [WHERE …]` with full `excluded.*`** (planned 2026-06-16)
  - **Goal:** Implement the DO UPDATE row rewrite with `excluded.*` (proposed-row register reads) and OLD-value carry-forward. Rowid/INTEGER-PK targets in default build; unique-index targets under `index_experimental`.
  - **Design:** Add `pub upsert_reg_overrides: Vec<(ast::Expr, usize)>` (owned) to `Resolver` (`emitter.rs:35`); extend `resolve_cached_expr_reg` to scan it first — zero `expr.rs` changes. New `emit_upsert_do_update()` helper in `insert.rs`: (1) read OLD via `emit_column` from positioned victim cursor; (2) populate overrides (`col`/`tbl.col`→old, `excluded.col`→proposed register `column_registers_start+i`); (3) DO UPDATE WHERE guard (`IfNot`); (4) build new row into fresh `new_start` registers; (5) strict TypeCheck; (6) rowid-change re-uniqueness; (7) index maintenance (delete old keys, insert new); (8) `Delete`+`Insert(update=true)`; (9) clear overrides + `Goto next_record_label`.
  - **Files:** `translate/emitter.rs` (~4 lines), `translate/insert.rs`; `tests/upsert.rs`.
  - **Tests:** `do_update_set_excluded_value`, `do_update_old_plus_one`, `do_update_old_plus_excluded`, `do_update_where_false_unchanged`, `do_update_where_true_applies`, `do_update_multirow`, `do_update_changes_rowid`, `do_update_notnull_violation_errors`, `excluded_invalid_outside_do_update`, `excluded_omitted_column`; `#[cfg(feature="index_experimental")]`: `do_update_unique_index_target`, `do_update_maintains_index`.
- [x] **Upsert edge-cases: chained clauses, `INSERT OR x` coexistence, generated-column rejection** (planned 2026-06-16)
  - **Goal:** Close remaining SQLite-parity edges; flip `TODO.md:235` to `[x]`.
  - **Design:** Confirm per-target routing yields OR-action coexistence by construction; add tests locking it. Reject `SET` on generated columns if schema marks them.
  - **Files:** `translate/insert.rs` (small); `tests/upsert.rs`.
  - **Tests:** `chained_conflict_targets`, `or_ignore_with_on_conflict_coexist`, `or_replace_with_on_conflict_coexist`, `composite_pk_target_do_update`, `set_generated_column_errors`.
- [x] Multi-row INSERT rollback unimplemented — OR ABORT/FAIL/ROLLBACK cannot be faithful for multi-row inserts. (DONE — 0.2.0 Slice 1: statement savepoint "_stmt" wraps multi-row writes; conflict emits SavepointOp::RollbackTo before Halt)
- [x] **Bump the schema cookie on every DDL + implement the `SchemaVersion` `SetCookie` writer** (planned 2026-06-16)
  - **Goal:** After this slice, `PRAGMA schema_version` increments by exactly 1 after every `CREATE TABLE` / `DROP TABLE` / `CREATE INDEX` / `DROP INDEX` / `ALTER TABLE …` / `CREATE VIRTUAL TABLE`, and `PRAGMA schema_version = N` works (no `todo!()`). No behavior change to existing statements — nothing reads the cookie for control flow yet — so all existing tests stay green.
  - **Files:** `vdbe/builder.rs`, `vdbe/execute.rs` (`op_set_cookie`), `translate/mod.rs`, `translate/pragma.rs`, `translate/schema.rs`, `translate/index.rs`, `translate/alter.rs`; new `tests/schema_cookie.rs` + `[[test]]` entry in `Cargo.toml`.
- [x] **Record the compile-time cookie on the prologue `Transaction`, verify at execute, add `LimboError::SchemaChanged`** (planned 2026-06-16)
  - **Goal:** A statement reused after a DDL raises `LimboError::SchemaChanged` at its first `step()` instead of running against a stale schema. A freshly-prepared statement with no intervening DDL never raises it.
  - **Files:** `error.rs`, `vdbe/insn.rs`, `vdbe/builder.rs`, `vdbe/execute.rs` (`op_transaction`), `translate/transaction.rs`, `vdbe/explain.rs`; `tests/schema_cookie.rs` (extend).
- [x] **Surface `SchemaChanged` in the facade and replace the compat `is_ddl` heuristic with re-prepare-on-schema-change** (planned 2026-06-16)
  - **Goal:** Delete the fragile `is_ddl` prefix check; route all execute statements through one cache path that transparently re-prepares a cached statement when the engine reports `SchemaChanged`. The comment-prefix false-negative and the never-evicted-stale-cache bugs both disappear.
  - **Files:** `crates/oxisqlite/src/lib.rs`, `crates/oxisql-sqlite-compat/src/connection.rs`; new `crates/oxisql-sqlite-compat/tests/schema_reprepare.rs`.
- [x] WAL truncate hygiene: stale -wal files are now salt-rejected on fresh-DB creation (pre-0.2.0 fix), but the engine never truncates/removes WAL on clean close. (planned 2026-06-15 — Slice A durability; see the PRAGMA synchronous plan block above)
- [x] `read_entire_wal_dumb` panics on malformed WAL — harden to an error return. (planned 2026-06-15 — Slice A durability; see the PRAGMA synchronous plan block above)
- [x] crates.io: oxisql-sqlite-compat 0.1.2 yanked 2026-06-12 (DDL statement-cache bug; fixed in 0.2.0).
- [x] Engine versions shipping in 0.2.0: oxisqlite-core 0.2.0, oxisqlite facade 0.2.0, oxisqlite-sqlite3-parser 0.2.0.
- [x] `ANALYZE` statement not implemented — DONE in 0.2.0; see "Done in 0.2.0" section above.

## Proposed follow-ups (2026-06-15)

Deferred from the 2026-06-15 `/ultra` run (collide on files owned by that run's slices; NOT blocked):
- **INSERT/UPDATE write-conflict cluster** (one coherent `translate/insert.rs` + `translate/update.rs` + `vdbe/execute.rs` slice): `UPDATE OR REPLACE` (above), `ON CONFLICT … DO UPDATE`/`excluded.*` (above), multi-row INSERT rollback with OR ABORT/FAIL/ROLLBACK (above). Savepoint primitives already exist (`pager.savepoints`, `Insn::Savepoint`).
- [x] **Parameterized `LIMIT $1`/`OFFSET`** — DONE 2026-06-16: `parse_limit_full`/`LimitValue` in `translate/planner.rs`; `init_limit` extended; 8 tests in `tests/limit_params.rs`.
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

- [~] **Slice 5: `splitrs` split `storage/btree.rs` (8715 lines → under 2000)** (planned 2026-06-16)
  - **Goal:** `storage/btree.rs` and every product module < 2000 lines; BTreeCursor API intact.
  - **Files:** `crates/oxisqlite-core/storage/btree.rs` → new `storage/btree/` module tree
- ~~**Engine schema-cookie statement invalidation** then remove the compat-side DDL-prefix bypass~~ — now planned as three `[~]` slices above (2026-06-16): SetCookie writer, SchemaChanged detection, facade re-prepare.
- [x] **IN / NOT IN three-valued-logic refinement** — `x NOT IN/IN (set)` with no match but the set contains a NULL now correctly returns NULL (three-valued logic). Fixed in `translate/subquery.rs` via Rewind+Column+IsNull check after set materialization; 7 regression tests in `tests/in_null.rs`.

Reclassified ready → blocked (upstream limitation):
- **Native `LOAD DATA LOCAL INFILE`** (`crates/oxisql-mysql/TODO.md`) — `mysql_async` 0.37 exposes no public `local_infile_handler`. Revisit when upstream adds it; `load_data_batched` remains the supported path.

Pre-existing refactor (surface, dedicated task — do NOT mix with feature work):
- `splitrs` split of `vdbe/execute.rs` (8378 ln), `translate/expr.rs` (3121 ln), `storage/btree.rs` (8715 ln) — all already over the 2000-ln policy before the 2026-06-15 run.
