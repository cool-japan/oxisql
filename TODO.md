# OxiSQL TODO — v0.4.0

Last updated: 2026-07-17

MSRV 1.89 · License Apache-2.0 · 20 workspace crates (10 facade/drivers + 7 C-free
oxisqlite-* engine + 3 ancillary: `whoami-patched`, `zstd-shim`,
`rustls-rustcrypto-patched`) · ~178,591 lines of Rust across 484 `.rs` files
(≈62.4k facade/drivers + ≈110.8k engine + ≈5.3k vendored patch shims) · 2,157
tests passing default-features (2,651 with `--all-features`) (nextest), 0
failing (31 skipped default-features, 90 with `--all-features`, mostly
live-server-gated) · 0 build warnings ·
C-free proven (`CC=/usr/bin/false cargo build --workspace` → EXIT 0) ·
`cargo deny check licenses bans sources` PASS; a full `cargo deny check` FAILS on
advisories (4 tracked as of 2026-07-17: `quick-xml` RUSTSEC-2026-0194/-0195 —
high-severity, reached only via `oxisqlite-core`'s dev-only `pprof`/`inferno`
benchmark dependency — `paste` + `rustls-pemfile` unmaintained; `rsa` 0.9.10
Marvin RUSTSEC-2023-0071 is now `[advisories] ignore`d in `deny.toml` — see
Q3 below, unreachable since OxiSQL is TLS-client-only); `cargo audit`
additionally flags `fxhash` + `instant` as unmaintained (no safe upgrade yet
for any of the remaining findings).

## Release History

- [x] **0.1.0** released — initial public availability.
- [x] **0.1.1** released (2026-06-04) — CSV import/export, interactive REPL,
  named parameters, statement-cache infrastructure, schema introspection.
- [x] **0.1.2** released (2026-06-10) — three big waves: C-free oxisqlite engine fork,
  full-transaction ROLLBACK, Apache-2.0 compliance + TLS advisory fix.
