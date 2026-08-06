//! `RAISE(...)` code generation.
//!
//! `RAISE` is only legal inside a trigger body. All four resolutions are
//! implemented:
//!
//! | form               | generated code                                        |
//! |--------------------|-------------------------------------------------------|
//! | `RAISE(IGNORE)`    | `Goto` the firing site's "skip this row" label        |
//! | `RAISE(ABORT, m)`  | `Halt` with `SQLITE_CONSTRAINT_TRIGGER` and message m |
//! | `RAISE(FAIL, m)`   | same                                                  |
//! | `RAISE(ROLLBACK,m)`| same                                                  |
//!
//! Upstream differentiates how much work `ABORT` / `FAIL` / `ROLLBACK` undo.
//! This engine has no statement journal, and `op_halt` already discards the
//! whole uncommitted page cache for *any* non-zero error code (that is how
//! `NOT NULL` and `UNIQUE` failures behave here too), so the three collapse to
//! the same recovery behaviour. The raised error itself — code and message — is
//! faithful in every case. See the module docs of [`super::fire`].

use limbo_sqlite3_parser::ast::{self, ResolveType};

use crate::error::SQLITE_CONSTRAINT_TRIGGER;
use crate::translate::emitter::Resolver;
use crate::translate::expr::sanitize_string;
use crate::vdbe::builder::ProgramBuilder;
use crate::vdbe::insn::Insn;
use crate::{bail_parse_error, Result};

use super::rewrite::{rewrite_expr, TriggerRowBindings};

/// Extract the message of a `RAISE(<type>, <message>)`.
///
/// SQLite's grammar only admits a literal here, so anything that is not a
/// string literal (or a bare name, which SQLite also accepts and treats as the
/// literal text) is rejected rather than silently turned into an empty message.
fn raise_message(message: Option<&ast::Expr>) -> Result<String> {
    match message {
        None => Ok(String::new()),
        Some(ast::Expr::Literal(ast::Literal::String(s))) => Ok(sanitize_string(s)),
        Some(ast::Expr::Literal(ast::Literal::Numeric(n))) => Ok(n.clone()),
        Some(ast::Expr::Id(id)) => Ok(id.0.clone()),
        Some(ast::Expr::Name(name)) => Ok(name.0.clone()),
        Some(other) => {
            bail_parse_error!("RAISE() error message must be a literal, got: {:?}", other)
        }
    }
}

/// Emit a `RAISE(...)` expression.
///
/// `target_register` is written with NULL first so that the (unreachable) code
/// after the jump/halt still sees an initialised register — every `RAISE` form
/// leaves the current row's processing, so the value itself is never observed.
pub fn emit_raise(
    program: &mut ProgramBuilder,
    resolve_type: ResolveType,
    message: Option<&ast::Expr>,
    target_register: usize,
) -> Result<usize> {
    let Some((ignore_jump, changes_reg)) = program
        .current_trigger_frame()
        .map(|frame| (frame.ignore_jump, frame.changes_reg))
    else {
        bail_parse_error!("RAISE() may only be used within a trigger-program");
    };
    program.emit_null(target_register, None);
    match resolve_type {
        ResolveType::Ignore => {
            // The `Goto` below jumps clear over the change-counter restore that
            // `emit_one_trigger` emits at the end of this trigger's region, so
            // restore it here first: rows this body wrote before raising IGNORE
            // must not leak into the outer statement's `changes()`. Only the
            // innermost frame needs it — an enclosing trigger's `ignore_jump`
            // is inside *its* body, so its own restore still runs.
            program.emit_insn(Insn::ChangeCounterSnapshot {
                reg: changes_reg,
                save: false,
            });
            // Abandon the rest of this trigger and the row that fired it, then
            // carry on with the statement — upstream jumps to the calling
            // `OP_Program`'s P2 for exactly this.
            program.emit_insn(Insn::Goto {
                target_pc: ignore_jump,
            });
        }
        ResolveType::Abort | ResolveType::Fail | ResolveType::Rollback => {
            program.emit_insn(Insn::Halt {
                err_code: SQLITE_CONSTRAINT_TRIGGER,
                description: raise_message(message)?,
            });
        }
        ResolveType::Replace => {
            // `RAISE(REPLACE)` is not valid SQL: the grammar only admits
            // ROLLBACK/ABORT/FAIL/IGNORE after RAISE. Reject rather than
            // pretend.
            bail_parse_error!("RAISE(REPLACE) is not a valid conflict resolution for RAISE()");
        }
    }
    Ok(target_register)
}

/// Emit a trigger body command of the form `SELECT RAISE(...)`.
///
/// This is the canonical constraint-trigger idiom. It is emitted directly
/// instead of being routed through the SELECT planner because the engine has no
/// result-discarding query destination, and emitting a `ResultRow` here would
/// surface the trigger's internals to the caller of the firing statement.
pub fn emit_raise_select(
    program: &mut ProgramBuilder,
    _resolver: &Resolver<'_>,
    select: &ast::Select,
    bindings: &TriggerRowBindings<'_>,
    trigger_name: &str,
) -> Result<()> {
    let unsupported = |what: &str| -> crate::error::LimboError {
        crate::error::LimboError::ParseError(format!(
            "unsupported SELECT in body of trigger {trigger_name}: {what}. Only \
             `SELECT RAISE(...)` is supported; put row filters in the trigger's WHEN clause"
        ))
    };

    if select.with.is_some() {
        return Err(unsupported("WITH clause"));
    }
    if select.body.compounds.is_some() {
        return Err(unsupported("compound SELECT"));
    }
    if select.order_by.is_some() || select.limit.is_some() {
        return Err(unsupported("ORDER BY / LIMIT"));
    }
    let ast::OneSelect::Select(inner) = select.body.select.as_ref() else {
        return Err(unsupported("VALUES"));
    };
    if inner.from.is_some() {
        return Err(unsupported("FROM clause"));
    }
    if inner.where_clause.is_some() {
        return Err(unsupported("WHERE clause"));
    }
    if inner.group_by.is_some() || inner.window_clause.is_some() {
        return Err(unsupported("GROUP BY / WINDOW clause"));
    }
    let [ast::ResultColumn::Expr(expr, _)] = inner.columns.as_slice() else {
        return Err(unsupported("expected exactly one RAISE(...) result column"));
    };
    let ast::Expr::Raise(resolve_type, message) = expr else {
        return Err(unsupported("result column is not a RAISE(...) call"));
    };

    // The message may reference OLD/NEW only in the literal sense; rewrite it
    // anyway so a rejected non-literal reports the rewritten (resolved) form.
    let message = match message {
        Some(message) => {
            let mut message = message.as_ref().clone();
            rewrite_expr(&mut message, bindings)?;
            Some(message)
        }
        None => None,
    };
    let target_register = program.alloc_register();
    emit_raise(program, *resolve_type, message.as_ref(), target_register)?;
    Ok(())
}
