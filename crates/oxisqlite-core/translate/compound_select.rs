use crate::schema::{Column, Index, IndexColumn, PseudoTable, Schema, Type};
use crate::translate::collate::CollationSeq;
use crate::translate::emitter::{emit_query, LimitCtx, TransactionMode, TranslateCtx};
use crate::translate::order_by::sorter_insert;
use crate::translate::plan::{Plan, QueryDestination, SelectPlan};
use crate::vdbe::builder::{CursorType, ProgramBuilder};
use crate::vdbe::insn::Insn;
use crate::vdbe::BranchOffset;
use crate::{LimboError, Result, SymbolTable};
use limbo_sqlite3_parser::ast::{self, CompoundOperator, ResolveType, SortOrder};
use std::rc::Rc;
use std::sync::Arc;
use tracing::instrument;

use tracing::Level;

/// Emits a whole compound SELECT (`UNION` / `UNION ALL` / `INTERSECT` / `EXCEPT` chain).
///
/// SQLite evaluates a chain of compound operators strictly left-to-right (NOT by any kind of
/// operator precedence): `A UNION B EXCEPT C INTERSECT D` groups as `((A UNION B) EXCEPT C)
/// INTERSECT D`. `Plan::CompoundSelect::left` already encodes exactly this grouping (see
/// `select::prepare_select_plan`): `left[i]` pairs a `SelectPlan` with the operator that combines
/// it with *the next* thing in the chain (either `left[i+1]` or, for the last entry, `right_most`).
/// `emit_compound_select` walks this right-to-left via `left.pop()`, which is what naturally
/// reproduces the same left-to-right grouping during code generation (see its doc comment below).
#[instrument(skip_all, level = Level::TRACE)]
pub fn emit_program_for_compound_select(
    program: &mut ProgramBuilder,
    plan: Plan,
    schema: &Schema,
    syms: &SymbolTable,
) -> Result<()> {
    let Plan::CompoundSelect {
        right_most,
        limit,
        offset,
        order_by,
        limit_expr,
        offset_expr,
        ..
    } = &plan
    else {
        crate::bail_parse_error!("expected compound select plan");
    };

    // Parameterized LIMIT/OFFSET is not yet supported for compound SELECTs
    if limit_expr.is_some() || offset_expr.is_some() {
        crate::bail_parse_error!(
            "parameterized LIMIT/OFFSET is not supported for compound SELECTs yet"
        );
    }

    let right_plan = right_most.clone();
    let limit = *limit;
    let offset = *offset;
    let order_by = order_by.clone();

    // Trivial exit on LIMIT 0
    if let Some(0) = limit {
        program.epilogue(TransactionMode::Read);
        program.result_columns = right_plan.result_columns;
        program.table_references.extend(right_plan.table_references);
        return Ok(());
    }

    // Each subselect shares the same limit_ctx, because the LIMIT applies to the entire compound select,
    // not just a single subselect.
    let limit_ctx = limit.map(|limit| {
        let reg = program.alloc_register();
        program.emit_insn(Insn::Integer {
            value: limit as i64,
            dest: reg,
        });
        LimitCtx::new_shared(reg)
    });

    // A shared OFFSET countdown register, exactly analogous to `limit_ctx` above: allocated and
    // initialized exactly once here, then threaded through unchanged so that every genuine
    // combined-output row -- regardless of which arm or which operator produced it -- is counted
    // against the very same countdown (see `result_row::emit_offset`, which this reuses at every
    // "real final output" point below).
    let offset_ctx = offset.filter(|o| *o > 0).map(|offset| {
        let reg = program.alloc_register();
        program.emit_insn(Insn::Integer {
            value: offset as i64,
            dest: reg,
        });
        reg
    });

    match order_by {
        None => {
            // When a compound SELECT is part of a query that yields results to a coroutine (e.g.
            // within an INSERT clause), we must allocate registers for the result columns to be
            // yielded -- specifically the registers immediately following `yield_reg`, since every
            // `Insn::Yield`-driving caller reads a yielded row's columns from the fixed `yield_reg
            // + 1` location by convention (e.g. `insert`'s post-`Yield` `MakeRecord { start_reg:
            // yield_reg + 1, .. }`; see also `emit_forward_row`'s doc comment, which relies on the
            // exact same convention for UNION/INTERSECT/EXCEPT). Each subselect will then yield to
            // the coroutine using this same set of registers.
            let reg_result_cols_start = match right_plan.query_destination {
                QueryDestination::CoroutineYield { .. } => {
                    Some(program.alloc_registers(right_plan.result_columns.len()))
                }
                _ => None,
            };

            emit_compound_select(
                program,
                plan,
                schema,
                syms,
                limit_ctx,
                offset_ctx,
                reg_result_cols_start,
            )?;
        }
        Some(order_by) => {
            emit_compound_select_order_by(
                program,
                plan,
                schema,
                syms,
                limit_ctx,
                offset_ctx,
                order_by,
                &right_plan,
            )?;
        }
    }

    program.epilogue(TransactionMode::Read);
    program.result_columns = right_plan.result_columns;
    program.table_references.extend(right_plan.table_references);

    Ok(())
}

