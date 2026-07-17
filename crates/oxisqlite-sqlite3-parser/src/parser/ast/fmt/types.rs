//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use std::fmt::{self, Formatter};

pub(super) struct WriteTokenStream<'a, T: fmt::Write> {
    pub(super) write: &'a mut T,
    pub(super) spaced: bool,
}
pub(super) struct FmtTokenStream<'a, 'b> {
    pub(super) f: &'a mut Formatter<'b>,
    pub(super) spaced: bool,
}
