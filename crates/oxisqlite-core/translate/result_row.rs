use crate::{
    vdbe::{
        builder::ProgramBuilder,
        insn::{IdxInsertFlags, Insn},
        BranchOffset,
    },
    Result,
};

use super::{
    emitter::{LimitCtx, Resolver},
    expr::{translate_expr, translate_expr_no_constant_opt, NoConstantOptReason},
    order_by::sorter_insert,
    plan::{Distinctness, QueryDestination, SelectPlan},
};

/// Emits the bytecode for:
/// - all result columns
/// - result row (or if a subquery, yields to the parent query)
/// - limit
pub fn emit_select_result(
    program: &mut ProgramBuilder,
    resolver: &Resolver,
    plan: &SelectPlan,
    label_on_limit_reached: Option<BranchOffset>,
    offset_jump_to: Option<BranchOffset>,
    reg_nonagg_emit_once_flag: Option<usize>,
    reg_offset: Option<usize>,
    reg_result_cols_start: usize,
    limit_ctx: Option<LimitCtx>,
) -> Result<()> {
    if let (Some(jump_to), Some(_)) = (offset_jump_to, label_on_limit_reached) {
        emit_offset(program, plan, jump_to, reg_offset)?;
    }

    let start_reg = reg_result_cols_start;
    // Destinations that write successive rows into the SAME shared result-column
    // registers (a coroutine yield block, a compound-SELECT dedup index, or the
    // ORDER BY materialization sorter) must NOT hoist constant result-column
    // expressions into the run-once prologue: doing so would let a later compound
    // arm's constant clobber an earlier arm's, since they share the register block
    // (e.g. `SELECT 1,'a' UNION ALL SELECT 2,'b'` driven through a coroutine).
    let reuses_result_registers = matches!(
        plan.query_destination,
        QueryDestination::CoroutineYield { .. }
            | QueryDestination::EphemeralIndex { .. }
            | QueryDestination::Sorter { .. }
    );
    for (i, rc) in plan.result_columns.iter().enumerate().filter(|(_, rc)| {
        // For aggregate queries, we handle columns differently; example: select id, first_name, sum(age) from users limit 1;
        // 1. Columns with aggregates (e.g., sum(age)) are computed in each iteration of aggregation
        // 2. Non-aggregate columns (e.g., id, first_name) are only computed once in the first iteration
        // This filter ensures we only emit expressions for non aggregate columns once,
        // preserving previously calculated values while updating aggregate results
        // For all other queries where reg_nonagg_emit_once_flag is none we do nothing.
        reg_nonagg_emit_once_flag.is_some() && !rc.contains_aggregates.is_empty()
            || reg_nonagg_emit_once_flag.is_none()
    }) {
        let reg = start_reg + i;
        if reuses_result_registers {
            translate_expr_no_constant_opt(
                program,
                Some(&plan.table_references),
                &rc.expr,
                reg,
                resolver,
                NoConstantOptReason::RegisterReuse,
            )?;
        } else {
            translate_expr(
                program,
                Some(&plan.table_references),
                &rc.expr,
                reg,
                resolver,
            )?;
        }
    }

    // Handle SELECT DISTINCT deduplication
    if let Distinctness::Distinct { ctx } = &plan.distinctness {
        let distinct_ctx = ctx.as_ref().expect("distinct context must exist");
        let num_regs = plan.result_columns.len();
        distinct_ctx.emit_deduplication_insns(program, num_regs, start_reg);
    }

    emit_result_row_and_limit(program, plan, start_reg, limit_ctx, label_on_limit_reached)?;
    Ok(())
}

