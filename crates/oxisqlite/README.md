# oxisqlite

Top-level facade of the C-free **oxisqlite** engine — a Pure-Rust fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the OxiSQL
workspace.

This crate re-exports the engine's public surface — `Connection`, `Statement`,
`params` / `params_from_iter`, and the value/`Value` types — and is the entry
point consumed by the `oxisql-sqlite-compat` backend. It is a thin, ergonomic
wrapper over `oxisqlite-core`; all bytecode execution, storage, and SQL
processing live there.

- **Role:** engine facade (`Connection`, `Statement`, params, value types).
- **Approx LOC:** ~973.
- **Pure Rust / no C:** 100% safe, portable Rust. No C allocator, no C parser
  generator, no `cc` / `build.rs`. `CC=/usr/bin/false cargo build` succeeds.
- **Internal:** this is a private member of the OxiSQL workspace and is not
  published separately.

## Fork lineage & licensing

This is part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). The three C
touchpoints from upstream (the C allocator, the `lemon.c` parser generator, and
the `built`/`git2` build-info probe) were removed. Full attribution, the
upstream commit, and per-component licensing are recorded in the repo-root
[`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
