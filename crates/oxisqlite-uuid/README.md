# oxisqlite-uuid

UUID SQL function extension for the C-free **oxisqlite** engine — a Pure-Rust
fork of [limbo](https://github.com/tursodatabase/limbo) 0.0.22, internal to the
OxiSQL workspace.

This crate provides the engine's UUID SQL functions, registered through the
oxisqlite extension API.

- **Role:** UUID SQL function extension.
- **Approx LOC:** ~126.
- **Pure Rust / no C:** 100% Rust. No C allocator, no C parser generator, no
  `cc` / `build.rs`.
- **Internal:** private member of the OxiSQL workspace; not published separately.

## Fork lineage & licensing

Part of a COOLJAPAN C-free fork of limbo 0.0.22 (MIT). Full attribution and
per-component licensing are recorded in the repo-root [`/NOTICE`](../../NOTICE).

Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan). COOLJAPAN code is licensed
under **Apache-2.0**; upstream limbo code remains under MIT (see
[`/NOTICE`](../../NOTICE)).

Part of the [OxiSQL](../../README.md) workspace.
