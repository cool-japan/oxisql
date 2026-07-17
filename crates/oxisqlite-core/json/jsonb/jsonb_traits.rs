//! # `Jsonb` - Trait Implementations
//!
//! This module contains trait implementations for `Jsonb`.
//!
//! ## Implemented Traits
//!
//! - `FromStr`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::json::error::Error as PError;

use super::jsonb_type::Jsonb;

impl std::str::FromStr for Jsonb {
    type Err = PError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::from_str(s)
    }
}
