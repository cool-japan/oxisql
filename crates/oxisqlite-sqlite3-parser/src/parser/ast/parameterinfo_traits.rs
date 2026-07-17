//! # `ParameterInfo` - Trait Implementations
//!
//! This module contains trait implementations for `ParameterInfo`.
//!
//! ## Implemented Traits
//!
//! - `TokenStream`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::fmt::TokenStream;
use crate::dialect::TokenType::{self, *};
use std::num::ParseIntError;
use std::str::{self, FromStr};

use super::types_11::ParameterInfo;

// https://sqlite.org/lang_expr.html#parameters
impl TokenStream for ParameterInfo {
    type Error = ParseIntError;

    fn append(&mut self, ty: TokenType, value: Option<&str>) -> Result<(), Self::Error> {
        if ty == TK_VARIABLE {
            if let Some(variable) = value {
                if variable == "?" {
                    self.count = self.count.saturating_add(1);
                } else if variable.as_bytes()[0] == b'?' {
                    let n = u32::from_str(&variable[1..])?;
                    if n > self.count {
                        self.count = n;
                    }
                } else if self.names.insert(variable.to_owned()) {
                    self.count = self.count.saturating_add(1);
                }
            }
        }
        Ok(())
    }
}
