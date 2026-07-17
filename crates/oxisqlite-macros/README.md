# oxisqlite-macros

Procedural macros backing the extension API of the C-free **oxisqlite** engine —
a Pure-Rust fork of [limbo](https://github.com/tursodatabase/limbo) 0.0.22,
internal to the OxiSQL workspace.

This crate implements the proc-macros that the `oxisqlite-ext` extension API is
built on:

- the `scalar` attribute macro (scalar SQL functions),
- `AggregateDerive` (aggregate SQL functions),
- `VTabModuleDerive` (virtual tables),
- `VfsDerive` (VFS modules), and
- the `register_extension!` function-like macro that wires the above into the
  engine.

- **Role:** proc-macros for the oxisqlite extension API.
- **Version:** 0.3.3 (2026-07-17).
- **Tests:** 0 dedicated `nextest` unit tests in this crate; validated
  indirectly through `oxisqlite-core`'s integration test suite (which depends
  on `oxisqlite-ext`, which in turn depends on this crate). 3 doctests are
  marked `ignore` — proc-macro crates typically cannot doctest their own
  macros directly (verified 2026-07-17).
- **Approx LOC:** ~908 (tokei `src/`).
- **Pure Rust / no C:** 100% Rust. No C allocator, no C parser generator, no
  `cc` / `build.rs`.
- **Internal:** private member of the OxiSQL workspace; not published separately.

## COOLJAPAN change vs upstream limbo

The registration macro no longer injects a non-static `#[global_allocator]`
(upstream wired in a C allocator there for dynamically-loaded extensions). That
injection was removed so the fork stays C-free; registering an extension pulls in
no C allocator and no global-allocator override.

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Full attribution and
per-component licensing are recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
