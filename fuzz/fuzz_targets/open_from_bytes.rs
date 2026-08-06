#![no_main]

//! Fuzz `SqliteConnectionBlocking::open_from_bytes`: reinterprets arbitrary
//! input bytes as a full on-disk SQLite database image (header, pager,
//! B-tree pages, cell/record encoding, overflow chains).
//!
//! This is the documented attack surface for the on-disk parser: eight
//! panic-DoS bugs (OOB slice indexing, `usize` underflow, a release-active
//! `assert!` on attacker-controlled header sizes) were fixed here by
//! converting raw indexing into typed `LimboError::Corrupt` returns. This
//! target's only invariant is that no input — however malformed — should
//! ever panic the process; a corrupt image must come back as `Err`.

use libfuzzer_sys::fuzz_target;
use oxisql_sqlite_compat::blocking::SqliteConnectionBlocking;

fuzz_target!(|data: &[u8]| {
    // The `Ok`/`Err` result is deliberately discarded: both are valid
    // outcomes for arbitrary input. A panic (caught by libFuzzer as a
    // crash) is the only failure mode this target is looking for.
    let _ = SqliteConnectionBlocking::open_from_bytes(data);
});