/// Emits a compound SELECT (`UNION [ALL]`/`INTERSECT`/`EXCEPT`) as a FROM-clause
/// subquery coroutine, mirroring [`super::subquery::emit_subquery`] for the
/// single-SELECT case. Returns the start register of the yielded result columns.
///
/// Unlike [`emit_program_for_compound_select`] (a top-level entry point that ends
/// with `program.epilogue(...)` and rewrites `program.result_columns`/
/// `table_references`), this wraps the row-emission core in
/// `InitCoroutine`/`EndCoroutine` and emits no epilogue, so it can run nested
/// inside an enclosing query's program without corrupting it.
///
/// `plan` is mutated in place so its arms carry the coroutine destination (the
/// main loop reads `right_most.query_destination` back to re-init the coroutine
/// per outer row); a clone is what actually gets consumed by emission.
pub(crate) fn emit_compound_subquery(
    program: &mut ProgramBuilder,
    plan: &mut Plan,
    schema: &Schema,
    syms: &SymbolTable,
) -> Result<usize> {
    let (num_result_cols, limit, offset, order_by, right_plan) = match &*plan {
        Plan::CompoundSelect {
            right_most,
            limit,
            offset,
            order_by,
            limit_expr,
            offset_expr,
            ..
        } => {
            if limit_expr.is_some() || offset_expr.is_some() {
                crate::bail_parse_error!(
                    "parameterized LIMIT/OFFSET is not supported for compound SELECTs yet"
                );
            }
            (
                right_most.result_columns.len(),
                *limit,
                *offset,
                order_by.clone(),
                right_most.clone(),
            )
        }
        _ => crate::bail_parse_error!("expected compound select plan"),
    };

    let yield_reg = program.alloc_register();
    // `reg_result_cols_start` is allocated immediately after `yield_reg` so it is
    // exactly `yield_reg + 1`: this keeps the two ways a combined-output row can
    // reach the coroutine in agreement -- a streaming arm (which writes columns to
    // `reg_result_cols_start`) and the dedup/materialization read-back path (which
    // uses the `yield_reg + 1` convention in `emit_forward_row`).
    let reg_result_cols_start = program.alloc_registers(num_result_cols);
    let coroutine_start = program.allocate_label();
    let body_end = program.allocate_label();

    // Point every arm (each with its own independent `query_destination` clone)
    // at this coroutine.
    if let Plan::CompoundSelect {
        left, right_most, ..
    } = plan
    {
        let dest = QueryDestination::CoroutineYield {
            yield_reg,
            coroutine_implementation_start: coroutine_start,
        };
        right_most.query_destination = dest.clone();
        for (arm, _) in left.iter_mut() {
            arm.query_destination = dest.clone();
        }
    }
    let plan_for_emit = plan.clone();

    program.emit_insn(Insn::InitCoroutine {
        yield_reg,
        jump_on_definition: body_end,
        start_offset: coroutine_start,
    });
    program.preassign_label_to_next_insn(coroutine_start);

    // A `LIMIT 0` body yields nothing: skip straight to the end of the coroutine.
    if let Some(0) = limit {
        program.emit_insn(Insn::EndCoroutine { yield_reg });
        program.preassign_label_to_next_insn(body_end);
        return Ok(reg_result_cols_start);
    }

    // Shared LIMIT/OFFSET countdown registers (see `emit_program_for_compound_select`).
    let limit_ctx = limit.map(|limit| {
        let reg = program.alloc_register();
        program.emit_insn(Insn::Integer {
            value: limit as i64,
            dest: reg,
        });
        LimitCtx::new_shared(reg)
    });
    let offset_ctx = offset.filter(|o| *o > 0).map(|offset| {
        let reg = program.alloc_register();
        program.emit_insn(Insn::Integer {
            value: offset as i64,
            dest: reg,
        });
        reg
    });

    match order_by {
        None => emit_compound_select(
            program,
            plan_for_emit,
            schema,
            syms,
            limit_ctx,
            offset_ctx,
            Some(reg_result_cols_start),
        )?,
        Some(order_by) => emit_compound_select_order_by(
            program,
            plan_for_emit,
            schema,
            syms,
            limit_ctx,
            offset_ctx,
            order_by,
            &right_plan,
        )?,
    }

    program.emit_insn(Insn::EndCoroutine { yield_reg });
    program.preassign_label_to_next_insn(body_end);
    Ok(reg_result_cols_start)
}

