//! # `Name` - Trait Implementations
//!
//! This module contains trait implementations for `Name`.
//!
//! ## Implemented Traits
//!
//! - `Hash`
//! - `PartialEq`
//! - `PartialEq`
//! - `PartialEq`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use std::str::{self};

use super::functions::eq_ignore_case_and_quote;
use super::types::Name;
use super::types_10::QuotedIterator;

/// Ignore case and quote
impl std::hash::Hash for Name {
    fn hash<H: std::hash::Hasher>(&self, hasher: &mut H) {
        self.as_bytes()
            .for_each(|b| hasher.write_u8(b.to_ascii_lowercase()));
    }
}
/// Ignore case and quote
impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        eq_ignore_case_and_quote(self.as_bytes(), other.as_bytes())
    }
}
/// Ignore case and quote
impl PartialEq<str> for Name {
    fn eq(&self, other: &str) -> bool {
        eq_ignore_case_and_quote(self.as_bytes(), QuotedIterator(other.bytes(), 0u8))
    }
}
/// Ignore case and quote
impl PartialEq<&str> for Name {
    fn eq(&self, other: &&str) -> bool {
        eq_ignore_case_and_quote(self.as_bytes(), QuotedIterator(other.bytes(), 0u8))
    }
}
