//! # `TableReferenceId` - Trait Implementations
//!
//! This module contains trait implementations for `TableReferenceId`.
//!
//! ## Implemented Traits
//!
//! - `Default`
//! - `From`
//! - `AddAssign`
//! - `Display`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::types_10::TableReferenceId;

impl Default for TableReferenceId {
    fn default() -> Self {
        Self(1)
    }
}
impl From<usize> for TableReferenceId {
    fn from(value: usize) -> Self {
        Self(value)
    }
}
impl std::ops::AddAssign<usize> for TableReferenceId {
    fn add_assign(&mut self, rhs: usize) {
        self.0 += rhs;
    }
}
impl std::fmt::Display for TableReferenceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "t{}", self.0)
    }
}
