//! # `UnaryOperator` - Trait Implementations
//!
//! This module contains trait implementations for `UnaryOperator`.
//!
//! ## Implemented Traits
//!
//! - `From`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::dialect::TokenType::*;
use crate::parser::parse::YYCODETYPE;

use super::types_10::UnaryOperator;

impl From<YYCODETYPE> for UnaryOperator {
    fn from(token_type: YYCODETYPE) -> Self {
        match token_type {
            x if x == TK_BITNOT as YYCODETYPE => Self::BitwiseNot,
            x if x == TK_MINUS as YYCODETYPE => Self::Negative,
            x if x == TK_NOT as YYCODETYPE => Self::Not,
            x if x == TK_PLUS as YYCODETYPE => Self::Positive,
            _ => unreachable!(),
        }
    }
}