- [x] **0.2.0** released (2026-06-17) — ANALYZE/System-R optimizer, UPSERT, execute/schema module splits, schema-cookie invalidation, blocking API, correlated subqueries, 1,997 tests.
- [x] **0.2.1** released (2026-06-20) — WITHOUT ROWID table support, `BorrowedValue<'a>` zero-alloc SQL value view, B-tree module split, doc-test fix (cancel_token), 2,024 tests.
- [x] **0.3.0** released (2026-06-22) — objc2-system-configuration removed from default dep closure, whoami-patched vendored crate, oxisqlite-core pure I/O backend, oxitls ^0.2.0.
- [x] **0.3.1** released (2026-06-23) — DataFusion 54 compatibility (arrow 58.3.0 pin, as_any() removed), oxistore-columnar 0.2.0 bump evaluated then reverted to 0.1.3 same day (arrow/parquet 59 conflicts with DataFusion 54's arrow ^58), FlamegraphProfiler benchmark utility, rand 0.9 API fix.
- [x] **0.3.2** released (2026-07-11) — `zstd-shim` Pure-Rust patch crate (drops the C-FFI `zstd-sys` from the `--all-features` closure via `oxiarc-zstd`, wired in via `[patch.crates-io]`), GROUP BY/HAVING `COUNT(*)` accumulator reset fix (`oxisqlite-core`), routine dependency bumps (`oxiarc-zstd` 0.3.5, `time` 0.3.53, `uuid` 1.23.4, `env_logger` 0.11.11, `io-uring` 0.7.13).
- [x] **0.3.3** released (2026-07-17) — `open_from_bytes` (open a database from an in-memory byte buffer, no temp file; `oxisqlite-core`/`oxisqlite`/`oxisql-sqlite-compat`), PostgreSQL logical replication (`oxisql-postgres` `replication` feature / `postgres-replication` facade feature, full `pgoutput` decoding + `COPY BOTH` streaming), `CREATE TABLE ... AS SELECT`, `EXCEPT`/`INTERSECT` compound-`SELECT` operators, `ON CONFLICT` on `CREATE TABLE` constraints, `OFFSET`/`ORDER BY`/`WITH` on compound `SELECT`, parenthesized join sources, `REGEXP` operator, more `printf()` format specifiers, virtual-table `ORDER BY` pushdown, named parameter binding in the raw `oxisqlite` engine, Windows file locking, server version accessors + cleaner REPL errors, index-based `DELETE`/`UPDATE` re-enabled after the real corruption hazard behind its disabling (limbo#1714) was properly fixed, a B-tree index-page balance panic/corruption fix, MVCC/WAL durability hardening, `UPDATE...RETURNING` + `ALTER TABLE RENAME` crash fixes plus several more engine correctness fixes, a PostgreSQL connect-timeout hang fix, and Miri-driven memory-safety hardening (40+ page-access call sites reworked). 2,157 tests passing (2,651 with `--all-features`), 0 failing. Two internal breaking changes (`oxisqlite::Params::Named` now `Cow<'static, str>` keyed; `oxisqlite-sqlite3-parser`'s lexer `Token` lost its lifetime parameter) — neither touches the `oxisql` facade API.

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

## Done in 0.2.1

- [x] **WITHOUT ROWID table support (`oxisqlite-core`).** `CREATE TABLE … WITHOUT ROWID` fully implemented using an index-format B-Tree. PK columns are the B-Tree key; the full row is the record payload. `INSERT` (single-row, multi-row, `INSERT … SELECT`), `SELECT` (full-scan), `OR IGNORE`, and `OR REPLACE` all work. PK NOT NULL and uniqueness enforced. `Index::synthetic_for_without_rowid()` drives cursor allocation. `validate_without_rowid_table()` enforces explicit PK + PK-columns-first layout constraint. 16 integration tests in `crates/oxisqlite-core/tests/without_rowid.rs`.
- [x] **`BorrowedValue<'a>` zero-allocation SQL value view (`oxisql-core`).** New enum where `Text`, `Blob`, `Json`, `Decimal` borrow from existing storage; all scalars copied inline. `to_owned() -> Value`, `From<&'a Value>`, `Display`, `type_name()` all implemented. Re-exported from `oxisql-core` root. 15 unit tests.
- [x] **B-tree module split via splitrs (`oxisqlite-core`).** `storage/btree.rs` (8 864 lines) split into 6-file sub-tree: `btree/mod.rs` (512 ln) + `btree/cursor_core.rs` (1 346 ln) + `btree/cursor_write.rs` (1 590 ln) + `btree/cursor_nav.rs` (1 265 ln) + `btree/page_ops.rs` (1 172 ln) + `btree/tests.rs` (1 964 ln). All tests pass; 0 warnings.
- [x] **Doc-test fix (`oxisql-postgres`).** Two doc examples on `PgConnection::cancel_token` / `PostgresCancelToken::cancel_query` lacked `.await` on the async `cancel_token()` call — fixed.

## Done in 0.3.3

- [x] **Open a database from an in-memory byte buffer (`oxisqlite-core`, `oxisqlite`, `oxisql-sqlite-compat`).** New `open_from_bytes` entry points at all three layers open a SQLite database directly from a byte slice (`include_bytes!`, `VACUUM INTO`, `sqlite3_serialize()` output) with no temporary file, enabling WASI/browser/read-only-filesystem use. `oxisqlite_core::Database::open_from_bytes(bytes, enable_mvcc)` is not gated by the `fs` feature; `oxisqlite::Database::open_from_bytes(bytes)` mirrors `sqlite3_deserialize()`; `SqliteConnection::open_from_bytes`/`SqliteConnectionBlocking::open_from_bytes` expose it through the facade. Malformed input returns a typed error, never panics.
- [x] **PostgreSQL logical replication (`oxisql-postgres`, opt-in `replication` feature; `postgres-replication` on the facade).** CDC-style streaming via `PgReplicationConnection::{connect, identify_system, create_replication_slot, drop_replication_slot, start_logical_replication}` returning a `ReplicationStream`; full `pgoutput` wire-protocol decoding, LSN parsing/formatting, `COPY BOTH` streaming with a background reader + keepalive task, and brace/quote/escape-aware array-literal text decoding. New `postgres-protocol` workspace dependency.
- [x] **SQL surface growth (`oxisqlite-core`).** `CREATE TABLE ... AS SELECT`; `EXCEPT`/`INTERSECT` compound-`SELECT` operators (`NULL`-aware, strict left-to-right grouping with `UNION`/`UNION ALL`); `ON CONFLICT` on `CREATE TABLE` column/table constraints; `OFFSET`/`ORDER BY`/`WITH` on the outer compound `SELECT`; parenthesized join sources (`FROM (t1 JOIN t2 ON ...)`); `REGEXP` operator/`regexp()` function; more `printf()` format specifiers (`%i`, `%x`/`%X`, `%o`, `%c`, `%e`/`%E`); virtual-table `ORDER BY` pushdown into `xBestIndex`.
- [x] **Named parameter binding in the raw engine (`oxisqlite`).** `:name`/`@name`/`$name`/`#name` placeholders now bind correctly in `Statement::query`/`execute` — previously an unconditional panic.
- [x] **Windows file locking (`oxisqlite-core`).** Real `LockFileEx`/`UnlockFileEx`-backed locking (`io/windows.rs`) via a new, Windows-only `windows-sys` dependency — previously an unconditional panic on that path.
- [x] **Index-based access restored for `DELETE`/`UPDATE` (`oxisqlite-core`, `index_experimental` feature).** Re-enabled after the real corruption hazard behind the original disabling ([limbo#1714](https://github.com/tursodatabase/limbo/issues/1714)) was properly fixed: the optimizer now keeps an index-based plan only when proven safe (non-looping rowid lookup, or a range scan/seek whose own key the statement provably cannot shift under it), otherwise still falls back to a full table scan.
- [x] **B-tree index-page balance panic/corruption fix (`oxisqlite-core`).** `insert_into_cell` no longer takes the in-place insert path when the target position lies beyond the page's physical cell count while overflow cells are pending — previously a panic (debug) or silent page corruption (release), reachable from ordinary index-heavy `JOIN` workloads whose transient automatic index grew deep enough to rebalance an interior index page.
- [x] **MVCC/WAL durability hardening (`oxisqlite-core`).** Fixed a transaction-removal race (typed errors instead of panics on concurrent commit/rollback), a commit-ordering bug where a transaction could be marked visible in memory before its WAL persist completed (crash-window data-loss gap), and WAL checkpoint bookkeeping so `checkpoint_seq`/`salt_1` evolve correctly on every fully-backfilled checkpoint and frame-cache bookkeeping doesn't grow unboundedly across repeated partial checkpoints.
- [x] **Correctness fixes: `UPDATE ... RETURNING`, `ALTER TABLE ... RENAME`, and more (`oxisqlite-core`).** `UPDATE ... RETURNING` previously returned zero rows silently; `ALTER TABLE ... RENAME` previously crashed on any schema containing a view/trigger/vtab anywhere; multi-row `INSERT` into virtual tables previously kept only the last row; `IN (...)` as a plain value expression, multiply-referenced `WITH` CTEs, wide-row (127+ column) header encoding, `BLOB`/`TEXT` coercion in `QUOTE()`/`||`/`concat()`, and a `PRAGMA auto_vacuum = FULL` high-water-mark bug are all also fixed; corrupt/malicious database files now return `LimboError::Corrupt` at several more sites instead of panicking.
- [x] **PostgreSQL connection-timeout hang fix (`oxisql`).** `connect()`/`connect_with_options`/`connect_with_tls` against an unreachable host previously hung indefinitely; now bounded by a 10s default timeout (`ConnectOptions::connect_timeout_ms`, returns a typed timeout error).
- [x] **Memory-safety hardening via Miri (`oxisqlite-core`).** Fixed a pointer-provenance bug materializing `BLOB`/`TEXT` from on-disk pages, an unaligned-reference bug in the `VECTOR` column type's slice conversions, and a page-cache memory leak; reworked 40+ page-access call sites across the storage/B-tree layers that could previously manufacture simultaneous mutable aliases into the same page buffer from a shared reference.
- [x] **Server version accessors + cleaner REPL errors.** `PgConnection::server_version()` (`oxisql-postgres`), `MyConnection::server_version()` (`oxisql-mysql`), `BackendInfo::from_postgres_connection`/`from_mysql_connection` (`oxisql`); a new `display_error()` helper replaces the raw GlueSQL parser-error debug-dump in `oxisql-repl`.
- [x] **Large modules split via `splitrs`.** `json/jsonb.rs`, `storage/pager.rs`, `functions/datetime.rs`, `translate/expr.rs`, `translate/insert.rs` (`oxisqlite-core`) plus `ast/fmt.rs`/`ast/mod.rs` (`oxisqlite-sqlite3-parser`) split to stay under the 2000-line file policy — no functional changes.
- [x] **Two internal breaking changes, neither touching the `oxisql` facade.** `oxisqlite`'s `Params::Named` now stores `Cow<'static, str>` keys (was `Vec<(String, Value)>`); `oxisqlite-sqlite3-parser`'s lexer `Token` dropped its lifetime parameter (`Token<'i>(usize, &'i [u8], usize)` → `Token(usize, Cow<'static, str>, usize)`). Both are internal to the low-level `oxisqlite`/parser crates beneath `oxisql-sqlite-compat`.

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
- [x] **Decide whether to publish / further rename the `oxisqlite-*` crates** —
  RESOLVED: published as-is (same names, no rename). All 7 `oxisqlite-*` crates
  (`oxisqlite-macros`, `oxisqlite-sqlite3-parser`, `oxisqlite-ext`, `oxisqlite-time`,
  `oxisqlite-uuid`, `oxisqlite-core`, `oxisqlite`) are live on crates.io alongside
  the `oxisql-*` facade/driver crates — none carry `publish = false`, and all are
  listed in the `CRATES` array in `pub_oxisql.sh`. Correction: per crates.io's own
  version history they were first published starting at v0.1.0 (2026-06-11), not
  newly introduced in 0.3.2 (2026-07-11); 0.3.2 is just their latest routine
  version bump alongside the rest of the workspace.

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
- `crates/oxisql-postgres/TODO.md` (now includes logical replication via `pgoutput`,
  behind the `replication` Cargo feature — see its "Logical replication" section)
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
- [x] WITHOUT ROWID table inserts — DONE in 0.2.1; see "Done in 0.2.1" section.
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

- [x] **Slice 5: `splitrs` split `storage/btree.rs` (8864 lines → under 2000)** (planned 2026-06-16; done 2026-06-19)
  - **Goal:** `storage/btree.rs` and every product module < 2000 lines; BTreeCursor API intact.
  - **Files:** `crates/oxisqlite-core/storage/btree.rs` → `btree/mod.rs` (512 ln) + `btree/cursor_core.rs` (1346 ln) + `btree/cursor_write.rs` (1590 ln) + `btree/cursor_nav.rs` (1265 ln) + `btree/page_ops.rs` (1172 ln) + `btree/tests.rs` (2028 ln). All six files included via `include!()` / `mod` from `mod.rs`. 636 tests pass, 0 warnings.
- ~~**Engine schema-cookie statement invalidation** then remove the compat-side DDL-prefix bypass~~ — now planned as three `[~]` slices above (2026-06-16): SetCookie writer, SchemaChanged detection, facade re-prepare.
- [x] **IN / NOT IN three-valued-logic refinement** — `x NOT IN/IN (set)` with no match but the set contains a NULL now correctly returns NULL (three-valued logic). Fixed in `translate/subquery.rs` via Rewind+Column+IsNull check after set materialization; 7 regression tests in `tests/in_null.rs`.

Reclassified ready → blocked (upstream limitation):
- **Native `LOAD DATA LOCAL INFILE`** (`crates/oxisql-mysql/TODO.md`) — `mysql_async` 0.37 exposes no public `local_infile_handler`. Revisit when upstream adds it; `load_data_batched` remains the supported path.

Pre-existing refactor (surface, dedicated task — do NOT mix with feature work):
- `splitrs` split of `vdbe/execute.rs` (8378 ln), `translate/expr.rs` (3121 ln), `storage/btree.rs` (8715 ln) — all already over the 2000-ln policy before the 2026-06-15 run.

## Stubs deferred (added 2026-07-13 by /stub-check)

16 items deliberately deferred (not implemented) during the 2026-07-13
`/stub-check` pass across `oxisqlite-core`, `oxisqlite-sqlite3-parser`, and
`oxisqlite-ext` — too large / needs a new subsystem / needs external input.
None of these three crates has its own `TODO.md` yet, so all items are
tracked here, grouped by crate, for a future `/ultra` pass to plan properly.

### oxisqlite-core

- [ ] Temp database / TEMP table support — `vdbe/execute/mutate.rs`, `vdbe/execute/txn_schema.rs` (6 sites total)
  - **Why deferred:** Needs a second catalog namespace + per-connection storage backend + Connection/Program multi-db plumbing — a new subsystem. Currently 100% dead code (parse-time rejection), zero live risk.
- [ ] Full serializable isolation (phantom reads, cursor lost updates, read/write skew) — `mvcc/mod.rs:27`
  - **Why deferred:** Needs commit-dependency/SIREAD-style tracking — zero supporting infrastructure exists; a research-grade concurrency-control project, not a bug fix.
- [ ] MVCC secondary-index insert — `storage/btree/cursor_nav.rs:239`
  - **Why deferred:** MVCC engine has no secondary-index keyspace at all; gated behind stacked `index_experimental` (off by default) + MVCC.
- [ ] User-defined COLLATE sequences — `translate/collate.rs:5`
  - **Why deferred:** Needs an FFI extension hook; `CollationSeq` is a closed 3-variant enum used pervasively.
- [ ] Expression-based UNIQUE constraints — `schema.rs:465`
  - **Why deferred:** Needs full expression-index infrastructure (evaluate/persist expression results during index maintenance) — a standalone engine feature.
- [ ] CREATE TRIGGER / trigger execution (blocks RAISE()) — `translate/expr.rs:2154`
  - **Why deferred:** Wholesale unimplemented elsewhere already; implement together as one project, not piecemeal.
- [ ] ATTACH DATABASE (blocks DoublyQualified, multi-schema) — `translate/expr.rs:670`
  - **Why deferred:** Low value today (2-part `table.column` already works everywhere); ATTACH itself is unimplemented.
- [ ] IN sometable / table-valued-function IN — `translate/expr.rs:2004`
  - **Why deferred:** 100% unimplemented in every context; rare SQLite extension syntax, needs a semantics decision (scan vs vtab invocation).
- [x] REGEXP operator — DONE (verified 2026-07-17): `X REGEXP Y` (== `regexp(Y, X)`) is fully implemented in `translate/expr/binary_emit.rs` (`ast::LikeOperator::Regexp` → `ScalarFunc::Regexp`) plus NULL-propagation/error handling in `translate/expr/value.rs`; covered by `tests/regexp_operator.rs`. This line is stale — kept struck through rather than deleted for the historical stub-check record.
- [ ] Async VirtualTableCursor I/O — `vdbe/insn.rs:378`
  - **Why deferred:** Backward-incompatible change to the public extension trait/API surface — needs coordinated sequencing with any existing vtab extensions.
- [ ] PRAGMA page_size on non-empty database — `translate/pragma.rs:225`
  - **Why deferred:** Needs a VACUUM-like full-file rewrite; coupled to pager/page-format internals beyond this translate-layer file.
- [ ] CTE/catalog-name scope & shadowing — `translate/planner.rs:421`
  - **Why deferred:** Needs a general "Scope" concept for identifier resolution; currently a safe strict-error (no silent-wrong-answer risk).

### oxisqlite-ext

- [ ] Cost-based query planning using vtab estimated_cost — `src/vtabs.rs:218`
  - **Why deferred:** Open-ended optimizer feature on the core query-planning side; the field/FFI plumbing already works, nothing to fix here.

### oxisqlite-sqlite3-parser

- [x] Lexer/parser crate split — WON'T FIX — `src/lexer/sql/mod.rs:24`
  - **Why deferred:** Pure reorg inherited from upstream Limbo, no functional benefit. (decision: not worth doing, kept only for historical record — listed for the historical stub-check hit, not phrased as future work)
- [ ] to_sql_string AST-fidelity backlog (trigger CTEs/DEFAULT VALUES/ALIAS, ORDER BY collation, LIMIT comma-form) — `create_trigger.rs:77,96,97`, `to_sql_string/stmt/select.rs:18,226`
  - **Why deferred:** Each needs a `parse.y` grammar change + Lemon regeneration; renders SQL that mostly can't execute yet (trigger bodies).
- [ ] Suspected LEFT/RIGHT JOIN bitflag bug (unverified) — `to_sql_string/stmt/select.rs:380,388`
  - **Why deferred:** Author-flagged as unverified ("I think..."); needs a grammar-action audit (research task) before any fix is safe to attempt — do NOT attempt a blind fix.

## Honest unsupported-surface note (added 2026-07-17)

`CREATE TRIGGER`/`RAISE()`, `ATTACH`/`DETACH DATABASE`, and `TEMP` tables remain
genuinely **unsupported** in `oxisqlite-core`. Each rejection is a clean,
typed `bail_parse_error!` at parse-translation time — never a stub, panic, or
silent no-op:
- `ast::Stmt::CreateTrigger { .. }` / `ast::Stmt::DropTrigger { .. }` → `bail_parse_error!("CREATE TRIGGER not supported yet")` / `"DROP TRIGGER not supported yet"` (`translate/mod.rs:157,188`). Blocks `RAISE()` (only reachable from trigger bodies).
- `ast::Stmt::Attach { .. }` / `ast::Stmt::Detach(_)` → `bail_parse_error!("ATTACH not supported yet")` / `"DETACH not supported yet"` (`translate/mod.rs:123,179`).
- `TEMP` tables → parse-time rejection, 6 sites in `vdbe/execute/mutate.rs` / `vdbe/execute/txn_schema.rs` (see "Stubs deferred" above); currently 100% dead code with zero live risk.

Each is deferred, not stubbed, because it needs a genuine multi-thousand-line
subsystem before it can be attempted safely: CREATE TRIGGER needs a
trigger-firing engine (OLD/NEW row bindings, cascading fire-on-write hooks
threaded through every INSERT/UPDATE/DELETE path, recursive-trigger depth
limits); ATTACH/DETACH needs a second catalog namespace plus per-connection
multi-database pager plumbing. Neither is a small fix — implementing either
piecemeal risks a half-working, silently-wrong feature, which is worse than
today's clean rejection. `REGEXP` (see above) is fully implemented and is not
part of this unsupported set.

**Pre-existing vendored `todo!()` panic sites (future no-panic hardening item):**
- `translate/expr/value.rs:1680` — `ast::Expr::InTable { .. } => todo!()`, reachable via valid SQL using the (also unimplemented) table-valued `IN sometable` syntax.
- `translate/expr/value.rs:1839` — `ast::Expr::Raise(_, _) => todo!()`, reachable via valid SQL once a `RAISE()` expression is parsed inside any context that reaches value-expression translation (currently only inside trigger bodies, which themselves bail at `CREATE TRIGGER`, but the `todo!()` itself is not gated and would panic if `RAISE()` became reachable through any other parse path).
- Neither is a regression: both are inherited from the vendored parser/translator and pre-date this fork. They should be converted to `bail_parse_error!` (matching the `CREATE TRIGGER`/ATTACH pattern above) as a small, independent hardening task — no new subsystem required, just replacing a panic with a typed parse error.

## Known issues (Miri-verified, added 2026-07-13 by /miri-check)

This session's `/miri-check` pass over the oxisql COOLJAPAN workspace found
and fixed 7 genuine UB/leak bugs in `oxisqlite-core`: a `RawSlice`
single-byte-provenance widening (`storage/sqlite3_ondisk.rs`), a broad
`Page::as_ptr()` shared/exclusive-reference redesign across the storage and
B-tree layers, an unaligned-reference bug (`vector/vector_types.rs`), a
page-cache `Drop`/leak bug (`storage/page_cache.rs`), a `Row.values`
pointer-provenance bug (`vdbe/execute/cursor.rs`), a `register_vtab_module`
aliasing bug (`ext/mod.rs`), and a `newrowid` immutable-cast-to-mut bug
(`vtab.rs`) — all fixed and verified this session. This section documents
what remains.

- [ ] B-tree CellArray design: long-lived borrowed slices across balance_non_root — `crates/oxisqlite-core/storage/btree/page_ops.rs` (`edit_page`, `page_free_array`, `CellArray` struct) and `crates/oxisqlite-core/storage/btree/cursor_write.rs` (`balance_non_root`, cell collection)
  - **Why deferred:** `CellArray.cells` holds 'static-transmuted, exclusive slices borrowed directly from multiple old sibling pages' live buffers, collected up front and read/written across the rest of a ~600-line rebalancing function that also calls ordinary `PageContent` accessors on those same pages while they're being recycled as new/edited siblings. A dedicated Miri-driven fix pass resolved the vast majority of `Page::as_ptr()`'s aliasing issues crate-wide (40+ call sites, shared/exclusive-reference redesign, zero test regressions across 849+790 tests) but this one required either (a) proving no `PageContent` access ever happens on a page while any `CellArray`-sourced slice from it is still needed — very hard to verify by hand given the ordering complexity — or (b) redesigning `CellArray` to use owned copies or just-in-time-resolved (page, offset, len) triples instead of long-lived pre-materialized `&'static mut` references. Both are correctness-critical design decisions, not mechanical Miri fixes. Strong evidence (deliberate page-processing iteration order matching SQLite's own C `balance_nonroot()` convention, plus all fuzz/balance tests passing) suggests this is a type-soundness gap rather than an active data-corruption bug, but it remains confirmed UB per Miri and should not be dismissed.
  - **Severity:** HIGH PRIORITY — confirmed Miri UB in the most correctness-critical algorithm in the engine (B-tree rebalancing); believed data-sound in practice but not type-system-provably-safe. Recommend a dedicated follow-up session with full attention, not a quick patch.
- [ ] Object-graph leak in vtab extension-registration tests (`Rc<ExternalFunc>` + general Connection/Database graph) — exercised via `crates/oxisqlite-core/tests/vtab_order_by_pushdown.rs`, rooted in `register_scalar_function_impl`/`register_aggregate_function_impl` (`crates/oxisqlite-core/ext/mod.rs`) and `register_builtins()`
  - **Why deferred:** Miri's leak-checker (distinct from its UB/Stacked-Borrows checker) reports ~1017 'memory leaked' diagnostics for a single test's Connection/Database object graph (schema, pager, WAL, page cache, B-tree nodes, and 240x `Rc<ExternalFunc>` from every builtin scalar/aggregate function registered via `register_builtins()`) — the whole graph appears never deallocated by process-exit leak-scan time. This was previously invisible because an unrelated UB bug aborted every such test before Miri's leak scan could run; now that the UB is fixed, the leak diagnostic is visible. Not yet root-caused whether this is a genuine reference-cycle leak (e.g. `Rc` cycles that never break) or an artifact of how Miri's leak-checker treats objects still live when a test's `#[test] fn` returns vs. actual `Drop` failing to run. Needs investigation before a fix can be scoped.
  - **Severity:** MEDIUM — resource leak, not memory-unsafety; orthogonal to the UB bugs found and fixed this session.
- [ ] Test-fixture CString leak in register_test_module helper — `crates/oxisqlite-core/translate/insert/tests.rs` (`register_test_module` helper, ~line 474)
  - **Why deferred:** Builds the FFI `VTabModuleImpl.name` field via `CString::new(name).expect(...).into_raw()` and never calls a matching `CString::from_raw` to reclaim it — leaks once per test that uses this helper (3 tests currently: `multi_row_values_insert_reaches_virtual_table_for_every_row`, `mismatched_row_arity_is_rejected_with_a_clear_error`, `single_row_values_insert_still_calls_virtual_table_exactly_once`). Test-only code, not production — low priority, but easy fix (store the `CString` and drop it, or call `CString::from_raw` during test teardown) whenever someone is next in this file.
  - **Severity:** LOW — test-only fixture code, no production impact.

**Coverage gaps:** `oxisqlite-ext` crate has zero unit/integration tests of
its own, so its 60 `unsafe` usages (`vtabs.rs`, `types.rs`,
`vfs_modules.rs`) have never been Miri-exercised — would need test-writing
investment first, which is a stub-check-style task, not a miri-check task.
The `oxisqlite` wrapper crate's 10 `unsafe impl Send`/`Sync` markers
(`Database`/`Connection`/`Statement`/`Rows`/`Row`) have also never been
Miri-exercised since no in-crate test shares these types across real OS
threads. `io/windows.rs` and `io/io_uring.rs` are platform-gated
(Windows-only / Linux-only+feature-gated respectively) and cannot be
Miri-tested on this session's macOS host at all.

## Policy-check findings (added 2026-07-13 by /policy-check)

A full COOLJAPAN compliance sweep ran across the 17-member workspace; checks
#7 (naming), #9 (temp files), #10 (default-features consistency), #11
(hardcoded /kitasan/ paths), and #12 (hardcoded /notebooks/ paths) came back
fully clean; check #6 (file size) was auto-fixed (2 oversized parser files
split via splitrs); this section documents the report-first findings that
need user review/a dedicated future pass.

- [ ] No-unwrap policy backlog: 330 production call sites, 97% in oxisqlite-core — `crates/oxisqlite-core/storage/` (132 hits — highest risk: `storage/sqlite3_ondisk/mod.rs`'s raw on-disk page parsing, e.g. `finish_read_database_header(...).unwrap()` and multiple `read_varint(...).unwrap()` sites — a corrupted or malicious .db file can currently panic the process instead of returning a clean error), `crates/oxisqlite-core/translate/` (82 hits) and `crates/oxisqlite-core/vdbe/` (60 hits), `crates/oxisqlite-core/mvcc/` (21 hits, mostly lock-poisoning-only, lower real risk), `crates/oxisqlite-core/json/jsonb/*_traits.rs` (16 hits), plus `crates/oxisqlite-sqlite3-parser/src/parser/generated/parse.rs` (10 hits — small count but high semantic risk since it directly parses arbitrary user-supplied SQL text; also the hardest to fix cleanly since it's Lemon-generated code, a real fix means patching the grammar/codegen template not a line edit)
  - **Why deferred:** A rigorous Rust-aware classifier (not a sample) found 330 genuine production-code `.unwrap()` call sites workspace-wide (down from an earlier ~867 raw-grep overestimate that didn't distinguish test code) — far too large and judgment-heavy (each site needs a case-by-case decision on the right Result/error type) to fix inline during a policy-check pass. 15 of 17 crates are already at zero production unwraps; the backlog is concentrated almost entirely in oxisqlite-core (320) plus oxisqlite-sqlite3-parser's generated parser (10).
  - **Severity/Priority:** Route to a dedicated `/no-unwrap` pass. Prioritize `storage/` first (raw disk parsing + B-tree write-path correctness — a data-integrity and untrusted-input-DoS surface, not just availability), then `vdbe/`+`translate/` (query execution/compilation), then the generated parser (small count, high semantic risk, hardest to fix), then `mvcc/` and `json/jsonb/*_traits.rs` last (mostly lower-risk lock-poisoning/small-per-file).
- [ ] core-foundation-sys reaches the macOS build unconditionally via chrono's clock feature — `crates/oxisqlite-core/Cargo.toml` (chrono dependency, not feature-gated), `crates/oxisqlite-ext/Cargo.toml`, `crates/oxisqlite-time/Cargo.toml` — same pattern in all three
  - **Why deferred:** chrono's "clock" feature pulls core-foundation-sys transitively on macOS (via iana-time-zone) to read the system timezone. This is Apple's own zero-build-script extern bindings (no C/C++ actually compiled), functionally the macOS sibling of the already-accepted windows-sys OS-boundary exception — but unlike this crate's other -sys-pulling capabilities (native-io, io_uring, load-extension, all correctly feature-gated behind explicit opt-in flags with a documented perf-vs-purity comment), chrono's clock feature has no such gate; it's unconditional. This is the same class of dependency this project has eliminated twice before this same session's history (whoami's objc2-system-configuration binding in the 0.3.0 release, zstd-sys in 0.3.2, both documented in this same TODO.md's git history) but had not yet been traced to this specific source.
  - **Severity/Priority:** Needs a deliberate decision, not a default: either (a) formally extend the project's accepted OS-boundary exception list to name core-foundation-sys explicitly alongside windows-sys, or (b) feature-gate chrono's clock feature in the 3 affected crates behind an opt-in flag mirroring the existing native-io pattern. Low urgency (no actual FFI compiled) but worth resolving deliberately given the project's track record on this exact issue class.
  - **2026-07-17 (Q8) decision:** (b), partially — `oxisqlite-ext` and `oxisqlite-time` moved `clock`→`now` cleanly (no `chrono::Local` use in either beyond a since-fixed `::now()` call). `oxisqlite-core` could not follow: `functions/datetime/mod.rs`'s `Modifier::Localtime`/`Modifier::Utc` (SQLite's `datetime(...,'localtime'/'utc')`) do a genuine `chrono::Local` host-timezone conversion — not a stray `::now()` call — so it stays on `clock`, now with a Cargo.toml comment explaining why. Separately confirmed this doesn't matter for the *workspace's* build regardless: `arrow-arith`/`arrow-array`/`arrow-cast`/`arrow-csv`/`arrow-json` (via `oxisql-datafusion`→`datafusion`→`arrow`) independently request chrono's `clock` feature in their own manifests, so `core-foundation-sys` remains in any full-workspace build no matter what this project does to its own 3 crates. See Q8 above for the full writeup.
- [ ] Workspace policy (*.workspace = true) gaps: 55 dependency lines across 9 members, root-caused to auto-generated manifests — 7 of 17 crates (`oxisqlite`, `oxisqlite-core`, `oxisqlite-ext`, `oxisqlite-macros`, `oxisqlite-sqlite3-parser`, `oxisqlite-time`, `oxisqlite-uuid`) have Cargo-auto-generated `Cargo.toml` files (from the original cargo-package/publish normalization when this SQLite-engine fork was vendored in — no `Cargo.toml.orig` checked in to recover the hand-authored source from, unlike the project's other vendored patch crates which do ship a `.orig`); additionally, `oxisql-postgres`, `oxisql-datafusion`, `oxisql-pool`, `oxisql-migrate`, and `oxisql`'s `[dev-dependencies]` sections use raw `path = "../x"` instead of `workspace = true` even though root already has matching entries
  - **Why deferred:** This isn't a simple find-replace: 6 of the 7 auto-generated crates (`oxisqlite-core`, `oxisqlite-ext`, `oxisqlite-macros`, `oxisqlite-sqlite3-parser`, `oxisqlite-time`, `oxisqlite-uuid`) don't even have a root-level `[workspace.dependencies]` entry to point to yet — only `oxisqlite` itself does. Fixing this properly means: (1) deciding whether to keep or replace the auto-generated-manifest structure entirely, (2) adding ~9 new root-level `workspace.dependencies` entries for the sibling `oxisqlite-*` crates, (3) reconciling at least one real feature-set mismatch found along the way (`criterion`: root has `[html_reports,async_tokio]`, `oxisqlite-core`'s dev-dependency hardcodes `[html_reports,async,async_futures]` — not a trivial swap), and (4) promoting ~20 external third-party deps (`crossbeam-skiplist`, `libc`, `parking_lot`, `rand`, `regex`, etc., all currently hardcoded per-crate, inherited verbatim from the original vendored fork) to root-level entries if full compliance is desired. A genuine multi-step structural project, not a quick fix.
  - **Severity/Priority:** Medium — real policy gap but zero functional/correctness impact (dependency resolution is unaffected either way; this is purely a maintainability/consistency convention). Route to a dedicated `/ultra` pass. LATENT RISK NOTE for whoever picks this up: `crates/oxisqlite-sqlite3-parser/Cargo.toml`'s `env_logger` dependency hardcodes `default-features=false`, while root `Cargo.toml`'s `env_logger` entry has no `default-features` key at all — naively rewriting it to `{ workspace = true, default-features = false }` would silently be ignored by Cargo (default-features overrides are only honored when root explicitly sets the key), re-enabling default features as a regression. Fix root's `env_logger` entry to explicitly set `default-features=false` at the same time, and check whether `oxisqlite-core`'s own `env_logger.workspace=true` dev-dependency (which wants defaults ON) then needs an explicit `default-features=true` override to compensate (confirmed empirically this override direction is honored).

## Release-check findings (added 2026-07-14 by /release-check)

A full `/release-check` pre-release validation pass ran across the
workspace; rustdoc, doctests, packaging, and publish-order all passed
clean (one trivial rustdoc private-link issue was fixed inline during
the pass). This section documents the two things surfaced during that
pass that need dedicated follow-up rather than an inline fix.

- [ ] cargo-audit: 3 vulnerabilities, most severe is an unconditional production-path RSA timing side-channel with no available fix — `crates/oxisql-postgres/Cargo.toml`, `crates/oxisql-mysql/Cargo.toml` (both depend unconditionally on `rustls-rustcrypto`, which pulls `rsa 0.9.10`), plus `crates/oxisqlite-core/Cargo.toml` (dev-only `pprof`→`inferno`→`quick-xml` path)
  - **Why deferred:** cargo audit found: (a) RUSTSEC-2023-0071 (Marvin Attack, MEDIUM 5.9) in `rsa 0.9.10`, reached via `rustls-rustcrypto 0.0.2-alpha` → unconditional non-optional dependency of both `oxisql-postgres` and `oxisql-mysql` — real production exposure, not dev-only. **No fixed upgrade is currently available upstream.** This project already vendors a patched fork at `crates/rustls-rustcrypto-patched` for a different RUSTSEC issue, but that patch does not touch rsa/timing behavior, and — critically — `[patch.crates-io]` overrides never travel with a published crate anyway, so downstream consumers of the published `oxisql-postgres`/`oxisql-mysql` crates get the vulnerable dependency regardless of this workspace's local patch. (b) RUSTSEC-2025-0134 (rustls-pemfile 2.2.0, unmaintained) — also unconditional/production in `oxisql-postgres`. (c) RUSTSEC-2026-0195 and RUSTSEC-2026-0194 (quick-xml 0.26.0, HIGH 7.5 each — unbounded-allocation DoS and quadratic-time parsing) via `pprof` → `inferno` → `quick-xml`, but this is a **dev-dependency only** (`oxisqlite-core`'s benchmark/flamegraph tooling) — never compiled into the published crate, low real-world risk. (d) 4 unmaintained-crate warnings (fxhash, instant — both opt-in via oxisql-embedded's optional `sled` feature, low risk; paste — partly via the same unconditional rustls-rustcrypto path as (a), partly via an opt-in oxisql-datafusion columnar feature). This needs a genuine security-and-architecture decision, not a mechanical fix: options include (1) accepting the rsa/Marvin-Attack risk with a documented justification and a `cargo audit` ignore entry, (2) evaluating whether a different Pure-Rust TLS crypto backend avoids `rsa` entirely, or (3) waiting for upstream `rustls-rustcrypto`/`rsa` to ship a fix and tracking it. None of these should be decided unilaterally by an automated pass.
  - **Severity/Priority:** HIGH — real, unconditional, production-path security exposure with no current fix available. Needs explicit user/maintainer review before this release ships to production users who depend on the postgres or mysql backends. The dev-only quick-xml findings and opt-in-only fxhash/instant findings are comparatively low priority.
  - **2026-07-17 (Q3) decision:** option (1) — accepted with a documented justification. The Marvin timing side-channel only fires on an RSA PKCS#1 v1.5 *private-key* operation; OxiSQL is TLS-client-only (`.with_no_client_auth()` in both `oxisql-postgres`/`oxisql-mysql`) and never performs one. `deny.toml` now carries a `[advisories] ignore = ["RUSTSEC-2023-0071"]` entry with the `cargo tree -i rsa`-backed reachability argument inline; README has a matching "SECURITY: RUSTSEC-2023-0071 (Marvin Attack) is unreachable in OxiSQL" subsection. rustls-pemfile/quick-xml/fxhash/instant are unchanged (not in Q3's scope).
- [ ] check_pub_order.py (external tool at ~/work/) has a false-positive dependency-cycle bug — `~/work/check_pub_order.py` (NOT part of this project — outside this repository)
  - **Why deferred:** The script's own docstring says it only scans `[dependencies]`/`[build-dependencies]`/target-specific dependencies for cycle detection, but its actual code (around line 116) also scans `dev-dependencies`, contradicting its own documented intent. This workspace has legitimate circular *dev*-dependencies (e.g. `oxisql-postgres`'s dev-dependency on `oxisql-pool` for integration tests, while `oxisql-pool` has a normal dependency back on `oxisql-postgres`) — a normal, Cargo-supported, publishable pattern (path-only dev-deps are stripped from published manifests and never block publish ordering), but this tool currently false-flags it as an unresolvable cycle and refuses to produce a topological order at all. Found independently by two separate release-check sub-passes this session, both of which worked around it by re-deriving the correct dependency graph directly from Cargo.toml files rather than trusting the tool. This project's own `pub_oxisql.sh` publish script already independently documents awareness of this exact dev-dependency-cycle nuance in its own comments and correctly handles it via `cargo publish --no-verify`.
  - **Severity/Priority:** LOW for this project (already worked around, publish script already correct) — but worth fixing upstream in `~/work/check_pub_order.py` at some point since it likely false-flags other COOLJAPAN workspaces with the same normal dev-dependency pattern. Out of scope for this project's own TODO in terms of who fixes it, but noted here since it was discovered during this project's release-check.


---

<!-- production-readiness-backlog 2026-07-16 -->
## Production-Readiness Backlog — 2026-07-16

_Consolidated from static audit + Opus adversarial bug-hunt (48 verified defects across noffi) + baseline nextest/clippy + design investigation. See `../NOFFI_PRODUCTION_BACKLOG.md` for the full cross-project list and severity/model legend. Not implemented; no commits._

**Confirmed bugs — Opus-verified (all in VENDORED oxisqlite-core on-disk parser; corrupt/malicious .db → panic-DoS):**
- [ ] **S · high** `storage/sqlite3_ondisk/mod.rs:1190` — `read_varint` indexes `buf[8]` unchecked after the 0..8 continuation loop → OOB panic on truncated 9-byte varint (pervasively reachable via rowid/payload decode). R2/N0
- [ ] **S · high** `storage/sqlite3_ondisk/mod.rs:640` — `cell_get_raw_region` slices at unvalidated 16-bit `cell_pointer` + `.unwrap()`s `read_varint`; crashes even `integrity_check` (which is meant to *report* corruption). R2/N0
- [ ] **S · high** `storage/sqlite3_ondisk/mod.rs:868` — `read_btree_cell` uses attacker cell-pointer to index page and compute `to_read = page.len() - pos` → OOB / underflow slice panic. R2/N0
- [ ] **S · med** `storage/sqlite3_ondisk/mod.rs:579` — interior/leaf rowid/left-child fast-path helpers index page at unvalidated cell_pointer (hit during normal cursor nav). R2/N0
- [ ] **S · med** `storage/sqlite3_ondisk/mod.rs:1019` — `read_record` uses `assert!` on attacker header_size (asserts active in release) → panic. R2/N0
- [ ] **S · med** `storage/sqlite3_ondisk/mod.rs:933` — `read_payload` computes `unread[cell_len - 4]`; <4 bytes remaining → usize underflow → huge-index panic. R2/N0
- [ ] **S · med** `storage/btree/page_ops.rs:796` — `free_cell_range` computes `(pc - end) as u8` before the `end > pc` guard → debug subtraction-overflow panic. R2/N0
- Note: these 8 corroborate design Q6 (unwrap Wave 1, on-disk parsing first). Harden with existing `LimboError::Corrupt`/`return_corrupt!`; add truncated/garbage `.db` regression corpus.

**Designed production-integrity work (design_oxisql.md):**
- [ ] **S/hard/Opus · Q5** CellArray UB fix — owned sibling-page snapshots (delete `to_static_buf` transmute in `page_ops.rs:482-565`; `balance_non_root`). Miri gate incl. issue-1203 tests.
- [ ] **S/med · Q1→Q2** publish-hole fix: `oxitls-rustcrypto-provider` fork (host in oxitls), package-rename in oxisql-postgres/mysql, swap `oxitls`→`oxitls-webpki-roots` (cuts 2nd rsa path), delete patch entry + vendored dir.
- [x] **S/easy · Q3** Marvin (RUSTSEC-2023-0071) = UNREACHABLE in oxisql paths (client-only, no_client_auth, no RSA kx) → deny.toml ignore + justification + README SECURITY note. DONE 2026-07-17: `deny.toml` `[advisories] ignore` entry with a `cargo tree -i rsa`-backed justification; README "SECURITY: RUSTSEC-2023-0071 (Marvin Attack) is unreachable in OxiSQL" subsection under Pure Rust — FFI eliminated.
- [x] **B/easy · Q4** downstream `[patch]` recipe docs for whoami+zstd (removal proven impossible). DONE 2026-07-17: README "Downstream `[patch.crates-io]` requirement (whoami / zstd)" subsection under Installation — explains Cargo's per-workspace-root `[patch]` scoping and gives the exact recipe to copy into a consuming workspace.
- [ ] **S/med · Q7** unwrap Waves 2–4: btree (after Q5) → translate/vdbe → parser/mvcc/json (330 prod sites, 320 vendored). Surgical (keep fork diffable vs limbo 0.0.22).
- [x] **B/easy · Q8** chrono `clock`→`now`, `Local::now`→`Utc::now` (3 Cargo.toml + 9 sites); drops core-foundation-sys from oxisqlite-*/sqlite-compat closure. PARTIALLY DONE 2026-07-17 — all 10 real `Local::now()` sites (6× `io/*.rs` `Clock::now()`, 2× `functions/datetime/mod.rs` 'now' time-value parsing, 1× `oxisqlite-ext/src/vfs_modules.rs::get_current_time`, 1× test) switched to `Utc::now()` (timestamps are UTC-correct now, not host-locale-dependent). `oxisqlite-time` and `oxisqlite-ext` Cargo.toml moved `clock`→`now` cleanly (zero remaining `chrono::Local` usage in either). **`oxisqlite-core` could NOT be moved to `now`-only**: `functions/datetime/mod.rs`'s `Modifier::Localtime`/`Modifier::Utc` (lines ~385–410) implement SQLite's `datetime(...,'localtime')`/`'utc'` modifiers via a genuine `chrono::Local` host-timezone conversion — this is real functionality, not a stray `::now()` call, and dropping `clock` there breaks `cargo build -p oxisqlite-core` standalone (confirmed by reproduction). Left `oxisqlite-core` on `clock` with a Cargo.toml comment explaining why. Separately confirmed (`cargo tree -i core-foundation-sys`) that even a fully `now`-only oxisqlite-core would NOT remove core-foundation-sys from a full-workspace build regardless, because `arrow-arith`/`arrow-array`/`arrow-cast`/`arrow-csv`/`arrow-json` (pulled in by `oxisql-datafusion`→`datafusion`→`arrow`) each independently request chrono's `clock` feature in their own manifests — an upstream constraint outside this workspace's control. Net effect: `oxisqlite-time`/`oxisqlite-ext` standalone builds are now core-foundation-sys-free; `oxisqlite-core` standalone and any full-workspace build still pull it in (for a real reason in the former case, an upstream one in the latter).
- [x] **B/easy · Q9** remove vendored `rustls-rustcrypto-patched/.github/{workflows,dependabot}` (moot if Q2 deletes dir). DONE 2026-07-17: `crates/rustls-rustcrypto-patched/.github/` removed (Q2 has not landed; the vendored patch dir itself is still present and still required for RUSTSEC-2026-0104).
- [x] **B/easy · Q10** oxistore-columnar pin re-check per ledger rule (arrow 58 vs 59); don't bump alone. DONE 2026-07-17: ledger `until` (2026-08-02) not yet reached; re-verified via `cargo info` that datafusion (54.0.0) and arrow (58.3.0 default) are still crates.io's max-compatible versions and oxistore-columnar's latest (0.2.0) still bundles arrow/parquet 59 — conflict unchanged. No bump; refreshed the Cargo.toml comment + `~/work/cargo-upgrade/oxisql.toml` ledger `recheck` note only.
**Vendored major stubs — USER-APPROVED to implement (was "track upstream"; hard/Opus, design-first each):**
- [ ] **A · Q11a** CREATE TRIGGER + trigger execution incl. RAISE() (`translate/expr.rs:2154`)
- [ ] **A · Q11b** ATTACH DATABASE (`expr.rs:670`)
- [x] **A · Q11c** REGEXP operator (`expr.rs:2582`) — DONE, shipped in 0.3.3 (2026-07-17): `X REGEXP Y` (== `regexp(Y, X)`) is fully implemented in `translate/expr/binary_emit.rs`/`translate/expr/value.rs`, covered by `tests/regexp_operator.rs`; see the "Stubs deferred" `oxisqlite-core` entry above (already marked done) and CHANGELOG.md's 0.3.3 Added section. This line was inconsistently left unchecked when that entry was updated — now reconciled.
- [ ] **A · Q11d** temp / TEMP table support (6 sites)
