# oxisql-cache — SQL query-result and prepared-plan caching for OxiSQL

[![Crates.io](https://img.shields.io/crates/v/oxisql-cache.svg)](https://crates.io/crates/oxisql-cache)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

LRU + TTL caching for SQL query result sets and prepared-statement plans, built
on top of [`oxistore-cache`](https://crates.io/crates/oxistore-cache)'s
Pure-Rust LRU primitive.

**Status: Stable.**

## What it is

`oxisql-cache` provides two focused caches for `oxisql`-backed connections:

- [`SqlQueryCache`] — caches `RowSet` results, keyed by normalised SQL text.
- [`SqlPlanCache<P>`] — caches an opaque, caller-supplied prepared-statement
  representation `P` (a compiled plan, a byte buffer, or any `Clone` type).

Both support optional per-entry TTL and are wrapped by [`CachedQueryRunner`], a
read-through adapter that turns any `FnMut(&str) -> Result<RowSet, E>`
executor into a caching one.

This crate supersedes the former `sql` feature of `oxistore-cache`. The
SQL-layer cache types now live on the `oxisql` side of the dependency graph —
`oxisql-cache` depends on `oxistore-cache`, not the other way around — which
breaks the cross-repo dependency cycle that previously existed between the
`oxisql` and `oxistore` repositories.

Neither cache is thread-safe on its own; wrap one in
[`oxistore_cache::sync::SyncCache`] or [`oxistore_cache::sharded::ShardedCache`]
for concurrent use.

## Installation

```toml
[dependencies]
oxisql-cache = "0.4.1"
```

MSRV 1.89 · edition 2021 · Apache-2.0 · `#![forbid(unsafe_code)]`.

## Quick start

```rust
use oxisql_core::{Row, RowSet, Value};
use oxisql_cache::SqlQueryCache;

let mut cache = SqlQueryCache::new(256);
let rows = vec![Row::new(vec!["id".into()], vec![Value::I64(1)])];
let rs = RowSet::from_rows(rows);
cache.put("SELECT id FROM t WHERE id = 1", rs.clone());

// Equivalent queries (different whitespace/case) hit the same entry.
assert!(cache.get("select  id  from  t  where  id = 1").is_some());
```

### Read-through caching with `CachedQueryRunner`

```rust
use oxisql_core::{Row, RowSet, Value};
use oxisql_cache::CachedQueryRunner;

let mut runner = CachedQueryRunner::new(32, |sql: &str| -> Result<RowSet, String> {
    // Simulated DB hit.
    Ok(RowSet::from_rows(vec![Row::new(vec!["n".into()], vec![Value::I64(1)])]))
});

let r1 = runner.run("SELECT 1").unwrap();
let r2 = runner.run("SELECT 1").unwrap(); // served from cache
assert_eq!(r1.len(), r2.len());
assert_eq!(runner.hits(), 1);
assert_eq!(runner.misses(), 1);
```

## Key API

| Item | Description |
|------|-------------|
| `SqlQueryCache::new(capacity)` | New LRU query-result cache |
| `SqlQueryCache::get(sql)` / `put(sql, rows)` / `put_with_ttl(sql, rows, ttl)` | Lookup / insert / insert-with-expiry |
| `SqlQueryCache::invalidate(sql)` / `clear()` / `contains(sql)` | Explicit eviction and membership check |
| `SqlQueryCache::stats()` | [`QueryCacheStats`] snapshot (hits, misses, len, cap) |
| `SqlPlanCache::<P>::new(capacity)` | New LRU plan cache, generic over the plan type `P` |
| `SqlPlanCache::get/put/put_with_ttl/invalidate/clear/contains` | Same shape as `SqlQueryCache`, returning `&P` on lookup |
| `SqlPlanCache::hits()` / `misses()` / `hit_rate()` | Running hit/miss counters |
| `CachedQueryRunner::new(capacity, executor)` | Wrap an executor closure with a `SqlQueryCache` |
| `CachedQueryRunner::run(sql)` / `run_with_ttl(sql, ttl)` | Execute-or-serve-from-cache |
| `CachedQueryRunner::invalidate(sql)` / `clear()` / `hits()` / `misses()` / `stats()` | Cache management + counters |

Cache keys are normalised SQL text: leading/trailing whitespace trimmed,
internal whitespace runs collapsed to a single space, and ASCII letters
upper-cased — so `select id from t` and `SELECT  ID  FROM  T` share the same
entry.

## Test coverage

**23 unit tests + 3 doc tests pass**, 0 failed, covering key normalisation,
hit/miss accounting, TTL expiry, resizing, invalidation, and the
`CachedQueryRunner` read-through path (including executor-error propagation).

## See also

This crate is part of the OxiSQL Pure-Rust workspace. See the
[workspace README](../../README.md). The underlying eviction primitives (LRU,
ARC, LFU, W-TinyLFU) live in
[`oxistore-cache`](https://crates.io/crates/oxistore-cache).

## License

Apache-2.0 — Copyright © 2024–2026 COOLJAPAN OU (Team Kitasan).

[`SqlQueryCache`]: https://docs.rs/oxisql-cache/latest/oxisql_cache/struct.SqlQueryCache.html
[`SqlPlanCache<P>`]: https://docs.rs/oxisql-cache/latest/oxisql_cache/struct.SqlPlanCache.html
[`CachedQueryRunner`]: https://docs.rs/oxisql-cache/latest/oxisql_cache/struct.CachedQueryRunner.html
[`QueryCacheStats`]: https://docs.rs/oxisql-cache/latest/oxisql_cache/struct.QueryCacheStats.html
[`oxistore_cache::sync::SyncCache`]: https://docs.rs/oxistore-cache/latest/oxistore_cache/sync/struct.SyncCache.html
[`oxistore_cache::sharded::ShardedCache`]: https://docs.rs/oxistore-cache/latest/oxistore_cache/sharded/struct.ShardedCache.html
