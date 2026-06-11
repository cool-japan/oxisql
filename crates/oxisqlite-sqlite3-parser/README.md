# oxisqlite-sqlite3-parser

Pure-Rust SQLite3 SQL parser and lexer — a COOLJAPAN **C-free fork of the
[limbo](https://github.com/tursodatabase/limbo) 0.0.22 parser**, internal to the
OxiSQL workspace.

This crate is part of the C-free **oxisqlite** engine fork. The parser
(`src/parser/generated/parse.rs`) and keyword table
(`src/dialect/generated/keywords.rs`) are pre-generated and committed directly —
no C compiler, no `lemon.c` parser generator, no build script, no `cc` crate
dependency.

- **Approx LOC:** ~14,800.
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

## Fork lineage & licensing

Full attribution, the upstream commit, and per-component licensing for this fork
are recorded in the repo-root [`/NOTICE`](../../NOTICE). Upstream limbo code
remains under MIT; COOLJAPAN code is licensed under Apache-2.0.

Part of the [OxiSQL](../../README.md) workspace.
