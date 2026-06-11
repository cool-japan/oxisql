# OxiSQL TODO — v0.1.2

Last updated: 2026-06-10

MSRV 1.89 · License Apache-2.0 · 17 workspace crates (10 facade/drivers + 7 C-free
oxisqlite-* engine) · ~118,234 lines of Rust across 323 `.rs` files
(≈34.9k facade/drivers + ≈81.2k engine + ≈2.1k vendored TLS patch) · 1,851 tests
passing (nextest), 0 failing (≈83 skipped, 4 slow, mostly live-server-gated) · 0 build warnings ·
C-free proven (`CC=/usr/bin/false cargo build --workspace` → EXIT 0) ·
`cargo deny` licenses/bans/sources PASS (3 pre-existing advisories: paste,
rsa Marvin, rustls-pemfile).

## Release History

- [x] **0.1.0** released — initial public availability.
- [x] **0.1.1** released (2026-06-04) — CSV import/export, interactive REPL,
  named parameters, statement-cache infrastructure, schema introspection.
- [x] **0.1.2** released (2026-06-10) — three big waves: C-free oxisqlite engine fork,
  full-transaction ROLLBACK, Apache-2.0 compliance + TLS advisory fix.

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

## Roadmap → 0.1.3+

Now that OxiSQL owns the engine, the following are **OxiSQL-owned engine work**,
not upstream blockers:

- [x] **SAVEPOINT / RELEASE / ROLLBACK TO SAVEPOINT** in oxisqlite (currently
  returns a clear "not supported yet" error). Next natural extension after the
  full-transaction ROLLBACK port. (planned 2026-06-10)
  - **Goal:** Full SQLite-compatible nested savepoint semantics: SAVEPOINT opens a named rollback point; ROLLBACK TO restores only post-savepoint pages; RELEASE commits to the parent scope. SAVEPOINT in autocommit starts a transaction; RELEASE of the outermost savepoint commits it.
  - **Design:** Add `SavepointOp{Begin,Release,RollbackTo}` + `Insn::Savepoint{op,name}` to vdbe/insn.rs; translate/rollback.rs gets `translate_savepoint`/`translate_release`; op_savepoint handler in execute.rs (single-pager, Rc<RefCell>, no MVCC); Pager gets a `savepoints: RefCell<Vec<SavepointFrame>>` stack (name, wal_max_frame, wal_checksum, db_size, dirty_pages, page_preimages — a fork-native in-memory subjournal); WAL rollback_to_frame generalizes the existing txn_start_max_frame rollback. Reference `~/work/oxilimbo/core/` as semantic spec only (oxilimbo is Arc<RwLock>/MVCC — not copy-paste).
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