// Emits bytecode for the rightmost part of the compound SELECT and handles the left parts
// recursively based on the compound operator type.
//
// `limit_ctx`/`offset_ctx` are `None` whenever this call's rows are *not* genuine final output --
// i.e. while materializing the left-hand operand of a UNION/INTERSECT/EXCEPT into that operator's
// own accumulator -- and `Some` (the shared, already-initialized counters from
// `emit_program_for_compound_select`) at every point where rows are genuinely final (which may be
// deep inside the recursion, e.g. a `UNION ALL` chain that streams straight through, or a
// `UNION`/`INTERSECT`/`EXCEPT` operator whose right-hand operand's original destination -- see
// `right_most.query_destination`, captured before it gets temporarily overwritten -- turns out to
// already be real output rather than an *inherited* accumulator from some enclosing operator).
fn emit_compound_select(
    program: &mut ProgramBuilder,
    plan: Plan,
    schema: &Schema,
    syms: &SymbolTable,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
    reg_result_cols_start: Option<usize>,
) -> Result<()> {
    let Plan::CompoundSelect {
        mut left,
        mut right_most,
        limit,
        offset,
        order_by,
        ..
    } = plan
    else {
        unreachable!()
    };

    let mut right_most_ctx = TranslateCtx::new(
        program,
        schema,
        syms,
        right_most.table_references.joined_tables().len(),
        right_most.result_columns.len(),
    );
    right_most_ctx.reg_result_cols_start = reg_result_cols_start;
    match left.pop() {
        Some((mut plan, operator)) => match operator {
            CompoundOperator::UnionAll => {
                // Transparent pass-through: UNION ALL performs no work of its own (no
                // deduplication, no membership test), so if this operator's right-hand operand is
                // itself being routed somewhere other than genuine final output -- an enclosing
                // UNION/INTERSECT/EXCEPT's own accumulator, or (for a compound SELECT with an
                // ORDER BY) the shared materialization `Sorter` -- the left-hand operand must be
                // routed to that exact same place too.
                if !matches!(
                    right_most.query_destination,
                    QueryDestination::ResultRows | QueryDestination::CoroutineYield { .. }
                ) {
                    plan.query_destination = right_most.query_destination.clone();
                }
                let compound_select = Plan::CompoundSelect {
                    left,
                    right_most: plan,
                    limit,
                    offset,
                    order_by,
                    limit_expr: None,
                    offset_expr: None,
                };
                emit_compound_select(
                    program,
                    compound_select,
                    schema,
                    syms,
                    limit_ctx,
                    offset_ctx,
                    reg_result_cols_start,
                )?;

                let label_next_select = program.allocate_label();
                if let Some(limit_ctx) = limit_ctx {
                    program.emit_insn(Insn::IfNot {
                        reg: limit_ctx.reg_limit,
                        target_pc: label_next_select,
                        jump_if_null: true,
                    });
                }
                if limit_ctx.is_some() || offset_ctx.is_some() {
                    right_most.limit = limit;
                    right_most.offset = offset;
                    right_most_ctx.limit_ctx = limit_ctx;
                    right_most_ctx.reg_offset = offset_ctx;
                }
                emit_query(program, &mut right_most, &mut right_most_ctx)?;
                program.preassign_label_to_next_insn(label_next_select);
            }
            CompoundOperator::Union => {
                // Captured *before* `right_most`'s destination is inspected/overwritten below:
                // this is what `emit_forward_all_rows` uses, once every arm has fed
                // `dedupe_index`, to decide where the deduplicated rows actually belong (see its
                // doc comment).
                let original_destination = right_most.query_destination.clone();
                let mut new_dedupe_index = false;
                let dedupe_index = match right_most.query_destination {
                    QueryDestination::EphemeralIndex { cursor_id, index } => {
                        (cursor_id, index.clone())
                    }
                    _ => {
                        if cfg!(not(feature = "index_experimental")) {
                            crate::bail_parse_error!("UNION not supported without indexes");
                        } else {
                            new_dedupe_index = true;
                            create_compound_dedupe_index(program, &right_most, "union")
                        }
                    }
                };
                plan.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: dedupe_index.0,
                    index: dedupe_index.1.clone(),
                };
                let compound_select = Plan::CompoundSelect {
                    left,
                    right_most: plan,
                    limit,
                    offset,
                    order_by,
                    limit_expr: None,
                    offset_expr: None,
                };
                emit_compound_select(
                    program,
                    compound_select,
                    schema,
                    syms,
                    None,
                    None,
                    reg_result_cols_start,
                )?;

                right_most.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: dedupe_index.0,
                    index: dedupe_index.1.clone(),
                };
                emit_query(program, &mut right_most, &mut right_most_ctx)?;

                if new_dedupe_index {
                    let label_jump_over_dedupe = program.allocate_label();
                    emit_forward_all_rows(
                        program,
                        dedupe_index.0,
                        dedupe_index.1.as_ref(),
                        &original_destination,
                        limit_ctx,
                        offset_ctx,
                        label_jump_over_dedupe,
                    );
                    program.preassign_label_to_next_insn(label_jump_over_dedupe);
                }
            }
            CompoundOperator::Intersect => {
                // INTERSECT relies on the same ephemeral-unique-index machinery as UNION (see
                // that arm above), so it is gated behind the same feature flag.
                if cfg!(not(feature = "index_experimental")) {
                    crate::bail_parse_error!("INTERSECT not supported without indexes");
                }
                // Unlike UNION (see above), INTERSECT never reuses an inherited `EphemeralIndex`
                // as its own left-hand accumulator: an inherited index here would belong to some
                // OTHER, unrelated operator instance one level up the chain (e.g. in
                // `A INTERSECT B INTERSECT C`, grouped `(A INTERSECT B) INTERSECT C`, the *outer*
                // INTERSECT's own accumulator is a different index than the *inner* INTERSECT's),
                // and conflating the two would silently corrupt the result. Each INTERSECT
                // instance therefore always materializes its own left-hand operand fresh.
                //
                // A run of consecutive UNION/UNION ALL operators immediately to the left is still
                // handled correctly and efficiently: `Union`'s own reuse check (above) sees this
                // freshly created accumulator as its inherited destination and feeds it directly
                // (see that arm's doc comment), exactly as it would for an enclosing UNION.
                let original_destination = right_most.query_destination.clone();

                let left_accum = create_compound_dedupe_index(program, &right_most, "intersect");
                plan.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: left_accum.0,
                    index: left_accum.1.clone(),
                };
                let compound_select = Plan::CompoundSelect {
                    left,
                    right_most: plan,
                    limit,
                    offset,
                    order_by,
                    limit_expr: None,
                    offset_expr: None,
                };
                emit_compound_select(
                    program,
                    compound_select,
                    schema,
                    syms,
                    None,
                    None,
                    reg_result_cols_start,
                )?;

                // Materialize the right-hand operand into its own deduplicated ephemeral index, so
                // that a right-hand row repeated more than once is only probed/forwarded once.
                let right_dedup =
                    create_compound_dedupe_index(program, &right_most, "intersect_probe");
                right_most.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: right_dedup.0,
                    index: right_dedup.1.clone(),
                };
                emit_query(program, &mut right_most, &mut right_most_ctx)?;

                // Every distinct right-hand row that is *also* present in the left accumulator
                // belongs in the intersection; forward it to the operator's true destination
                // immediately. No separate "confirmed" index is needed: `right_dedup` already
                // guarantees each qualifying key is visited exactly once.
                let label_jump_over = program.allocate_label();
                emit_intersect_probe_and_forward(
                    program,
                    right_dedup.0,
                    right_dedup.1.as_ref(),
                    left_accum.0,
                    &original_destination,
                    limit_ctx,
                    offset_ctx,
                    label_jump_over,
                );
                program.preassign_label_to_next_insn(label_jump_over);
            }
            CompoundOperator::Except => {
                // EXCEPT relies on the same ephemeral-unique-index machinery as UNION (see that
                // arm above), so it is gated behind the same feature flag.
                if cfg!(not(feature = "index_experimental")) {
                    crate::bail_parse_error!("EXCEPT not supported without indexes");
                }
                // See the `Intersect` arm's doc comment: EXCEPT likewise always materializes its
                // own left-hand accumulator fresh, for the same reason.
                let original_destination = right_most.query_destination.clone();

                let left_accum = create_compound_dedupe_index(program, &right_most, "except");
                plan.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: left_accum.0,
                    index: left_accum.1.clone(),
                };
                let compound_select = Plan::CompoundSelect {
                    left,
                    right_most: plan,
                    limit,
                    offset,
                    order_by,
                    limit_expr: None,
                    offset_expr: None,
                };
                emit_compound_select(
                    program,
                    compound_select,
                    schema,
                    syms,
                    None,
                    None,
                    reg_result_cols_start,
                )?;

                let right_dedup =
                    create_compound_dedupe_index(program, &right_most, "except_probe");
                right_most.query_destination = QueryDestination::EphemeralIndex {
                    cursor_id: right_dedup.0,
                    index: right_dedup.1.clone(),
                };
                emit_query(program, &mut right_most, &mut right_most_ctx)?;

                // Remove every right-hand row (deduplicated, so each key is subtracted at most
                // once) from the left accumulator. Whatever remains afterwards is exactly
                // left-minus-right, each surviving key exactly once (it started out deduplicated --
                // see `create_compound_dedupe_index` -- and deletion only ever removes keys, never
                // adds any).
                emit_except_subtract(program, right_dedup.0, right_dedup.1.as_ref(), left_accum.0);

                let label_jump_over = program.allocate_label();
                emit_forward_all_rows(
                    program,
                    left_accum.0,
                    left_accum.1.as_ref(),
                    &original_destination,
                    limit_ctx,
                    offset_ctx,
                    label_jump_over,
                );
                program.preassign_label_to_next_insn(label_jump_over);
            }
        },
        None => {
            if limit_ctx.is_some() || offset_ctx.is_some() {
                right_most_ctx.limit_ctx = limit_ctx;
                right_most_ctx.reg_offset = offset_ctx;
                right_most.limit = limit;
                right_most.offset = offset;
            }
            emit_query(program, &mut right_most, &mut right_most_ctx)?;
        }
    }

    Ok(())
}

