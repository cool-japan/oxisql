//! Code generation for firing row triggers.
//!
//! # Strategy
//!
//! Upstream SQLite compiles each trigger into a `SubProgram` and invokes it with
//! `OP_Program`, which pushes a VDBE frame. This engine's VDBE has no frame
//! stack, so trigger bodies are instead **inlined** into the program that fires
//! them, using the same `incr_nesting()`/`decr_nesting()` mechanism the engine
//! already uses to splice a `SELECT` into a `CREATE TABLE AS` or an
//! `INSERT … SELECT`. Inlining is behaviourally equivalent to a frame for
//! everything except unbounded self-recursion — and self-recursion is exactly
//! what SQLite's default `recursive_triggers = off` forbids anyway (see
//! [`ProgramBuilder::is_trigger_active`]).
//!
//! `OLD.*` / `NEW.*` are resolved before translation by rewriting them into
//! [`limbo_sqlite3_parser::ast::Expr::Register`] reads — see
//! [`super::rewrite`]. That keeps the whole planner/optimizer path free of
//! trigger-specific plumbing.
//!
//! # Deviations from upstream SQLite, deliberately
//!
//! * `RAISE(ABORT)`, `RAISE(FAIL)` and `RAISE(ROLLBACK)` all abort the statement
//!   with `SQLITE_CONSTRAINT_TRIGGER` and drop the uncommitted page cache.
//!   Upstream distinguishes how much work each one undoes; this engine has no
//!   statement journal, so every constraint failure already behaves this way
//!   (see `op_halt`, which calls `Pager::clear_page_cache` for any non-zero
//!   error code). The error *is* raised in every case — the deviation is only in
//!   how much uncommitted work survives it.
//! * `INSTEAD OF` triggers are stored but never fire, because writing through a
//!   view is itself rejected upstream of here ("cannot modify X because it is a
//!   view"). No write silently misses its trigger: it fails.
//! * A `SELECT` inside a trigger body is supported in the `SELECT RAISE(…)`
//!   form. Any other `SELECT` body command is a typed error rather than a
//!   silently discarded query, because the engine has no result-discarding
//!   query destination and surfacing a trigger's rows to the caller would be
//!   worse than refusing.

use limbo_sqlite3_parser::ast::{self, TriggerTime};

use crate::schema::{BTreeTable, TriggerOp, TriggerRef};
use crate::translate::emitter::Resolver;
use crate::translate::expr::{translate_condition_expr, ConditionMetadata};
use crate::translate::plan::TableReferences;
use crate::translate::translate_inner;
use crate::vdbe::builder::{ProgramBuilder, ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::Insn;
use crate::vdbe::BranchOffset;
use crate::{LimboError, Result};

use super::raise::emit_raise_select;
use super::rewrite::{rewrite_when_clause, trigger_cmd_to_stmt, RowImage, TriggerRowBindings};

/// Hard cap on how deep trigger inlining may go.
///
/// With `recursive_triggers` off the chain is already finite (a trigger cannot
/// re-enter itself), so this only bounds pathological schemas where a long chain
/// of distinct triggers would generate an unreasonable amount of bytecode. It is
/// the analogue of upstream's `SQLITE_MAX_TRIGGER_DEPTH`.
pub const MAX_TRIGGER_DEPTH: usize = 32;

/// Which write is happening, for trigger matching.
#[derive(Clone, Debug)]
pub enum TriggerEventKind<'a> {
    Insert,
    Delete,
    /// The normalized names of the columns the `UPDATE` assigns, used to match
    /// `UPDATE OF (a, b)` triggers.
    Update(&'a [String]),
}

/// Everything the firing site has to tell the trigger code generator.
pub struct TriggerFireArgs<'a> {
    /// The table being written.
    pub table: &'a BTreeTable,
    /// `BEFORE` or `AFTER`.
    pub time: TriggerTime,
    /// The write being performed.
    pub event: TriggerEventKind<'a>,
    /// Where the pre-image lives; `None` for `INSERT`.
    pub old: Option<RowImage>,
    /// Where the post-image lives; `None` for `DELETE`.
    pub new: Option<RowImage>,
    /// Where `RAISE(IGNORE)` jumps — "skip this row, continue the statement".
    pub ignore_jump: BranchOffset,
}

/// Whether any trigger at all is attached to `table_name`.
///
/// Cheap pre-check so the write emitters can skip building row images when the
/// table has no triggers, which is the overwhelmingly common case.
pub fn table_has_triggers(resolver: &Resolver<'_>, table_name: &str) -> bool {
    resolver
        .schema
        .triggers
        .values()
        .any(|t| t.tbl_name == crate::util::normalize_ident(table_name))
}

/// Fire the `INSERT` triggers of `table` for the row image in `new_image`.
///
/// Convenience wrapper over [`emit_triggers`] for the one write path that has
/// no `OLD` image at all; keeps `translate::insert` free of the full
/// [`TriggerFireArgs`] construction at both of its call sites.
pub fn emit_insert_triggers(
    program: &mut ProgramBuilder,
    resolver: &Resolver<'_>,
    table: &BTreeTable,
    time: TriggerTime,
    new_image: RowImage,
    ignore_jump: BranchOffset,
) -> Result<()> {
    emit_triggers(
        program,
        resolver,
        &TriggerFireArgs {
            table,
            time,
            event: TriggerEventKind::Insert,
            old: None,
            new: Some(new_image),
            ignore_jump,
        },
    )
}

