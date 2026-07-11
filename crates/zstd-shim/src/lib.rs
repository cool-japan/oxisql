//! Pure-Rust `zstd`-API shim backed by `oxiarc-zstd`.
//!
//! NOT the upstream `zstd` crate. A minimal source-compatible replacement wired
//! into oxisql via `[patch.crates-io]` so the `--all-features` dependency closure
//! carries no `zstd-sys` C FFI (COOLJAPAN Pure Rust Policy v2). Implements only the
//! surface `arrow-ipc` uses: `bulk::{Compressor, Decompressor}` and
//! `DEFAULT_COMPRESSION_LEVEL`.

/// Mirrors `zstd::DEFAULT_COMPRESSION_LEVEL` (zstd's `CLEVEL_DEFAULT` = 3).
pub const DEFAULT_COMPRESSION_LEVEL: i32 = 3;

pub mod bulk;
