use crate::translate::{ProgramBuilder, ProgramBuilderOpts};
use crate::vdbe::insn::{Insn, SavepointOp};
use crate::{QueryMode, Result};
use limbo_sqlite3_parser::ast::Name;

/// Translates a `ROLLBACK [TRANSACTION]` statement into VDBE bytecode.
///
/// For a full transaction rollback, emits `AutoCommit { auto_commit: true, rollback: true }`.
/// For `ROLLBACK TO [SAVEPOINT] name`, emits a `Savepoint { op: RollbackTo, name }` instruction.
pub fn translate_rollback(
    tx_name: Option<Name>,
    savepoint_name: Option<Name>,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let _ = tx_name; // transaction name is informational only in SQLite
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    if let Some(sp_name) = savepoint_name {
        program.emit_insn(Insn::Savepoint {
            op: SavepointOp::RollbackTo,
            name: sp_name.0.to_string(),
        });
    } else {
        program.emit_insn(Insn::AutoCommit {
            auto_commit: true,
            rollback: true,
        });
    }
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

/// Translates a `SAVEPOINT name` statement into VDBE bytecode.
pub fn translate_savepoint(name: Name, mut program: ProgramBuilder) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    program.emit_insn(Insn::Savepoint {
        op: SavepointOp::Begin,
        name: name.0.to_string(),
    });
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

/// Translates a `RELEASE [SAVEPOINT] name` statement into VDBE bytecode.
pub fn translate_release(name: Name, mut program: ProgramBuilder) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    program.emit_insn(Insn::Savepoint {
        op: SavepointOp::Release,
        name: name.0.to_string(),
    });
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}
