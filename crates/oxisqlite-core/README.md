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
- **Version:** 0.2.0 (2026-06-17).
- **Tests:** 636 passing (all-features), 640 with `index_experimental`, 13 skipped.
- **Approx LOC:** ~82,000 (after execute/ + schema/ module splits).
- **Pure Rust / no C:** 100% Rust. No C allocator, no C parser generator, no
  `cc` / `build.rs`. `CC=/usr/bin/false cargo build` succeeds.
- **Internal:** private member of the OxiSQL workspace; not published separately.

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
   `index_experimental` unique-index path.

5. **Schema-cookie invalidation + `SchemaChanged`.** DDL bumps the schema cookie;
   `op_transaction` verifies it; stale statements raise `LimboError::SchemaChanged`.

6. **Module splits via `splitrs`.** `schema.rs` (1,920 lines) → `schema/` (7 files);
   `vdbe/execute.rs` (8,361 lines) → `vdbe/execute/` (10 files).

7. **Pure-Rust Julian-day conversion.** GPL `julian_day_converter` removed;
   replaced by inline `functions/julian_day.rs`.

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Full attribution, the
upstream commit, the `turso_core` ROLLBACK provenance, and per-component
licensing are recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
