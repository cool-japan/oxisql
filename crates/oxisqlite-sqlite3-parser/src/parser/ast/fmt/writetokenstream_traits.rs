//! # `WriteTokenStream` - Trait Implementations
//!
//! This module contains trait implementations for `WriteTokenStream`.
//!
//! ## Implemented Traits
//!
//! - `TokenStream`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::dialect::TokenType;
use crate::dialect::TokenType::*;
use std::fmt::{self};

use super::functions::TokenStream;
use super::types::WriteTokenStream;

impl<T: fmt::Write> TokenStream for WriteTokenStream<'_, T> {
    type Error = fmt::Error;

    fn append(&mut self, ty: TokenType, value: Option<&str>) -> fmt::Result {
        if !self.spaced {
            match ty {
                TK_COMMA | TK_SEMI | TK_RP | TK_DOT => {}
                _ => {
                    self.write.write_char(' ')?;
                    self.spaced = true;
                }
            };
        }
        if ty == TK_BLOB {
            self.write.write_char('X')?;
            self.write.write_char('\'')?;
            if let Some(str) = value {
                self.write.write_str(str)?;
            }
            return self.write.write_char('\'');
        } else if let Some(str) = ty.as_str() {
            self.write.write_str(str)?;
            self.spaced = ty == TK_LP || ty == TK_DOT; // str should not be whitespace
        }
        if let Some(str) = value {
            // trick for pretty-print
            self.spaced = str.bytes().all(|b| b.is_ascii_whitespace());
            self.write.write_str(str)
        } else {
            Ok(())
        }
    }
}
