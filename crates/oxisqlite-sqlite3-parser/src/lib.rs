//! SQLite3 syntax lexer and parser
// UPSTREAM: vendored Limbo fork — allow upstream style
#![allow(
    clippy::borrow_deref_ref,
    clippy::doc_lazy_continuation,
    clippy::empty_line_after_outer_attr,
    clippy::needless_borrow,
    clippy::obfuscated_if_else,
    clippy::redundant_pattern_matching,
    clippy::single_match,
    clippy::unnecessary_map_or,
    clippy::useless_format
)]
#![warn(missing_docs)]

pub mod dialect;
// In Lemon, the tokenizer calls the parser.
pub mod lexer;
mod parser;
pub use parser::ast;
pub mod to_sql_string;