/// Creates a fresh ephemeral UNIQUE index used to deduplicate rows for UNION, or to materialize
/// one operand of INTERSECT/EXCEPT as a proper (duplicate-free) set. `schema_source`'s result
/// columns are used only to name the index's columns for `EXPLAIN`/debug fidelity -- every operand
/// of a compound SELECT is guaranteed (checked in `select::prepare_select_plan`) to have the same
/// number of result columns, so which particular arm's names are borrowed here has no effect on
/// behavior, only on how the ephemeral index's schema prints.
fn create_compound_dedupe_index(
    program: &mut ProgramBuilder,
    schema_source: &SelectPlan,
    debug_name: &str,
) -> (usize, Arc<Index>) {
    let dedupe_index = Arc::new(Index {
        columns: schema_source
            .result_columns
            .iter()
            .map(|c| IndexColumn {
                name: c
                    .name(&schema_source.table_references)
                    .map(|n| n.to_string())
                    .unwrap_or_default(),
                order: SortOrder::Asc,
                pos_in_table: 0,
                default: None,
                collation: None, // FIXME: this should be inferred
            })
            .collect(),
        name: format!("{debug_name}_dedupe"),
        root_page: 0,
        ephemeral: true,
        table_name: String::new(),
        unique: true,
        has_rowid: false,
        // Ephemeral (never registered in `Schema`, never consulted by DML
        // conflict-resolution codegen): the resolve action is irrelevant.
        on_conflict: ResolveType::Abort,
    });
    let cursor_id = program.alloc_cursor_id(CursorType::BTreeIndex(dedupe_index.clone()));
    program.emit_insn(Insn::OpenEphemeral {
        cursor_id,
        is_table: false,
    });
    (cursor_id, dedupe_index.clone())
}

