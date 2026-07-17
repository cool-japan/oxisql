//! # `ElementType` - Trait Implementations
//!
//! This module contains trait implementations for `ElementType`.
//!
//! ## Implemented Traits
//!
//! - `TryFrom`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::{bail_parse_error, LimboError};

use super::types::ElementType;

impl TryFrom<u8> for ElementType {
    type Error = LimboError;

    fn try_from(value: u8) -> std::result::Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::NULL),
            1 => Ok(Self::TRUE),
            2 => Ok(Self::FALSE),
            3 => Ok(Self::INT),
            4 => Ok(Self::INT5),
            5 => Ok(Self::FLOAT),
            6 => Ok(Self::FLOAT5),
            7 => Ok(Self::TEXT),
            8 => Ok(Self::TEXTJ),
            9 => Ok(Self::TEXT5),
            10 => Ok(Self::TEXTRAW),
            11 => Ok(Self::ARRAY),
            12 => Ok(Self::OBJECT),
            13 => Ok(Self::RESERVED1),
            14 => Ok(Self::RESERVED2),
            15 => Ok(Self::RESERVED3),
            _ => bail_parse_error!("Failed to recognize jsonvalue type"),
        }
    }
}
