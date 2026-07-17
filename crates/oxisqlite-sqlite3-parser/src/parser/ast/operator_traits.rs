//! # `Operator` - Trait Implementations
//!
//! This module contains trait implementations for `Operator`.
//!
//! ## Implemented Traits
//!
//! - `From`
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::dialect::TokenType::*;
use crate::parser::parse::YYCODETYPE;

use super::types::Operator;

impl From<YYCODETYPE> for Operator {
    fn from(token_type: YYCODETYPE) -> Self {
        match token_type {
            x if x == TK_AND as YYCODETYPE => Self::And,
            x if x == TK_OR as YYCODETYPE => Self::Or,
            x if x == TK_LT as YYCODETYPE => Self::Less,
            x if x == TK_GT as YYCODETYPE => Self::Greater,
            x if x == TK_GE as YYCODETYPE => Self::GreaterEquals,
            x if x == TK_LE as YYCODETYPE => Self::LessEquals,
            x if x == TK_EQ as YYCODETYPE => Self::Equals,
            x if x == TK_NE as YYCODETYPE => Self::NotEquals,
            x if x == TK_BITAND as YYCODETYPE => Self::BitwiseAnd,
            x if x == TK_BITOR as YYCODETYPE => Self::BitwiseOr,
            x if x == TK_BITNOT as YYCODETYPE => Self::BitwiseNot,
            x if x == TK_LSHIFT as YYCODETYPE => Self::LeftShift,
            x if x == TK_RSHIFT as YYCODETYPE => Self::RightShift,
            x if x == TK_PLUS as YYCODETYPE => Self::Add,
            x if x == TK_MINUS as YYCODETYPE => Self::Subtract,
            x if x == TK_STAR as YYCODETYPE => Self::Multiply,
            x if x == TK_SLASH as YYCODETYPE => Self::Divide,
            x if x == TK_REM as YYCODETYPE => Self::Modulus,
            x if x == TK_CONCAT as YYCODETYPE => Self::Concat,
            x if x == TK_IS as YYCODETYPE => Self::Is,
            x if x == TK_NOT as YYCODETYPE => Self::IsNot,
            _ => unreachable!(),
        }
    }
}
