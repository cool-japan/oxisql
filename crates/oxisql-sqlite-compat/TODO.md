# oxisql-sqlite-compat TODO

## Wave 38A (current)

- [x] Create crate with limbo 0.0.22 dependency
- [x] `error.rs` — `SqliteCompatError` mapping to `OxiSqlError`
- [x] `types.rs` — value conversion + `$N → ?` param rewriter
- [x] `connection.rs` — `SqliteConnection`, `SqliteTransaction`, `SqlitePrepared`
- [x] `Connection` trait impl (execute, query, transaction, execute_batch, ping, prepare, tables, columns, indexes, foreign_keys)
- [x] Integration tests (CRUD, transactions, schema introspection, file persistence)
- [x] Added to workspace Cargo.toml
- [x] `SqliteCompatPool` added to `oxisql-pool` (sqlite-compat feature)
- [x] Wired into `oxisql` facade (sqlite feature)

## Known limbo 0.0.22 limitations

- Affected-row count uses `SELECT changes()` round-trip (no native API)
- Named parameters (`$name` / `:name`) — supported at the oxisql-core layer via
  `execute_named` / `query_named` (automatic rewrite to positional `?`); the
  limbo-level named param binding remains `todo!()` but that path is bypassed
- `Statement::reset()` does not reset `Program::n_change` — so reusing a cached
  statement via `stmt.execute()` produces wrong `changes()` counts.  The LRU
  cache is populated (infrastructure in place) but execution still goes through
  `conn.execute()` which prepares fresh.  Will become fully effective once limbo
  exposes a complete reset (expected in 0.1+).
- ROLLBACK is rejected by limbo at parse time.  `SqliteTransaction::rollback()`
  now returns `Err(OxiSqlError::Other("ROLLBACK is not supported by the limbo
  0.0.22 engine …"))` instead of a cryptic parse error.
- Savepoints not supported
- `execute_batch` previously used naive `;` split — now replaced with token-aware state machine

## Future improvements

- [ ] Upgrade to limbo 0.1+ for stable public API when released
- [x] Statement-cache infrastructure added at the oxisql layer (LRU, capacity
  128, keyed by rewritten SQL).  Full parse-skip optimisation blocked on limbo
  fixing `Statement::reset()` to also clear `Program::n_change`.
- [x] `rollback()` returns a clear, honest error instead of a raw limbo parse
  error.  Full rollback blocked on upstream limbo support.
- [x] Named parameters (`$name` / `:name`) — handled at the oxisql-core layer
  via `execute_named` / `query_named` default trait methods, which rewrite named
  params to positional `?` before forwarding to limbo.  The limbo-level named
  param binding path (`todo!()`) is bypassed entirely.
- [ ] Add savepoint support once limbo implements it
- [x] DataFusion registration for SQLite tables
- [x] `execute_batch` via token-aware state-machine split (handles `;` in literals, identifiers, comments)

## Future improvements (continued)

- [x] Implement foreign_keys() via sqlite_master DDL parsing (closes PRAGMA foreign_key_list gap)