/// Forwards one already-materialized row -- its `num_cols` values sitting in
/// `row_cols_start_reg..row_cols_start_reg+num_cols` -- to its true destination:
/// - `EphemeralIndex` (this operator's result is itself feeding an *enclosing*
///   UNION/INTERSECT/EXCEPT's own accumulator): insert it (deduplicating via the unique index).
///   Not final output yet, so LIMIT/OFFSET must not apply.
/// - `Sorter` (this compound SELECT has an ORDER BY, see `QueryDestination::Sorter`): copy this
///   row's sort-key values out (per `sort_key_ordinals`) and insert the combined
///   `[keys][row]` record. Likewise not final output yet -- the sort + LIMIT/OFFSET pass happens
///   once, afterwards, in `emit_sorted_compound_rows`.
/// - `ResultRows`/`CoroutineYield`: genuine final output. Apply OFFSET (skip -- jumping to
///   `label_offset_skip`, the caller's "advance to the next row" point -- without emitting or
///   touching LIMIT) then emit (`ResultRow`/`Yield`) then LIMIT (`DecrJumpZero` to
///   `label_limit_reached`).
///
/// For the `CoroutineYield` case, `row_cols_start_reg` **must** already be `yield_reg + 1`
/// (callers get this via [`forward_row_start_reg`], the same convention
/// `QueryDestination::CoroutineYield`'s other consumer, `emit_result_row_and_limit`, relies on):
/// whatever coroutine-driving code resumes after `Insn::Yield` reads the yielded row's columns
/// from that fixed, known location, and does not look at `row_cols_start_reg` at all (it has no
/// way to -- this function's caller, not its own bytecode, is what "returns" that location, and
/// only implicitly, by convention).
fn emit_forward_row(
    program: &mut ProgramBuilder,
    row_cols_start_reg: usize,
    num_cols: usize,
    destination: &QueryDestination,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
    label_limit_reached: BranchOffset,
    label_offset_skip: BranchOffset,
) {
    match destination {
        QueryDestination::EphemeralIndex { cursor_id, index } => {
            let record_reg = program.alloc_register();
            program.emit_insn(Insn::MakeRecord {
                start_reg: row_cols_start_reg,
                count: num_cols,
                dest_reg: record_reg,
                index_name: Some(index.name.clone()),
            });
            program.emit_insn(Insn::IdxInsert {
                cursor_id: *cursor_id,
                record_reg,
                unpacked_start: None,
                unpacked_count: None,
                flags: crate::vdbe::insn::IdxInsertFlags::new(),
            });
        }
        QueryDestination::Sorter {
            cursor_id,
            reg_sorter_data,
            sort_key_ordinals,
        } => {
            let num_keys = sort_key_ordinals.len();
            let combined_start = program.alloc_registers(num_keys + num_cols);
            for (i, &ordinal) in sort_key_ordinals.iter().enumerate() {
                program.emit_insn(Insn::Copy {
                    src_reg: row_cols_start_reg + ordinal,
                    dst_reg: combined_start + i,
                    amount: 0,
                });
            }
            if num_cols > 0 {
                program.emit_insn(Insn::Copy {
                    src_reg: row_cols_start_reg,
                    dst_reg: combined_start + num_keys,
                    amount: num_cols - 1,
                });
            }
            sorter_insert(
                program,
                combined_start,
                num_keys + num_cols,
                *cursor_id,
                *reg_sorter_data,
            );
        }
        QueryDestination::ResultRows | QueryDestination::CoroutineYield { .. } => {
            if let Some(offset_ctx) = offset_ctx {
                program.emit_insn(Insn::IfPos {
                    reg: offset_ctx,
                    target_pc: label_offset_skip,
                    decrement_by: 1,
                });
            }
            if let QueryDestination::CoroutineYield { yield_reg, .. } = destination {
                program.emit_insn(Insn::Yield {
                    yield_reg: *yield_reg,
                    end_offset: BranchOffset::Offset(0),
                });
            } else {
                program.emit_insn(Insn::ResultRow {
                    start_reg: row_cols_start_reg,
                    count: num_cols,
                });
            }
            if let Some(limit_ctx) = limit_ctx {
                program.emit_insn(Insn::DecrJumpZero {
                    reg: limit_ctx.reg_limit,
                    target_pc: label_limit_reached,
                });
            }
        }
    }
}

/// The register range a row's `num_cols` values should be read/copied into before calling
/// [`emit_forward_row`], chosen the same way `read_deduplicated_union_rows` (this crate's
/// original, UNION-only predecessor of the forwarding helpers here) always has: for
/// `CoroutineYield`, the fixed `yield_reg + 1` location that `Insn::Yield`'s caller reads a
/// yielded row's columns from by convention (see `emit_forward_row`'s doc comment) -- reusing any
/// other registers here would leave that location holding stale data from whatever last wrote it.
/// For every other destination, a fresh register range is fine (nothing outside this function's
/// own emitted bytecode depends on *which* registers hold the row).
fn forward_row_start_reg(
    program: &mut ProgramBuilder,
    destination: &QueryDestination,
    num_cols: usize,
) -> usize {
    match destination {
        QueryDestination::CoroutineYield { yield_reg, .. } => yield_reg + 1,
        _ => program.alloc_registers(num_cols),
    }
}

/// Iterates every row of `source_cursor_id` (an ephemeral index with `source_index.columns.len()`
/// columns) and forwards each one to `destination` via [`emit_forward_row`]. Used for UNION's
/// deduplicated read-back and for EXCEPT's final read-back of whatever remains in the left
/// accumulator after every right-hand row has been subtracted out.
fn emit_forward_all_rows(
    program: &mut ProgramBuilder,
    source_cursor_id: usize,
    source_index: &Index,
    destination: &QueryDestination,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
    label_limit_reached: BranchOffset,
) {
    let label_next = program.allocate_label();
    let label_loop_start = program.allocate_label();
    let num_cols = source_index.columns.len();
    let cols_start_reg = forward_row_start_reg(program, destination, num_cols);
    program.emit_insn(Insn::Rewind {
        cursor_id: source_cursor_id,
        pc_if_empty: label_next,
    });
    program.preassign_label_to_next_insn(label_loop_start);
    for col_idx in 0..num_cols {
        program.emit_insn(Insn::Column {
            cursor_id: source_cursor_id,
            column: col_idx,
            dest: cols_start_reg + col_idx,
            default: None,
        });
    }
    emit_forward_row(
        program,
        cols_start_reg,
        num_cols,
        destination,
        limit_ctx,
        offset_ctx,
        label_limit_reached,
        label_next,
    );
    program.preassign_label_to_next_insn(label_next);
    program.emit_insn(Insn::Next {
        cursor_id: source_cursor_id,
        pc_if_next: label_loop_start,
    });
    program.emit_insn(Insn::Close {
        cursor_id: source_cursor_id,
    });
}

