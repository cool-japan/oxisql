# oxisqlite

Top-level facade of the C-free **oxisqlite** engine — a Pure-Rust fork of
[limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the OxiSQL
workspace.

This crate re-exports the engine's public surface — `Connection`, `Statement`,
positional and named parameter binding (`params` / `params_from_iter` /
`named_params!`, covering `:name` / `@name` / `$name` / `#name` placeholder
forms), and the value/`Value` types — and is the entry point consumed by the
`oxisql-sqlite-compat` backend. It is a thin, ergonomic wrapper over
`oxisqlite-core`; all bytecode execution, storage, and SQL processing live
there.

`Database::open_from_bytes(bytes)` additionally opens a shareable `Database`
directly from an in-memory SQLite image — e.g. `include_bytes!`, `VACUUM INTO`,
or `sqlite3_serialize()` output — mirroring SQLite's `sqlite3_deserialize()`.
No temporary file is involved, so it works under WASI, in a browser, or on a
read-only filesystem; the returned `Database` can be `connect()`ed multiple
times, and all connections share the same preloaded image. Malformed input
(too short, wrong magic, or an invalid page size) is a typed error, never a
panic.

- **Role:** engine facade (`Connection`, `Statement`, params, value types).
- **Version:** 0.3.3 (2026-07-17).
- **Tests:** 38 passing (default features and `--all-features`), 0 failed
  (verified 2026-07-17).
- **Approx LOC:** ~1,900 (tokei `src/`; up from ~973 pre-0.3.3 — this release
  added `open_from_bytes` and full named-parameter binding with
  `Cow<'static, str>` keys).
- **Pure Rust / no C:** 100% safe, portable Rust. No C allocator, no C parser
  generator, no `cc` / `build.rs`. `CC=/usr/bin/false cargo build` succeeds.
- **Internal:** engine-internal member of the OxiSQL workspace (the entry
  point consumed by `oxisql-sqlite-compat`); independently published on
  crates.io like every other `oxisqlite-*` crate (no `publish = false`; live
  since v0.1.0, 2026-06-11).

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
