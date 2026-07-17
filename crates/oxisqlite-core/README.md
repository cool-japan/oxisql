# oxisqlite-core

The engine core of the C-free **oxisqlite** engine — a Pure-Rust fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the OxiSQL
workspace.

This is the heart of the engine that powers the `oxisql-sqlite-compat` backend.
It contains:

- the **VDBE** bytecode interpreter,
- **B-tree** storage, the **pager**, and **WAL**,
- **SQL → bytecode** translation with a System-R cost-based optimizer,
- **ANALYZE** statement + `sqlite_stat1` cardinality statistics,
- **MVCC** transaction machinery (full `ROLLBACK`, `SAVEPOINT`),
- **UPSERT** `ON CONFLICT DO UPDATE/DO NOTHING` with `excluded.*`,
- **JSON / JSONB** support, and
- the SQL **built-in functions**.

- **Role:** engine core (interpreter, storage, translation, functions).
- **Version:** 0.3.3 (2026-07-17).
- **Tests:** 888 passing with default features; 885 passing `--all-features`
  (which already enables `index_experimental` — the small delta is a couple of
  feature-gated tests moving crates, not a regression); 0 failed (verified
  2026-07-17).
- **Approx LOC:** ~87,900 (tokei, all `.rs` under this crate incl. tests; up
  from ~86,400 prior release).
- **Pure Rust / no C:** 100% Rust. No C allocator, no C parser generator, no
  `cc` / `build.rs`. `CC=/usr/bin/false cargo build` succeeds.
- **Known debt:** ~315 `.unwrap()` calls remain in production code paths
  (concentrated in `storage/`, `translate/`, `vdbe/`), inherited from upstream
  `limbo` and not new this release. This release converted several on-disk
  page-corruption panics to typed `LimboError::Corrupt` errors, but most
  remaining sites are untouched; tracked in the workspace-root `TODO.md`.
- **Internal:** engine-internal member of the OxiSQL workspace (consumed via
  `oxisql-sqlite-compat`); independently published on crates.io like every
  other `oxisqlite-*` crate (no `publish = false`; live since v0.1.0, 2026-06-11).

## COOLJAPAN changes vs upstream limbo 0.0.22

Notable additions on top of the original fork:

1. **Full-transaction `ROLLBACK`.** Ported from `turso_core` 0.7.0-pre.5 (MIT).
   Spans `translate/rollback.rs`, `vdbe/execute/txn_schema.rs`, `storage/wal.rs`,
   and `storage/pager.rs`.

2. **`SAVEPOINT` / `RELEASE` / `ROLLBACK TO SAVEPOINT`.** Full nested savepoint
   semantics with WAL-based page-state restoration; pager savepoint stack.

3. **`ANALYZE` statement + System-R optimizer.** `translate/analyze.rs` generates
   bytecode that writes `sqlite_stat1` rows; `statistics.rs` loads them into a
   `SchemaStats` side-map; `translate/optimizer/cost.rs` uses real selectivity when
   stats are present (backwards compatible — un-analyzed DBs unchanged).

4. **UPSERT `ON CONFLICT DO UPDATE / DO NOTHING`.** `translate/upsert.rs` handles
   all forms including `excluded.*`, per-target conflict routing, and the
   `index_experimental` unique-index path. `DO UPDATE SET` can no longer
   target a `GENERATED ALWAYS AS (...)` column — rejected the same way a
   plain `UPDATE` rejects it (this release).

5. **Schema-cookie invalidation + `SchemaChanged`.** DDL bumps the schema cookie;
   `op_transaction` verifies it; stale statements raise `LimboError::SchemaChanged`.

6. **Module splits via `splitrs`.** `schema.rs` (1,920 lines) → `schema/` (7 files);
   `vdbe/execute.rs` (8,361 lines) → `vdbe/execute/` (10 files); this release
   added `json/jsonb.rs` → `json/jsonb/`, `storage/pager.rs` →
   `storage/pager/`, `functions/datetime.rs` → `functions/datetime/`,
   `translate/expr.rs` → `translate/expr/`, `translate/insert.rs` →
   `translate/insert/`, `types.rs` → `types/`, and `util.rs` → `util/` — all
   to stay under the workspace's 2000-line-per-file policy, no functional
   change. Also this release: `NATURAL JOIN` common-column detection
   rewritten from an O(n²) nested loop to a `HashSet` precomputation
   (`translate/planner.rs`) — same results, better performance on wide joins.

7. **Pure-Rust Julian-day conversion.** GPL `julian_day_converter` removed;
   replaced by inline `functions/julian_day.rs`.

8. **`CREATE TABLE ... AS SELECT` (CTAS) + per-constraint `ON CONFLICT`.**
   `translate/schema.rs` executes the SELECT and populates the new table;
   column- and table-level `UNIQUE` constraints now carry their own
   `ON CONFLICT <action>` resolution (`schema/column.rs`, `schema/table.rs`,
   `schema/index.rs`).