fn trigger_matches(trigger: &TriggerRef, args: &TriggerFireArgs<'_>) -> bool {
    if trigger.time != args.time {
        return false;
    }
    match (&trigger.op, &args.event) {
        (TriggerOp::Insert, TriggerEventKind::Insert) => true,
        (TriggerOp::Delete, TriggerEventKind::Delete) => true,
        (op @ TriggerOp::Update(_), TriggerEventKind::Update(changed)) => {
            op.matches_update_of(changed)
        }
        _ => false,
    }
}

/// Emit every trigger of `args.time`/`args.event` attached to `args.table`.
pub fn emit_triggers(
    program: &mut ProgramBuilder,
    resolver: &Resolver<'_>,
    args: &TriggerFireArgs<'_>,
) -> Result<()> {
    // `INSTEAD OF` never matches a table write; only BEFORE/AFTER do.
    if matches!(args.time, TriggerTime::InsteadOf) {
        return Ok(());
    }
    let triggers = resolver.schema.triggers_for_table(&args.table.name);
    for trigger in triggers {
        if !trigger_matches(&trigger, args) {
            continue;
        }
        // recursive_triggers = off: a trigger already being inlined further out
        // is skipped rather than re-entered.
        if program.is_trigger_active(&trigger.name) {
            continue;
        }
        if program.trigger_depth() >= MAX_TRIGGER_DEPTH {
            return Err(LimboError::ParseError(format!(
                "too many levels of trigger recursion (limit {MAX_TRIGGER_DEPTH}) at trigger {}",
                trigger.name
            )));
        }
        emit_one_trigger(program, resolver, args, &trigger)?;
    }
    Ok(())
}

fn emit_one_trigger(
    program: &mut ProgramBuilder,
    resolver: &Resolver<'_>,
    args: &TriggerFireArgs<'_>,
    trigger: &TriggerRef,
) -> Result<()> {
    let bindings = TriggerRowBindings {
        table: args.table,
        old: args.old,
        new: args.new,
    };

    // Jumped to when the WHEN guard is false: skip this trigger entirely.
    let skip_label = program.allocate_label();

    if let Some(when_clause) = trigger.when_clause.as_ref() {
        let when_clause = rewrite_when_clause(when_clause, &bindings)?;
        let continue_label = program.allocate_label();
        let meta = ConditionMetadata {
            jump_if_condition_is_true: false,
            jump_target_when_true: continue_label,
            jump_target_when_false: skip_label,
        };
        // The rewritten guard reads only registers and literals, so it needs no
        // table references in scope.
        let no_tables = TableReferences::new(vec![], vec![]);
        translate_condition_expr(program, &no_tables, &when_clause, meta, resolver)?;
        program.preassign_label_to_next_insn(continue_label);
    }

    // Rows written by the body must not leak into the outer statement's
    // `changes()`; upstream gets this from per-frame counters.
    let saved_changes_reg = program.alloc_register();
    program.emit_insn(Insn::ChangeCounterSnapshot {
        reg: saved_changes_reg,
        save: true,
    });

    program.push_trigger_frame(trigger.name.clone(), args.ignore_jump, saved_changes_reg);
    let body_result = emit_trigger_body(program, resolver, trigger, &bindings);
    program.pop_trigger_frame();
    body_result?;

    program.emit_insn(Insn::ChangeCounterSnapshot {
        reg: saved_changes_reg,
        save: false,
    });
    program.preassign_label_to_next_insn(skip_label);
    Ok(())
}

fn emit_trigger_body(
    program: &mut ProgramBuilder,
    resolver: &Resolver<'_>,
    trigger: &TriggerRef,
    bindings: &TriggerRowBindings<'_>,
) -> Result<()> {
    for cmd in trigger.commands.iter() {
        match cmd {
            // `SELECT RAISE(…)` is emitted directly: it needs no query plan,
            // and routing it through `translate_select` would emit a ResultRow
            // that surfaced the trigger's row to the caller.
            ast::TriggerCmd::Select(select) => {
                emit_raise_select(program, resolver, select, bindings, &trigger.name)?;
            }
            _ => {
                let stmt = trigger_cmd_to_stmt(cmd, bindings)?;
                translate_stmt_inline(program, resolver, stmt)?;
            }
        }
    }
    Ok(())
}

/// Splice a whole statement into the program currently being built.
///
/// `translate_inner` takes the builder by value and hands it back, while every
/// write-path emitter holds it behind `&mut`. Swapping a throwaway builder in
/// for the duration bridges the two without cloning the real one. On error the
/// enclosing translation is abandoned wholesale (every caller propagates with
/// `?` up to `translate()`), so the throwaway that is left behind is never
/// observed.
fn translate_stmt_inline(
    program: &mut ProgramBuilder,
    resolver: &Resolver<'_>,
    stmt: ast::Stmt,
) -> Result<()> {
    let query_mode = program.query_mode();
    // A nested statement's own emitter overwrites `result_columns` with its
    // (empty) plan output; preserve the outer statement's RETURNING columns.
    let saved_result_columns = std::mem::take(&mut program.result_columns);

    let placeholder = ProgramBuilder::new(ProgramBuilderOpts {
        query_mode: QueryMode::Normal,
        num_cursors: 0,
        approx_num_insns: 0,
        approx_num_labels: 0,
    });
    let taken = std::mem::replace(program, placeholder);
    // `nested_level > 0` makes `prologue()`/`epilogue()` no-ops, so the spliced
    // statement contributes only its body instructions.
    let mut taken = taken;
    taken.incr_nesting();
    let mut produced = translate_inner(
        resolver.schema,
        stmt,
        resolver.symbol_table,
        query_mode,
        taken,
    )?;
    produced.decr_nesting();
    produced.result_columns = saved_result_columns;
    *program = produced;
    Ok(())
}
