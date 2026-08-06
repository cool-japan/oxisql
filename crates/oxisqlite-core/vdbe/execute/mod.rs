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

/// Resolve the pager backing database registry index `db`.
///
/// `db == 0` is `main`, whose pager is the one the statement was stepped with;
/// everything else is looked up in the connection's auxiliary-database registry
/// (see [`crate::multidb`]).
pub(crate) fn pager_for_db(program: &Program, pager: &Rc<Pager>, db: usize) -> Result<Rc<Pager>> {
    if db == crate::multidb::DB_MAIN {
        Ok(pager.clone())
    } else {
        program.connection.pager_for_db(db)
    }
}

/// Open the read (and optionally write) transaction on a non-`main` database
/// before a cursor or B-tree operation touches it.
///
/// `main`'s transaction is opened by the prologue's `Transaction` opcode;
/// auxiliary databases are locked lazily here so a statement only ever locks the
/// databases it actually uses (`main`-only statements keep behaving exactly as
/// before). Returns `true` when the pager reported BUSY and the caller must
/// yield [`InsnFunctionStepResult::Busy`].
pub(crate) fn begin_db_txn(program: &Program, db: usize, write: bool) -> Result<bool> {
    if db == crate::multidb::DB_MAIN {
        return Ok(false);
    }
    program.connection.begin_aux_txn(db, write)
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