/// Iterates every (deduplicated) row of `right_cursor_id`, keeping only the ones that are also
/// present in `left_accum_cursor_id` (`Insn::NotFound` performs an exact full-row membership test,
/// since the probe key has exactly as many columns as the index -- see `Insn::Found`/`NotFound`'s
/// doc comments), and forwards each survivor to `destination` via [`emit_forward_row`]. This is
/// INTERSECT's core: a key belongs in the result iff it was seen on *both* sides.
#[allow(clippy::too_many_arguments)]
fn emit_intersect_probe_and_forward(
    program: &mut ProgramBuilder,
    right_cursor_id: usize,
    right_index: &Index,
    left_accum_cursor_id: usize,
    destination: &QueryDestination,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
    label_limit_reached: BranchOffset,
) {
    let label_next = program.allocate_label();
    let label_loop_start = program.allocate_label();
    let num_cols = right_index.columns.len();
    let cols_start_reg = forward_row_start_reg(program, destination, num_cols);
    program.emit_insn(Insn::Rewind {
        cursor_id: right_cursor_id,
        pc_if_empty: label_next,
    });
    program.preassign_label_to_next_insn(label_loop_start);
    for col_idx in 0..num_cols {
        program.emit_insn(Insn::Column {
            cursor_id: right_cursor_id,
            column: col_idx,
            dest: cols_start_reg + col_idx,
            default: None,
        });
    }
    // Not present in the left accumulator -- this key never appeared on the left-hand side, so it
    // is not part of the intersection. Skip straight to `Next`.
    program.emit_insn(Insn::NotFound {
        cursor_id: left_accum_cursor_id,
        target_pc: label_next,
        record_reg: cols_start_reg,
        num_regs: num_cols,
    });
    emit_forward_row(
        program,
        cols_start_reg,
        num_cols,
        destination,
        limit_ctx,
        offset_ctx,
        label_limit_reached,
        label_next,
    );
    program.preassign_label_to_next_insn(label_next);
    program.emit_insn(Insn::Next {
        cursor_id: right_cursor_id,
        pc_if_next: label_loop_start,
    });
    program.emit_insn(Insn::Close {
        cursor_id: right_cursor_id,
    });
    program.emit_insn(Insn::Close {
        cursor_id: left_accum_cursor_id,
    });
}

/// Deletes every row of `right_cursor_id` (an ephemeral index with `right_index.columns.len()`
/// columns) from `left_accum_cursor_id`, when present. A plain "is this key present on the
/// right-hand side" check (`Insn::NotFound`) guards every delete rather than deleting
/// unconditionally, since most of `left_accum`'s rows will *not* also be present in
/// `right_cursor_id`. This is EXCEPT's core: a key survives iff it was seen on the left and *not*
/// on the right.
///
/// Deletion uses plain `Insn::Delete` (delete whatever `left_accum_cursor_id` is currently
/// positioned on) rather than `Insn::IdxDelete` (re-seek by an explicit key, then verify via
/// `cursor.rowid()`): `left_accum_cursor_id` is a `has_rowid: false` index (see
/// `create_compound_dedupe_index`) -- a bare index-only B-tree with no separate integer rowid
/// concept at all -- and `cursor.rowid()` unconditionally returns `None` for such an index
/// (`get_index_rowid_from_record` bails out immediately when `!self.has_rowid()`), which makes
/// `IdxDelete`'s verification step *always* conclude "not found" and raise
/// `LimboError::Corrupt`, even when the key genuinely exists. `Insn::NotFound`'s own `seek` just
/// above already leaves the cursor positioned exactly on the matching row when it falls through
/// (found), so a plain position-relative `Delete` is both correct and simpler here.
fn emit_except_subtract(
    program: &mut ProgramBuilder,
    right_cursor_id: usize,
    right_index: &Index,
    left_accum_cursor_id: usize,
) {
    let label_next = program.allocate_label();
    let label_loop_start = program.allocate_label();
    let num_cols = right_index.columns.len();
    let cols_start_reg = program.alloc_registers(num_cols);
    program.emit_insn(Insn::Rewind {
        cursor_id: right_cursor_id,
        pc_if_empty: label_next,
    });
    program.preassign_label_to_next_insn(label_loop_start);
    for col_idx in 0..num_cols {
        program.emit_insn(Insn::Column {
            cursor_id: right_cursor_id,
            column: col_idx,
            dest: cols_start_reg + col_idx,
            default: None,
        });
    }
    program.emit_insn(Insn::NotFound {
        cursor_id: left_accum_cursor_id,
        target_pc: label_next,
        record_reg: cols_start_reg,
        num_regs: num_cols,
    });
    program.emit_insn(Insn::Delete {
        cursor_id: left_accum_cursor_id,
    });
    program.preassign_label_to_next_insn(label_next);
    program.emit_insn(Insn::Next {
        cursor_id: right_cursor_id,
        pc_if_next: label_loop_start,
    });
    program.emit_insn(Insn::Close {
        cursor_id: right_cursor_id,
    });
}

