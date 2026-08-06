use crate::schema::Schema;
use crate::translate::emitter::Resolver;
use crate::translate::expr::translate_expr;
use crate::translate::{ProgramBuilder, ProgramBuilderOpts};
use crate::vdbe::insn::Insn;
use crate::{QueryMode, Result, SymbolTable};
use limbo_sqlite3_parser::ast::{Expr, Name, TransactionType};

pub fn translate_tx_begin(
    tx_type: Option<TransactionType>,
    _tx_name: Option<Name>,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    let tx_type = tx_type.unwrap_or(TransactionType::Deferred);
    match tx_type {
        TransactionType::Deferred => {
            program.emit_insn(Insn::AutoCommit {
                auto_commit: false,
                rollback: false,
            });
        }
        TransactionType::Immediate | TransactionType::Exclusive => {
            program.emit_insn(Insn::Transaction {
                write: true,
                schema_cookie: program.schema_cookie,
            });
            // TODO: Emit transaction instruction on temporary tables when we support them.
            program.emit_insn(Insn::AutoCommit {
                auto_commit: false,
                rollback: false,
            });
        }
    }
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

pub fn translate_tx_commit(
    _tx_name: Option<Name>,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    program.emit_insn(Insn::AutoCommit {
        auto_commit: true,
        rollback: false,
    });
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

/// Translate `ATTACH [DATABASE] <filename> AS <schema-name> [KEY <key>]`.
///
/// Both operands are ordinary expressions in SQLite's grammar (a literal, a
/// bind parameter, or anything constant-foldable), so they are compiled into
/// registers and read at run time by [`Insn::Attach`].
///
/// # Errors
///
/// [`crate::LimboError::ParseError`] for the `KEY` clause, which requires the
/// SQLite Encryption Extension this engine does not implement -- rejecting is
/// the only honest answer, since silently ignoring a key would attach an
/// unencrypted database under a name the caller believes is encrypted.
pub fn translate_attach(
    schema: &Schema,
    syms: &SymbolTable,
    path: Expr,
    alias: Expr,
    key: Option<Box<Expr>>,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    if key.is_some() {
        crate::bail_parse_error!("ATTACH ... KEY is not supported (no encryption extension)");
    }
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 8,
        approx_num_labels: 0,
    });
    let resolver = Resolver::new(schema, syms);
    let path_reg = emit_db_name_operand(&mut program, &path, &resolver)?;
    let alias_reg = emit_db_name_operand(&mut program, &alias, &resolver)?;
    program.emit_insn(Insn::Attach {
        path_reg,
        alias_reg,
    });
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

/// Translate `DETACH [DATABASE] <schema-name>`.
pub fn translate_detach(
    schema: &Schema,
    syms: &SymbolTable,
    alias: Expr,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 4,
        approx_num_labels: 0,
    });
    let resolver = Resolver::new(schema, syms);
    let alias_reg = emit_db_name_operand(&mut program, &alias, &resolver)?;
    program.emit_insn(Insn::Detach { alias_reg });
    program.epilogue(super::emitter::TransactionMode::None);
    Ok(program)
}

/// Evaluate an `ATTACH`/`DETACH` operand into a fresh register.
///
/// The grammar calls these operands expressions, and they may indeed be string
/// literals or bind parameters. A *bare identifier* however is a name, not a
/// column reference -- `ATTACH \'file.db\' AS aux` has no table in scope for
/// `aux` to be a column of -- so it is emitted as its own text, matching what
/// SQLite accepts. Everything else goes through the ordinary expression
/// translator.
fn emit_db_name_operand(
    program: &mut ProgramBuilder,
    expr: &Expr,
    resolver: &Resolver,
) -> Result<usize> {
    if let Expr::Id(name) = expr {
        return Ok(program.emit_string8_new_reg(crate::util::normalize_ident(name.0.as_str())));
    }
    let reg = program.alloc_register();
    translate_expr(program, None, expr, reg, resolver)?;
    Ok(reg)
}
