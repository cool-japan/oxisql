//! # `DistinctNames` - Trait Implementations
//!
//! This module contains trait implementations for `DistinctNames`.
//!
//! ## Implemented Traits
//!
//! - `Deref`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use indexmap::IndexSet;
use std::ops::Deref;

use super::types::{DistinctNames, Name};

impl Deref for DistinctNames {
    type Target = IndexSet<Name>;

    fn deref(&self) -> &IndexSet<Name> {
        &self.0
    }
}