/// Returns the left-most arm's [`SelectPlan`] of a [`Plan::CompoundSelect`] -- `left[0]`, if any
/// left-hand operands exist, else `right_most` itself (a compound with no left entries is not
/// actually a compound, but this stays total rather than panicking on that degenerate case).
fn left_most_arm(plan: &Plan) -> &SelectPlan {
    match plan {
        Plan::CompoundSelect {
            left, right_most, ..
        } => left.first().map(|(p, _)| p).unwrap_or(right_most),
        _ => unreachable!("left_most_arm called on a non-compound plan"),
    }
}

/// The collating sequence a compound SELECT's ORDER BY should use for its `ordinal`-th (0-based)
/// sort key, mirroring `order_by::init_order_by`'s per-key collation lookup for a plain SELECT:
/// the declared collation of the underlying column when the result column is a bare column
/// reference, else the default (BINARY) collating sequence. `select::resolve_compound_order_by`
/// resolves both ordinal positions and column names against the left-most arm (matching SQLite's
/// documented behavior that compound-SELECT `ORDER BY` names/types follow the left-most SELECT),
/// so `plan` here should always be that same left-most arm (see `left_most_arm`).
fn column_collation(plan: &SelectPlan, ordinal: usize) -> Option<CollationSeq> {
    let rc = plan.result_columns.get(ordinal)?;
    if let ast::Expr::Column { table, column, .. } = &rc.expr {
        if let Some(table_ref) = plan.table_references.find_table_by_internal_id(*table) {
            if let Some(table_column) = table_ref.get_column_at(*column) {
                return table_column.collation;
            }
        }
    }
    Some(CollationSeq::default())
}

/// Emits a compound SELECT that has an ORDER BY.
///
/// Plain per-arm streaming (as `emit_compound_select` does on its own) cannot honor an ORDER BY
/// that spans the *entire* combined result: values need to be interleaved across arms, not just
/// sorted within each one. Instead, every arm's rows -- after each operator's own set-operation
/// processing (dedup for UNION, membership-intersection for INTERSECT, membership-subtraction for
/// EXCEPT, plain pass-through for UNION ALL, all of which are completely unchanged from the
/// non-ORDER-BY path) -- are materialized into one shared `Sorter` (`QueryDestination::Sorter`)
/// instead of being streamed to the real output directly. Once every arm has contributed, a single
/// sort + LIMIT/OFFSET pass (`emit_sorted_compound_rows`) runs over the combined, sorted rows.
fn emit_compound_select_order_by(
    program: &mut ProgramBuilder,
    mut plan: Plan,
    schema: &Schema,
    syms: &SymbolTable,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
    order_by: Vec<(ast::Expr, SortOrder)>,
    right_plan: &SelectPlan,
) -> Result<()> {
    // `select::resolve_compound_order_by` always resolves every ORDER BY term to a 1-based
    // ordinal position, represented as a numeric-literal expression purely so
    // `Plan::CompoundSelect::order_by`'s existing `Vec<(ast::Expr, SortOrder)>` field shape didn't
    // need to change (see its doc comment); unpack that representation back into plain ordinals
    // here.
    let mut ordinals = Vec::with_capacity(order_by.len());
    for (expr, dir) in &order_by {
        let ast::Expr::Literal(ast::Literal::Numeric(n)) = expr else {
            return Err(LimboError::InternalError(
                "compound SELECT ORDER BY term was not pre-resolved to an ordinal position"
                    .to_string(),
            ));
        };
        let ordinal_1_based: usize = n.parse().map_err(|_| {
            LimboError::InternalError("invalid ordinal position in compound ORDER BY".to_string())
        })?;
        if ordinal_1_based == 0 {
            return Err(LimboError::InternalError(
                "invalid ordinal position in compound ORDER BY".to_string(),
            ));
        }
        ordinals.push((ordinal_1_based - 1, *dir));
    }

    let num_result_cols = right_plan.result_columns.len();
    let sort_orders: Vec<SortOrder> = ordinals.iter().map(|(_, d)| *d).collect();
    let name_source = left_most_arm(&plan);
    let collations: Vec<Option<CollationSeq>> = ordinals
        .iter()
        .map(|&(ordinal, _)| column_collation(name_source, ordinal))
        .collect();
    let sort_key_ordinals: Rc<Vec<usize>> = Rc::new(ordinals.iter().map(|&(o, _)| o).collect());
    let num_keys = sort_key_ordinals.len();

    let sort_cursor = program.alloc_cursor_id(CursorType::Sorter);
    program.emit_insn(Insn::SorterOpen {
        cursor_id: sort_cursor,
        columns: num_keys,
        order: sort_orders,
        collations,
    });
    let reg_sorter_data = program.alloc_register();

    // Reserve `[sort keys][output columns]` as ONE contiguous register block, shared by every arm
    // exactly like the `yield_reg + 1` convention `QueryDestination::CoroutineYield` relies on --
    // see `emit_result_row_and_limit`'s `Sorter` branch and `QueryDestination::Sorter`'s doc
    // comment.
    let key_block_start = program.alloc_registers(num_keys + num_result_cols);
    let reg_result_cols_start = key_block_start + num_keys;

    // The compound SELECT's *real* destination, captured before it gets temporarily overwritten
    // with `Sorter` below -- this is where `emit_sorted_compound_rows` sends the final, sorted
    // rows. A compound SELECT can never itself appear in subquery-expression position or as a
    // FROM-clause subquery/CTE body (see `subquery::plan_expr_subquery` and
    // `planner::parse_from_clause_table`'s `SelectTable::Select` arm, both of which reject
    // `Plan::CompoundSelect`), so this is always `ResultRows` or `CoroutineYield`, never
    // `EphemeralIndex`/`Sorter` itself.
    let original_destination = right_plan.query_destination.clone();

    if let Plan::CompoundSelect { right_most, .. } = &mut plan {
        right_most.query_destination = QueryDestination::Sorter {
            cursor_id: sort_cursor,
            reg_sorter_data,
            sort_key_ordinals,
        };
    }

    // Materialize: identical machinery to the non-ORDER-BY path (every operator's set-operation
    // handling is completely unchanged), just with every genuine "final output" point routed into
    // the sorter instead, and no LIMIT/OFFSET applied yet -- which rows survive isn't known until
    // every arm has contributed and the whole combined result has been sorted.
    emit_compound_select(
        program,
        plan,
        schema,
        syms,
        None,
        None,
        Some(reg_result_cols_start),
    )?;

    emit_sorted_compound_rows(
        program,
        sort_cursor,
        reg_sorter_data,
        num_keys,
        num_result_cols,
        &original_destination,
        limit_ctx,
        offset_ctx,
    )
}