9. **Compound-SELECT `INTERSECT` / `EXCEPT`, plus compound `LIMIT`/`OFFSET`/
   `ORDER BY` and a shared `WITH`.** `translate/compound_select.rs`. All reuse
   the ephemeral-unique-index dedupe machinery `UNION` already used, so — like
   UPSERT's unique-index path above — they require the `index_experimental`
   feature.

10. **`FROM (t1 JOIN t2 ON ...)` parenthesized joins** as a pure grouping
    construct (`translate/planner.rs`), **and virtual-table `ORDER BY`
    pushdown** — `xBestIndex` now receives the query's real `ORDER BY`
    columns and the core elides its own sorter when the vtab reports
    `order_by_consumed` (`translate/main_loop.rs`).

11. **`x IN (...)` / `NOT IN (...)` as a value expression**, e.g.
    `SELECT x IN (1,2,3) FROM t`, not just a top-level `WHERE`/`JOIN ON`
    condition (`translate/expr/value.rs`).

12. **Open a database from an in-memory byte buffer.**
    `Database::open_from_bytes(bytes, enable_mvcc)` (`lib.rs`) copies a
    `sqlite3_serialize()`-style image into a fresh in-memory page store
    (`MemoryFile::from_bytes`, now public — `io/memory.rs`) with no temp
    file; deliberately **not** gated by the `fs` feature, so it works on
    `wasm32`/WASI and read-only filesystems. Malformed input (too-short
    header, bad magic, invalid page size) returns a typed error and never
    panics.

13. **Windows file locking (`io/windows.rs`).** Real `LockFileEx` /
    `UnlockFileEx`-backed locking via a new `windows-sys` dependency, gated
    to `cfg(target_os = "windows")` so it never enters the dependency graph
    on other targets — previously an `unimplemented!()` stub.

14. **`REGEXP` operator / `regexp()` function.** `X REGEXP Y` (equivalently
    `regexp(Y, X)`) is now a recognized function (`function.rs`,
    `translate/expr/binary_emit.rs`) — an unanchored `regex`-crate search,
    three-valued `NULL` handling, and a clean constraint error (never a
    panic) on a malformed pattern.

15. **Wider `printf()` specifier coverage (`functions/printf.rs`).** `%i`
    (alias for `%d`), `%x`/`%X` (hex), `%o` (octal), `%c` (first character of
    the argument), and `%e`/`%E` (C-style scientific notation) now work;
    flag/width/precision modifiers (e.g. `%05d`, `%.3s`) remain a documented
    `TODO`.

This release also closed a substantial list of correctness bugs. Data-safety:
a B-tree index-page balance bug that could silently corrupt a page while
rebalancing an interior index page (`insert_into_cell`, `storage/btree/`);
index-based access for `DELETE`/`UPDATE`, disabled workspace-wide since an
upstream-reported corruption bug
([tursodatabase/limbo#1714](https://github.com/tursodatabase/limbo/issues/1714)),
now re-enabled behind a provable-safety check instead of an unconditional
table-scan fallback; an MVCC transaction-removal race and an MVCC
commit-ordering durability gap (a crash between marking a transaction visible
and persisting it could previously lose data); WAL checkpoint bookkeeping
that could leak unbounded frame-cache state across repeated partial
checkpoints; and on-disk page-corruption handling that now returns
`LimboError::Corrupt` instead of panicking on an untrusted cell/freeblock
pointer or page-type byte. SQL behavior: `UPDATE ... RETURNING` (previously
silently returned zero rows), `ALTER TABLE RENAME` on schemas containing
views/triggers/vtabs, virtual-table multi-row `INSERT` (previously kept only
the last row), repeated-CTE-reference resolution, `SELECT COUNT(*)` on an
MVCC-backed cursor (previously `todo!()`), wide-row varint record headers,
`||`/`concat()`/`QUOTE()` BLOB→TEXT coercion, `strftime()` `%J` and
pad-override flags, and `PRAGMA auto_vacuum = FULL` root-page tracking. A
Miri-driven memory-safety pass also reworked 40+ page-access call sites
across the storage/B-tree layers to remove simultaneous-mutable-alias
hazards, plus a pointer-provenance fix in BLOB/TEXT materialization and a
page-cache leak fix. See the repo-root `CHANGELOG.md`'s `[0.3.3]` entry for
the complete list, and the workspace-root `TODO.md` for the highest-priority
Miri issue that remains open (B-tree rebalancing borrow lifetimes in
`storage/btree/page_ops.rs`/`cursor_write.rs`, HIGH priority, believed
data-sound in practice but not yet fixed).

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Full attribution, the
upstream commit, the `turso_core` ROLLBACK provenance, and per-component
licensing are recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
