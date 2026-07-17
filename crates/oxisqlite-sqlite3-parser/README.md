# oxisqlite-sqlite3-parser

Pure-Rust SQLite3 SQL parser and lexer — a COOLJAPAN **C-free fork of the
[limbo](https://github.com/tursodatabase/limbo) 0.0.22 parser**, internal to the
OxiSQL workspace.

This crate is part of the C-free **oxisqlite** engine fork. The parser
(`src/parser/generated/parse.rs`) and keyword table
(`src/dialect/generated/keywords.rs`) are pre-generated and committed directly —
no C compiler, no `lemon.c` parser generator, no build script, no `cc` crate
dependency.

- **Approx LOC:** ~15,800 (tokei; up from ~14,800 — mostly this release's
  `splitrs` file-per-concern reorganization, see below).
- **Version:** 0.3.3 (2026-07-17).
- **Tests:** 221 passing (default features and `--all-features`), 0 failed
  (verified 2026-07-17).
- **Internal:** private member of the OxiSQL workspace; not published separately.

## SQLite3 Grammar Coverage

The lexer and parser cover the full SQLite3 grammar, including:

- DDL: `CREATE TABLE`, `CREATE INDEX`, `CREATE VIEW`, `CREATE TRIGGER`, `CREATE VIRTUAL TABLE`, `ALTER TABLE`, `DROP ...`
- DML: `SELECT`, `INSERT`, `UPDATE`, `DELETE`, `REPLACE`
- TCL: `BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, `RELEASE`
- Utility: `ATTACH`, `DETACH`, `PRAGMA`, `ANALYZE`, `VACUUM`, `REINDEX`, `EXPLAIN`

### Parser Features

- Tracks line and column positions.
- Streamable: stops at the end of each statement.
- Resumable: restarts after the end of a statement.
- Builds a typed AST.

## Extra Consistency Checks

See `checks.md` for the semantic checks applied on top of the grammar.

## Minimum Supported Rust Version

Latest stable Rust at time of release. Building requires no C toolchain.

## COOLJAPAN changes vs upstream limbo

- **Lifetime-free `Token`.** The lexer's token type was simplified from the
  lifetime-parameterized `Token<'i>(usize, &'i [u8], usize)` to
  `Token(usize, Cow<'static, str>, usize)` (`src/dialect/mod.rs`): a token
  whose source spelling matches its canonical form exactly (e.g. an
  uppercase `SELECT` keyword) borrows the associated `&'static str` from
  `TokenType::as_str` at zero cost, while every other token (identifiers,
  literals, differently-cased keywords) still allocates an owned `String`.
  **Breaking** for any direct consumer of the lexer's `Token` type.
- **Module split via `splitrs`.** `parser/ast/fmt.rs` and `parser/ast/mod.rs`
  were split into smaller per-concern files (`fmt/functions.rs`,
  `fmt/types.rs`, `ast/types.rs` plus `types_9.rs`/`types_10.rs`/`types_11.rs`,
  etc.) to stay under the workspace's 2000-line-per-file policy — purely
  internal reorganization, no functional or public-API change.

## Fork lineage & licensing

Full attribution, the upstream commit, and per-component licensing for this fork
are recorded in the repo-root [`/NOTICE`](../../NOTICE). Upstream limbo code
remains under MIT; COOLJAPAN code is licensed under Apache-2.0.

Part of the [OxiSQL](../../README.md) workspace.