/// Emits the bytecode for:
/// - result row (or if a subquery, yields to the parent query)
/// - limit
pub fn emit_result_row_and_limit(
    program: &mut ProgramBuilder,
    plan: &SelectPlan,
    result_columns_start_reg: usize,
    limit_ctx: Option<LimitCtx>,
    label_on_limit_reached: Option<BranchOffset>,
) -> Result<()> {
    match &plan.query_destination {
        QueryDestination::ResultRows => {
            program.emit_insn(Insn::ResultRow {
                start_reg: result_columns_start_reg,
                count: plan.result_columns.len(),
            });
        }
        QueryDestination::EphemeralIndex {
            cursor_id: index_cursor_id,
            index: dedupe_index,
        } => {
            let record_reg = program.alloc_register();
            program.emit_insn(Insn::MakeRecord {
                start_reg: result_columns_start_reg,
                count: plan.result_columns.len(),
                dest_reg: record_reg,
                index_name: Some(dedupe_index.name.clone()),
            });
            program.emit_insn(Insn::IdxInsert {
                cursor_id: *index_cursor_id,
                record_reg,
                unpacked_start: None,
                unpacked_count: None,
                flags: IdxInsertFlags::new(),
            });
        }
        QueryDestination::CoroutineYield { yield_reg, .. } => {
            program.emit_insn(Insn::Yield {
                yield_reg: *yield_reg,
                end_offset: BranchOffset::Offset(0),
            });
        }
        QueryDestination::Sorter {
            cursor_id,
            reg_sorter_data,
            sort_key_ordinals,
        } => {
            // Materializing for a later, single sort pass over the *combined* compound-SELECT
            // result (see `translate::compound_select`) -- LIMIT/OFFSET must not apply here (which
            // rows end up in the final top-N isn't known until every arm has been sorted), so
            // return before the LIMIT handling below runs. This mirrors how the `EphemeralIndex`
            // destination above is likewise never called with `plan.limit`/`limit_ctx` set for its
            // own (dedup-accumulator) intermediate inserts -- see `compound_select.rs`.
            //
            // `compound_select::emit_compound_select_order_by` reserves `sort_key_ordinals.len()`
            // registers immediately *before* wherever this arm's own output columns end up
            // (`result_columns_start_reg`), exactly like the `yield_reg + 1` convention
            // `QueryDestination::CoroutineYield` relies on -- so filling those reserved registers
            // with copies of this arm's own values at the sort-key ordinal positions makes
            // `[keys][result_columns_start_reg..]` one contiguous, `sorter_insert`-ready range,
            // with no need to re-copy the (already-computed) output columns themselves.
            let num_keys = sort_key_ordinals.len();
            let key_block_start = result_columns_start_reg - num_keys;
            for (i, &ordinal) in sort_key_ordinals.iter().enumerate() {
                program.emit_insn(Insn::Copy {
                    src_reg: result_columns_start_reg + ordinal,
                    dst_reg: key_block_start + i,
                    amount: 0,
                });
            }
            sorter_insert(
                program,
                key_block_start,
                num_keys + plan.result_columns.len(),
                *cursor_id,
                *reg_sorter_data,
            );
            return Ok(());
        }
    }

    if plan.limit.is_some() || limit_ctx.is_some() {
        if label_on_limit_reached.is_none() {
            // There are cases where LIMIT is ignored, e.g. aggregation without a GROUP BY clause.
            // We already early return on LIMIT 0, so we can just return here since the n of rows
            // is always 1 here.
            return Ok(());
        }
        let limit_ctx = limit_ctx.expect("limit_ctx must be Some when any limit is active");

        program.emit_insn(Insn::DecrJumpZero {
            reg: limit_ctx.reg_limit,
            target_pc: label_on_limit_reached.expect("label_on_limit_reached checked above"),
        });
    }
    Ok(())
}

pub fn emit_offset(
    program: &mut ProgramBuilder,
    plan: &SelectPlan,
    jump_to: BranchOffset,
    reg_offset: Option<usize>,
) -> Result<()> {
    let needs_offset_skip =
        plan.offset.is_some_and(|o| o > 0) || (plan.offset_expr.is_some() && reg_offset.is_some());
    if needs_offset_skip {
        program.add_comment(program.offset(), "OFFSET");
        program.emit_insn(Insn::IfPos {
            reg: reg_offset.expect("reg_offset must be Some when offset skipping is active"),
            target_pc: jump_to,
            decrement_by: 1,
        });
    }
    Ok(())
}
