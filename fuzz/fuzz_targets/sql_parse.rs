#![no_main]

//! Fuzz the SQL lexer/parser (`limbo_sqlite3_parser`, pre-generated from the
//! `lemon` grammar) with arbitrary bytes, independent of any on-disk file
//! format.
//!
//! `Parser::new` takes raw bytes directly (no UTF-8 requirement — SQL text
//! can be any byte sequence up to tokenization), matching the crate's own
//! `examples/sql_check.rs`. This target drives the parser to completion (or
//! its first typed error) on every input; the only failure mode being
//! searched for is a panic.

use fallible_iterator::FallibleIterator;
use libfuzzer_sys::fuzz_target;
use limbo_sqlite3_parser::lexer::sql::Parser;

fuzz_target!(|data: &[u8]| {
    let mut parser = Parser::new(data);
    loop {
        match parser.next() {
            // Successfully parsed one statement — keep going; a single
            // input may contain several `;`-separated statements.
            Ok(Some(_cmd)) => continue,
            // Clean end of input.
            Ok(None) => break,
            // Typed parse error — expected for most random byte strings.
            Err(_) => break,
        }
    }
});