/// Reads back every row from the compound SELECT's ORDER BY materialization `Sorter`, in sorted
/// order, and emits it to `destination` (the compound SELECT's genuine, real final destination),
/// honoring OFFSET then LIMIT exactly once per row. Mirrors `order_by::emit_order_by`'s opcode
/// sequence (`SorterSort` / `SorterData` via an `OpenPseudo` pseudo-cursor / `SorterNext`),
/// adapted to a plain `(cursor_id, reg_sorter_data)` pair instead of a single `SelectPlan`'s
/// `TranslateCtx`/`SortMetadata`, since a compound SELECT's combined result set has no single
/// owning `SelectPlan`.
#[allow(clippy::too_many_arguments)]
fn emit_sorted_compound_rows(
    program: &mut ProgramBuilder,
    sort_cursor: usize,
    reg_sorter_data: usize,
    num_keys: usize,
    num_result_cols: usize,
    destination: &QueryDestination,
    limit_ctx: Option<LimitCtx>,
    offset_ctx: Option<usize>,
) -> Result<()> {
    let num_columns_in_sorter = num_keys + num_result_cols;
    // Field names/types don't matter here -- only the count, so the pseudo-cursor can address
    // individual fields of a sorted record via `Insn::Column` -- see `order_by::emit_order_by`'s
    // identical use of a `PseudoTable` for the same purpose.
    let pseudo_columns: Vec<Column> = (0..num_columns_in_sorter)
        .map(|_| Column {
            name: None,
            primary_key: false,
            ty: Type::Null,
            ty_str: Type::Null.to_string().to_uppercase(),
            is_rowid_alias: false,
            notnull: false,
            default: None,
            unique: false,
            unique_conflict: ResolveType::Abort,
            collation: None,
            is_generated: false,
        })
        .collect();
    let pseudo_table = Rc::new(PseudoTable {
        columns: pseudo_columns,
    });
    let pseudo_cursor = program.alloc_cursor_id(CursorType::Pseudo(pseudo_table));
    program.emit_insn(Insn::OpenPseudo {
        cursor_id: pseudo_cursor,
        content_reg: reg_sorter_data,
        num_fields: num_columns_in_sorter,
    });

    let sort_loop_start_label = program.allocate_label();
    let sort_loop_next_label = program.allocate_label();
    let sort_loop_end_label = program.allocate_label();

    program.emit_insn(Insn::SorterSort {
        cursor_id: sort_cursor,
        pc_if_empty: sort_loop_end_label,
    });
    program.preassign_label_to_next_insn(sort_loop_start_label);

    if let Some(offset_ctx) = offset_ctx {
        program.emit_insn(Insn::IfPos {
            reg: offset_ctx,
            target_pc: sort_loop_next_label,
            decrement_by: 1,
        });
    }

    program.emit_insn(Insn::SorterData {
        cursor_id: sort_cursor,
        dest_reg: reg_sorter_data,
        pseudo_cursor,
    });

    let (yield_reg, result_cols_start_reg) = match destination {
        QueryDestination::CoroutineYield { yield_reg, .. } => (Some(*yield_reg), *yield_reg + 1),
        QueryDestination::ResultRows => (None, program.alloc_registers(num_result_cols)),
        QueryDestination::EphemeralIndex { .. } | QueryDestination::Sorter { .. } => {
            return Err(LimboError::InternalError(
                "compound SELECT's real destination must be ResultRows or CoroutineYield"
                    .to_string(),
            ));
        }
    };
    for i in 0..num_result_cols {
        program.emit_column(pseudo_cursor, num_keys + i, result_cols_start_reg + i);
    }

    if let Some(yield_reg) = yield_reg {
        program.emit_insn(Insn::Yield {
            yield_reg,
            end_offset: BranchOffset::Offset(0),
        });
    } else {
        program.emit_insn(Insn::ResultRow {
            start_reg: result_cols_start_reg,
            count: num_result_cols,
        });
    }
    if let Some(limit_ctx) = limit_ctx {
        program.emit_insn(Insn::DecrJumpZero {
            reg: limit_ctx.reg_limit,
            target_pc: sort_loop_end_label,
        });
    }

    program.preassign_label_to_next_insn(sort_loop_next_label);
    program.emit_insn(Insn::SorterNext {
        cursor_id: sort_cursor,
        pc_if_next: sort_loop_start_label,
    });
    program.preassign_label_to_next_insn(sort_loop_end_label);
    Ok(())
}
