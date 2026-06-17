#![allow(unused_variables)]
//! VDBE instruction execution handlers (split into cohesive submodules).

use std::rc::Rc;

use crate::vdbe::insn::Insn;
use crate::{MvStore, Pager, Result};

use super::{Program, ProgramState};

macro_rules! return_if_io {
    ($expr:expr) => {
        match $expr? {
            CursorResult::Ok(v) => v,
            CursorResult::IO => return Ok(InsnFunctionStepResult::IO),
        }
    };
}

pub type InsnFunction = fn(
    &Program,
    &mut ProgramState,
    &Insn,
    &Rc<Pager>,
    Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult>;

pub enum InsnFunctionStepResult {
    Done,
    IO,
    Row,
    Interrupt,
    Busy,
    Step,
}

mod aggregate;
mod arith_logic;
mod cursor;
mod function;
mod mutate;
mod numeric;
mod txn_schema;
mod values;

pub use aggregate::*;
pub use arith_logic::*;
pub use cursor::*;
pub use function::*;
pub use mutate::*;
pub use txn_schema::*;

#[cfg(test)]
mod tests;
