# oxisqlite-core

The engine core of the C-free **oxisqlite** engine — a Pure-Rust fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the OxiSQL
workspace.

This is the heart of the engine that powers the `oxisql-sqlite-compat` backend.
It contains:

- the **VDBE** bytecode interpreter,
- **B-tree** storage, the **pager**, and **WAL**,
- **SQL → bytecode** translation,
- **MVCC** transaction machinery,
- **JSON / JSONB** support, and
- the SQL **built-in functions**.

- **Role:** engine core (interpreter, storage, translation, functions).
- **Approx LOC:** ~62,000.
- **Pure Rust / no C:** 100% Rust. No C allocator, no C parser generator, no
  `cc` / `build.rs`. `CC=/usr/bin/false cargo build` succeeds.
- **Internal:** private member of the OxiSQL workspace; not published separately.

## COOLJAPAN changes vs upstream limbo

Two notable modifications were made on top of limbo 0.0.22:

1. **Full-transaction `ROLLBACK`.** Upstream lacked complete rollback of an open
   write transaction; it was ported from `turso_core` 0.7.0-pre.5 (MIT). The
   change spans `translate/rollback.rs`, `vdbe/execute.rs`, `storage/wal.rs`,
   and `storage/pager.rs`.

2. **Pure-Rust Julian-day conversion.** The GPL-licensed `julian_day_converter`
   dependency was removed and replaced by an inline, `chrono`-based
   `functions/julian_day.rs`, keeping the crate's date/time functions compatible
   while preserving Apache-2.0 licensing.

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Full attribution, the
upstream commit, the `turso_core` ROLLBACK provenance, and per-component
licensing are recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
