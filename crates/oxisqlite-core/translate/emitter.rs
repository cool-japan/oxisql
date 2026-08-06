// This module contains code for emitting bytecode instructions for SQL query execution.
// It handles translating high-level SQL operations into low-level bytecode that can be executed by the virtual machine.

use std::rc::Rc;

use limbo_sqlite3_parser::ast::{self};
use tracing::{instrument, Level};

use super::aggregation::emit_ungrouped_aggregation;
use super::expr::{
    expr_contains_subquery, translate_condition_expr, translate_expr, ConditionMetadata,
};
use super::group_by::{
    group_by_agg_phase, group_by_emit_row_phase, init_group_by, GroupByMetadata, GroupByRowSource,
};
use super::main_loop::{
    close_loop, emit_loop, init_distinct, init_loop, open_loop, LeftJoinMetadata, LoopLabels,
};
use super::order_by::{emit_order_by, init_order_by, SortMetadata};
use super::plan::{JoinOrderMember, Operation, SelectPlan, TableReferences, UpdatePlan};
use super::select::emit_simple_count;
use super::subquery::emit_subqueries;
use crate::error::{LimboError, SQLITE_CONSTRAINT_PRIMARYKEY};
use crate::function::Func;
use crate::schema::Schema;
use crate::translate::compound_select::emit_program_for_compound_select;
use crate::translate::plan::{DeletePlan, Plan, Search};
use crate::translate::values::emit_values;
use crate::util::exprs_are_equivalent;
use crate::vdbe::builder::{CursorKey, CursorType, ProgramBuilder};
use crate::vdbe::insn::{CmpInsFlags, IdxInsertFlags, InsertFlags, RegisterOrLiteral};
use crate::vdbe::{insn::Insn, BranchOffset};
use crate::{Result, SymbolTable};

pub struct Resolver<'a> {
    pub schema: &'a Schema,
    pub symbol_table: &'a SymbolTable,
    pub expr_to_reg_cache: Vec<(&'a ast::Expr, usize)>,
    /// Owned expression→register overrides used inside `DO UPDATE` upsert blocks.
    /// Checked before `expr_to_reg_cache` so that `excluded.*` and old-value
    /// references shadow any outer cache entries during DO UPDATE emit.
    pub upsert_reg_overrides: Vec<(ast::Expr, usize)>,
}

impl<'a> Resolver<'a> {
    pub fn new(schema: &'a Schema, symbol_table: &'a SymbolTable) -> Self {
        Self {
            schema,
            symbol_table,
            expr_to_reg_cache: Vec::new(),
            upsert_reg_overrides: Vec::new(),
        }
    }

    pub fn resolve_function(&self, func_name: &str, arg_count: usize) -> Option<Func> {
        match Func::resolve_function(func_name, arg_count).ok() {
            Some(func) => Some(func),
            None => self
                .symbol_table
                .resolve_function(func_name, arg_count)
                .map(|arg| Func::External(arg.clone())),
        }
    }

    pub fn resolve_cached_expr_reg(&self, expr: &ast::Expr) -> Option<usize> {
        // Check owned overrides first (upsert DO UPDATE context).
        for (key, reg) in &self.upsert_reg_overrides {
            if exprs_are_equivalent(expr, key) {
                return Some(*reg);
            }
        }
        self.expr_to_reg_cache
            .iter()
            .find(|(e, _)| exprs_are_equivalent(expr, e))
            .map(|(_, reg)| *reg)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LimitCtx {
    /// Register holding the LIMIT value (e.g. LIMIT 5)
    pub reg_limit: usize,
    /// Whether to initialize the LIMIT counter to the LIMIT value;
    /// There are cases like compound SELECTs where all the sub-selects
    /// utilize the same limit register, but it is initialized only once.
    pub initialize_counter: bool,
}

impl LimitCtx {
    pub fn new(program: &mut ProgramBuilder) -> Self {
        Self {
            reg_limit: program.alloc_register(),
            initialize_counter: true,
        }
    }

    pub fn new_shared(reg_limit: usize) -> Self {
        Self {
            reg_limit,
            initialize_counter: false,
        }
    }
}

/// The TranslateCtx struct holds various information and labels used during bytecode generation.
/// It is used for maintaining state and control flow during the bytecode
/// generation process.
pub struct TranslateCtx<'a> {
    // A typical query plan is a nested loop. Each loop has its own LoopLabels (see the definition of LoopLabels for more details)
    pub labels_main_loop: Vec<LoopLabels>,
    // label for the instruction that jumps to the next phase of the query after the main loop
    // we don't know ahead of time what that is (GROUP BY, ORDER BY, etc.)
    pub label_main_loop_end: Option<BranchOffset>,
    // label for immediately aborting all processing (used by parameterized LIMIT=0 to skip ORDER BY/GROUP BY)
    pub label_abort: Option<BranchOffset>,
    // First register of the aggregation results
    pub reg_agg_start: Option<usize>,
    // In non-group-by statements with aggregations (e.g. SELECT foo, bar, sum(baz) FROM t),
    // we want to emit the non-aggregate columns (foo and bar) only once.
    // This register is a flag that tracks whether we have already done that.
    pub reg_nonagg_emit_once_flag: Option<usize>,
    // First register of the result columns of the query
    pub reg_result_cols_start: Option<usize>,
    pub limit_ctx: Option<LimitCtx>,
    // The register holding the offset value, if any.
    pub reg_offset: Option<usize>,
    // The register holding the limit+offset value, if any.
    pub reg_limit_offset_sum: Option<usize>,
    // metadata for the group by operator
    pub meta_group_by: Option<GroupByMetadata>,
    // metadata for the order by operator
    pub meta_sort: Option<SortMetadata>,
    /// Set by `main_loop::open_loop` when a virtual table scan's `xBestIndex` reports
    /// (`order_by_consumed`) that it already produces rows in exactly the order the query's
    /// `ORDER BY` demands. When true, `emit_loop`'s dispatch and `emit_query`'s
    /// `order_by_necessary` computation both skip the ORDER BY sorter -- it stays allocated
    /// (opened by `init_order_by` before `open_loop` runs) but is left empty and unused.
    pub vtab_order_by_consumed: bool,
    /// mapping between table loop index and associated metadata (for left joins only)
    /// this metadata exists for the right table in a given left join
    pub meta_left_joins: Vec<Option<LeftJoinMetadata>>,
    // We need to emit result columns in the order they are present in the SELECT, but they may not be in the same order in the ORDER BY sorter.
    // This vector holds the indexes of the result columns in the ORDER BY sorter.
    pub result_column_indexes_in_orderby_sorter: Vec<usize>,
    // We might skip adding a SELECT result column into the ORDER BY sorter if it is an exact match in the ORDER BY keys.
    // This vector holds the indexes of the result columns that we need to skip.
    pub result_columns_to_skip_in_orderby_sorter: Option<Vec<usize>>,
    pub resolver: Resolver<'a>,
}

impl<'a> TranslateCtx<'a> {
    pub fn new(
        program: &mut ProgramBuilder,
        schema: &'a Schema,
        syms: &'a SymbolTable,
        table_count: usize,
        result_column_count: usize,
    ) -> Self {
        TranslateCtx {
            labels_main_loop: (0..table_count).map(|_| LoopLabels::new(program)).collect(),
            label_main_loop_end: None,
            label_abort: None,
            reg_agg_start: None,
            reg_nonagg_emit_once_flag: None,
            limit_ctx: None,
            reg_offset: None,
            reg_limit_offset_sum: None,
            reg_result_cols_start: None,
            meta_group_by: None,
            meta_left_joins: (0..table_count).map(|_| None).collect(),
            meta_sort: None,
            vtab_order_by_consumed: false,
            result_column_indexes_in_orderby_sorter: (0..result_column_count).collect(),
            result_columns_to_skip_in_orderby_sorter: None,
            resolver: Resolver::new(schema, syms),
        }
    }
}

/// Used to distinguish database operations
#[allow(clippy::upper_case_acronyms, dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationMode {
    SELECT,
    INSERT,
    UPDATE,
    DELETE,
}

#[derive(Clone, Copy, Debug)]
pub enum TransactionMode {
    None,
    Read,
    Write,
}

/// Main entry point for emitting bytecode for a SQL query
/// Takes a query plan and generates the corresponding bytecode program
#[instrument(skip_all, level = Level::TRACE)]
pub fn emit_program(
    program: &mut ProgramBuilder,
    plan: Plan,
    schema: &Schema,
    syms: &SymbolTable,
    after: impl FnOnce(&mut ProgramBuilder),
) -> Result<()> {
    match plan {
        Plan::Select(plan) => emit_program_for_select(program, plan, schema, syms),
        Plan::Delete(plan) => emit_program_for_delete(program, plan, schema, syms),
        Plan::Update(plan) => emit_program_for_update(program, plan, schema, syms, after),
        Plan::CompoundSelect { .. } => {
            emit_program_for_compound_select(program, plan, schema, syms)
        }
    }
}

#[instrument(skip_all, level = Level::TRACE)]
fn emit_program_for_select(
    program: &mut ProgramBuilder,
    mut plan: SelectPlan,
    schema: &Schema,
    syms: &SymbolTable,
) -> Result<()> {
    let mut t_ctx = TranslateCtx::new(
        program,
        schema,
        syms,
        plan.table_references.joined_tables().len(),
        plan.result_columns.len(),
    );

    // Trivial exit on LIMIT 0 (only for literal LIMIT, not parameterized)
    if plan.limit_expr.is_none() {
        if let Some(0) = plan.limit {
            program.epilogue(TransactionMode::Read);
            program.result_columns = plan.result_columns;
            program.table_references.extend(plan.table_references);
            return Ok(());
        }
    }
    // Emit main parts of query
    emit_query(program, &mut plan, &mut t_ctx)?;

    // Finalize program
    if plan.table_references.joined_tables().is_empty() {
        program.epilogue(TransactionMode::None);
    } else {
        program.epilogue(TransactionMode::Read);
    }

    program.result_columns = plan.result_columns;
    program.table_references.extend(plan.table_references);
    Ok(())
}

#[instrument(skip_all, level = Level::TRACE)]
pub fn emit_query<'a>(
    program: &'a mut ProgramBuilder,
    plan: &'a mut SelectPlan,
    t_ctx: &'a mut TranslateCtx<'a>,
) -> Result<usize> {
    if !plan.values.is_empty() {
        let reg_result_cols_start = emit_values(program, &plan, &t_ctx.resolver)?;
        return Ok(reg_result_cols_start);
    }

    // Emit subqueries first so the results can be read in the main query loop.
    emit_subqueries(program, t_ctx, &mut plan.table_references)?;

    // Allocate end-of-loop label first so init_limit can emit LIMIT-0 jumps
    let after_main_loop_label = program.allocate_label();
    t_ctx.label_main_loop_end = Some(after_main_loop_label);

    // Allocate abort label (points past ORDER BY / GROUP BY post-processing) so that
    // a parameterized LIMIT=0 at runtime can skip all processing without touching cursors.
    let abort_label = program.allocate_label();
    t_ctx.label_abort = Some(abort_label);

    // No rows will be read from source table loops if there is a constant false condition eg. WHERE 0
    // however an aggregation might still happen,
    // e.g. SELECT COUNT(*) WHERE 0 returns a row with 0, not an empty result set
    init_limit(
        program,
        t_ctx,
        plan.limit,
        plan.offset,
        plan.limit_expr.as_deref(),
        plan.offset_expr.as_deref(),
    )?;

    if plan.contains_constant_false_condition {
        program.emit_insn(Insn::Goto {
            target_pc: after_main_loop_label,
        });
    }

    // For non-grouped aggregation queries that also have non-aggregate columns,
    // we need to ensure non-aggregate columns are only emitted once.
    // This flag helps track whether we've already emitted these columns.
    if !plan.aggregates.is_empty()
        && plan.group_by.is_none()
        && plan
            .result_columns
            .iter()
            .any(|c| c.contains_aggregates.is_empty())
    {
        let flag = program.alloc_register();
        program.emit_int(0, flag); // Initialize flag to 0 (not yet emitted)
        t_ctx.reg_nonagg_emit_once_flag = Some(flag);
    }

    // Allocate registers for result columns
    if t_ctx.reg_result_cols_start.is_none() {
        t_ctx.reg_result_cols_start = Some(program.alloc_registers(plan.result_columns.len()));
    }

    // Initialize cursors and other resources needed for query execution
    if let Some(ref mut order_by) = plan.order_by {
        init_order_by(program, t_ctx, order_by, &plan.table_references)?;
    }

    if let Some(ref group_by) = plan.group_by {
        init_group_by(program, t_ctx, group_by, &plan)?;
    } else if !plan.aggregates.is_empty() {
        // Aggregate registers need to be NULLed at the start because the same registers might be reused on another invocation of a subquery,
        // and if they are not NULLed, the 2nd invocation of the same subquery will have values left over from the first invocation.
        t_ctx.reg_agg_start = Some(program.alloc_registers_and_init_w_null(plan.aggregates.len()));
    }

    init_distinct(program, plan);
    init_loop(
        program,
        t_ctx,
        &plan.table_references,
        &mut plan.aggregates,
        plan.group_by.as_ref(),
        OperationMode::SELECT,
    )?;

    if plan.is_simple_count() {
        emit_simple_count(program, t_ctx, plan)?;
        return t_ctx
            .reg_result_cols_start
            .ok_or_else(|| LimboError::InternalError("reg_result_cols_start not set".to_string()));
    }

    for where_term in plan.where_clause.iter().filter(|wt| {
        // A WHERE term containing a subquery (scalar / EXISTS / IN (SELECT ...)) must never be
        // hoisted before the main loop: a correlated subquery needs the outer row's cursors to be
        // positioned first. Such terms are evaluated inside the innermost loop instead (see
        // `should_eval_term_at_loop` in main_loop.rs).
        wt.should_eval_before_loop(&plan.join_order) && !expr_contains_subquery(&wt.expr)
    }) {
        let jump_target_when_true = program.allocate_label();
        let condition_metadata = ConditionMetadata {
            jump_if_condition_is_true: false,
            jump_target_when_false: after_main_loop_label,
            jump_target_when_true,
        };
        translate_condition_expr(
            program,
            &plan.table_references,
            &where_term.expr,
            condition_metadata,
            &t_ctx.resolver,
        )?;
        program.preassign_label_to_next_insn(jump_target_when_true);
    }

    // Set up main query execution loop
    open_loop(
        program,
        t_ctx,
        &plan.table_references,
        &plan.join_order,
        &mut plan.where_clause,
        plan.order_by.as_deref(),
        plan.group_by.as_ref(),
    )?;

    // Process result columns and expressions in the inner loop
    emit_loop(program, t_ctx, plan)?;

    // Clean up and close the main execution loop
    close_loop(program, t_ctx, &plan.table_references, &plan.join_order)?;

    program.preassign_label_to_next_insn(after_main_loop_label);

    let mut order_by_necessary = plan.order_by.is_some()
        && !plan.contains_constant_false_condition
        && !t_ctx.vtab_order_by_consumed;
    let order_by = plan.order_by.as_ref();

    // Handle GROUP BY and aggregation processing
    if plan.group_by.is_some() {
        let row_source = &t_ctx
            .meta_group_by
            .as_ref()
            .expect("group by metadata not found")
            .row_source;
        if matches!(row_source, GroupByRowSource::Sorter { .. }) {
            group_by_agg_phase(program, t_ctx, plan)?;
        }
        group_by_emit_row_phase(program, t_ctx, plan)?;
    } else if !plan.aggregates.is_empty() {
        // Handle aggregation without GROUP BY
        emit_ungrouped_aggregation(program, t_ctx, plan)?;
        // Single row result for aggregates without GROUP BY, so ORDER BY not needed
        order_by_necessary = false;
    }

    // Process ORDER BY results if needed
    if order_by.is_some() && order_by_necessary {
        emit_order_by(program, t_ctx, plan)?;
    }

    // Assign the abort_label here — a parameterized LIMIT=0 jumps to this point,
    // past all post-processing (ORDER BY, GROUP BY, etc.) and straight to epilogue.
    if let Some(abort_label) = t_ctx.label_abort {
        program.preassign_label_to_next_insn(abort_label);
    }

    t_ctx
        .reg_result_cols_start
        .ok_or_else(|| LimboError::InternalError("reg_result_cols_start not set".to_string()))
}

#[instrument(skip_all, level = Level::TRACE)]
fn emit_program_for_delete(
    program: &mut ProgramBuilder,
    mut plan: DeletePlan,
    schema: &Schema,
    syms: &SymbolTable,
) -> Result<()> {
    let mut t_ctx = TranslateCtx::new(
        program,
        schema,
        syms,
        plan.table_references.joined_tables().len(),
        plan.result_columns.len(),
    );

    // exit early if LIMIT 0 (only for literal LIMIT, not parameterized)
    if plan.limit_expr.is_none() {
        if let Some(0) = plan.limit {
            program.epilogue(TransactionMode::Write);
            program.result_columns = plan.result_columns;
            program.table_references.extend(plan.table_references);
            return Ok(());
        }
    }

    let after_main_loop_label = program.allocate_label();
    t_ctx.label_main_loop_end = Some(after_main_loop_label);
    // For DELETE there is no post-loop processing, so abort_label == after_main_loop_label.
    t_ctx.label_abort = Some(after_main_loop_label);

    // No rows will be read from source table loops if there is a constant false condition eg. WHERE 0
    init_limit(
        program,
        &mut t_ctx,
        plan.limit,
        None,
        plan.limit_expr.as_deref(),
        None,
    )?;

    if plan.contains_constant_false_condition {
        program.emit_insn(Insn::Goto {
            target_pc: after_main_loop_label,
        });
    }

    // Initialize cursors and other resources needed for query execution
    init_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &mut [],
        None,
        OperationMode::DELETE,
    )?;

    // Set up main query execution loop
    open_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &[JoinOrderMember::default()],
        &mut plan.where_clause,
        None,
        None,
    )?;

    emit_delete_insns(program, &mut t_ctx, &plan.table_references)?;

    // Clean up and close the main execution loop
    close_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &[JoinOrderMember::default()],
    )?;
    program.preassign_label_to_next_insn(after_main_loop_label);

    // Finalize program
    program.epilogue(TransactionMode::Write);
    program.result_columns = plan.result_columns;
    program.table_references.extend(plan.table_references);
    Ok(())
}

fn emit_delete_insns(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    table_references: &TableReferences,
) -> Result<()> {
    let table_reference = table_references
        .joined_tables()
        .first()
        .ok_or_else(|| LimboError::InternalError("DELETE: no table reference found".to_string()))?;
    let cursor_id = match &table_reference.op {
        Operation::Scan { .. } => {
            program.resolve_cursor_id(&CursorKey::table(table_reference.internal_id))
        }
        Operation::Search(search) => match search {
            Search::RowidEq { .. } | Search::Seek { index: None, .. } => {
                program.resolve_cursor_id(&CursorKey::table(table_reference.internal_id))
            }
            Search::Seek {
                index: Some(index), ..
            } => program.resolve_cursor_id(&CursorKey::index(
                table_reference.internal_id,
                index.clone(),
            )),
        },
    };
    let main_table_cursor_id =
        program.resolve_cursor_id(&CursorKey::table(table_reference.internal_id));

    // Emit the instructions to delete the row
    let key_reg = program.alloc_register();
    program.emit_insn(Insn::RowId {
        cursor_id: main_table_cursor_id,
        dest: key_reg,
    });

    // Row triggers: the OLD image has to be captured while the cursor is still
    // positioned on the row, i.e. before `Insn::Delete` below, because AFTER
    // triggers read it too.
    let trigger_ctx = prepare_delete_trigger_context(
        program,
        t_ctx,
        table_reference,
        main_table_cursor_id,
        key_reg,
    )?;
    if let Some((btree, old_image, ignore_jump)) = trigger_ctx.as_ref() {
        crate::translate::trigger::emit_triggers(
            program,
            &t_ctx.resolver,
            &crate::translate::trigger::TriggerFireArgs {
                table: btree.as_ref(),
                time: limbo_sqlite3_parser::ast::TriggerTime::Before,
                event: crate::translate::trigger::TriggerEventKind::Delete,
                old: Some(*old_image),
                new: None,
                ignore_jump: *ignore_jump,
            },
        )?;
        // A BEFORE trigger body may have written to this table and moved the
        // cursor, or deleted this very row. Re-seek; if the row is gone, skip
        // it — upstream emits the same `OP_NotExists` guard.
        program.emit_insn(Insn::NotExists {
            cursor: main_table_cursor_id,
            rowid_reg: key_reg,
            target_pc: *ignore_jump,
        });
    }

    if let Some(_) = table_reference.virtual_table() {
        let conflict_action = 0u16;
        let start_reg = key_reg;

        let new_rowid_reg = program.alloc_register();
        program.emit_insn(Insn::Null {
            dest: new_rowid_reg,
            dest_end: None,
        });
        program.emit_insn(Insn::VUpdate {
            cursor_id,
            arg_count: 2,
            start_reg,
            conflict_action,
        });
    } else {
        // Delete from all indexes before deleting from the main table.
        let indexes = t_ctx
            .resolver
            .schema
            .indexes
            .get(table_reference.table.get_name());
        let index_refs_opt = indexes.map(|indexes| {
            indexes
                .iter()
                .map(|index| {
                    (
                        index.clone(),
                        program.resolve_cursor_id(&CursorKey::index(
                            table_reference.internal_id,
                            index.clone(),
                        )),
                    )
                })
                .collect::<Vec<_>>()
        });

        if let Some(index_refs) = index_refs_opt {
            for (index, index_cursor_id) in index_refs {
                let num_regs = index.columns.len() + 1;
                let start_reg = program.alloc_registers(num_regs);
                // Emit columns that are part of the index
                index
                    .columns
                    .iter()
                    .enumerate()
                    .for_each(|(reg_offset, column_index)| {
                        program.emit_column(
                            main_table_cursor_id,
                            column_index.pos_in_table,
                            start_reg + reg_offset,
                        );
                    });
                program.emit_insn(Insn::RowId {
                    cursor_id: main_table_cursor_id,
                    dest: start_reg + num_regs - 1,
                });
                program.emit_insn(Insn::IdxDelete {
                    start_reg,
                    num_regs,
                    cursor_id: index_cursor_id,
                });
            }
        }

        program.emit_insn(Insn::Delete {
            cursor_id: main_table_cursor_id,
        });
    }

    if let Some((btree, old_image, ignore_jump)) = trigger_ctx.as_ref() {
        crate::translate::trigger::emit_triggers(
            program,
            &t_ctx.resolver,
            &crate::translate::trigger::TriggerFireArgs {
                table: btree.as_ref(),
                time: limbo_sqlite3_parser::ast::TriggerTime::After,
                event: crate::translate::trigger::TriggerEventKind::Delete,
                old: Some(*old_image),
                new: None,
                ignore_jump: *ignore_jump,
            },
        )?;
    }

    if let Some(limit_ctx) = t_ctx.limit_ctx {
        program.emit_insn(Insn::DecrJumpZero {
            reg: limit_ctx.reg_limit,
            target_pc: t_ctx.label_main_loop_end.ok_or_else(|| {
                LimboError::InternalError("label_main_loop_end not set".to_string())
            })?,
        })
    }

    Ok(())
}

/// Normalized names of the columns an `UPDATE` assigns, for matching
/// `CREATE TRIGGER ... UPDATE OF (a, b)`.
fn changed_column_names(btree: &crate::schema::BTreeTable, plan: &UpdatePlan) -> Vec<String> {
    plan.set_clauses
        .iter()
        .filter_map(|(idx, _)| {
            btree
                .columns
                .get(*idx)
                .and_then(|c| c.name.as_deref())
                .map(crate::util::normalize_ident)
        })
        .collect()
}

/// Materialize the `OLD` row image for `DELETE` triggers, if the table has any.
///
/// Returns `None` when the table has no triggers at all, so the common case
/// costs one hash-map scan and emits no extra instructions. The image must be
/// built while the table cursor is still positioned on the victim row: `AFTER
/// DELETE` bodies read `OLD.*` after `Insn::Delete` has moved the cursor.
#[allow(clippy::type_complexity)]
fn prepare_delete_trigger_context(
    program: &mut ProgramBuilder,
    t_ctx: &TranslateCtx,
    table_reference: &crate::translate::plan::JoinedTable,
    main_table_cursor_id: usize,
    rowid_reg: usize,
) -> Result<
    Option<(
        Rc<crate::schema::BTreeTable>,
        crate::translate::trigger::RowImage,
        BranchOffset,
    )>,
> {
    let Some(btree) = table_reference.table.btree() else {
        return Ok(None);
    };
    if !crate::translate::trigger::table_has_triggers(&t_ctx.resolver, &btree.name) {
        return Ok(None);
    }
    let cols_start_reg = program.alloc_registers(btree.columns.len().max(1));
    for (idx, _) in btree.columns.iter().enumerate() {
        program.emit_column(main_table_cursor_id, idx, cols_start_reg + idx);
    }
    // `RAISE(IGNORE)` abandons this row and resumes the statement, which for a
    // scan-driven DELETE means jumping to the loop's `Next`.
    let ignore_jump = t_ctx
        .labels_main_loop
        .first()
        .map(|labels| labels.next)
        .ok_or_else(|| {
            LimboError::InternalError("DELETE: main loop labels not initialized".to_string())
        })?;
    Ok(Some((
        btree,
        crate::translate::trigger::RowImage {
            rowid_reg,
            cols_start_reg,
        },
        ignore_jump,
    )))
}

#[instrument(skip_all, level = Level::TRACE)]
fn emit_program_for_update(
    program: &mut ProgramBuilder,
    mut plan: UpdatePlan,
    schema: &Schema,
    syms: &SymbolTable,
    after: impl FnOnce(&mut ProgramBuilder),
) -> Result<()> {
    let mut t_ctx = TranslateCtx::new(
        program,
        schema,
        syms,
        plan.table_references.joined_tables().len(),
        plan.returning.as_ref().map_or(0, |r| r.len()),
    );

    // Exit on LIMIT 0 (only for literal LIMIT, not parameterized)
    if plan.limit_expr.is_none() {
        if let Some(0) = plan.limit {
            program.epilogue(TransactionMode::None);
            program.result_columns = plan.returning.unwrap_or_default();
            program.table_references.extend(plan.table_references);
            return Ok(());
        }
    }

    let after_main_loop_label = program.allocate_label();
    t_ctx.label_main_loop_end = Some(after_main_loop_label);
    // For UPDATE there is no post-loop processing, so abort_label == after_main_loop_label.
    t_ctx.label_abort = Some(after_main_loop_label);

    init_limit(
        program,
        &mut t_ctx,
        plan.limit,
        plan.offset,
        plan.limit_expr.as_deref(),
        plan.offset_expr.as_deref(),
    )?;

    if plan.contains_constant_false_condition {
        program.emit_insn(Insn::Goto {
            target_pc: after_main_loop_label,
        });
    }

    init_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &mut [],
        None,
        OperationMode::UPDATE,
    )?;
    // Open indexes for update. An index always lives in the same database as
    // the table it indexes, so the update target's registry index is reused.
    let table_db_index = plan
        .table_references
        .joined_tables()
        .first()
        .map_or(crate::multidb::DB_MAIN, |joined| joined.table.db_index());
    let mut index_cursors = Vec::with_capacity(plan.indexes_to_update.len());
    for index in &plan.indexes_to_update {
        if let Some(index_cursor) = program.resolve_cursor_id_safe(&CursorKey::index(
            plan.table_references
                .joined_tables()
                .first()
                .unwrap()
                .internal_id,
            index.clone(),
        )) {
            // Don't reopen index if it was already opened as the iteration cursor for this update plan.
            let record_reg = program.alloc_register();
            index_cursors.push((index_cursor, record_reg));
            continue;
        }
        let index_cursor = program.alloc_cursor_id(CursorType::BTreeIndex(index.clone()));
        program.emit_insn(Insn::OpenWrite {
            // An index always lives in the same database as its table.
            db: table_db_index,
            cursor_id: index_cursor,
            root_page: RegisterOrLiteral::Literal(index.root_page),
            name: index.name.clone(),
        });
        let record_reg = program.alloc_register();
        index_cursors.push((index_cursor, record_reg));
    }
    // Determine if statement-level savepoint is needed for UPDATE OR ABORT/ROLLBACK.
    use limbo_sqlite3_parser::ast::ResolveType;
    // A nested statement (an inlined trigger body) must not open the "_stmt"
    // savepoint: releasing a transaction-owning savepoint commits and halts the
    // enclosing program, dropping everything emitted after it.
    let needs_stmt_savepoint = !program.is_nested()
        && matches!(
            plan.or_conflict,
            None | Some(ResolveType::Abort) | Some(ResolveType::Rollback)
        );
    if needs_stmt_savepoint {
        use crate::vdbe::insn::SavepointOp;
        program.emit_insn(Insn::Savepoint {
            op: SavepointOp::Begin,
            name: "_stmt".to_string(),
        });
    }
    open_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &[JoinOrderMember::default()],
        &mut plan.where_clause,
        None,
        None,
    )?;
    emit_update_insns(&plan, &mut t_ctx, program, index_cursors)?;

    close_loop(
        program,
        &mut t_ctx,
        &plan.table_references,
        &[JoinOrderMember::default()],
    )?;

    program.preassign_label_to_next_insn(after_main_loop_label);

    after(program);

    if needs_stmt_savepoint {
        use crate::vdbe::insn::SavepointOp;
        program.emit_insn(Insn::Savepoint {
            op: SavepointOp::Release,
            name: "_stmt".to_string(),
        });
    }
    // Finalize program
    program.epilogue(TransactionMode::Write);
    program.result_columns = plan.returning.unwrap_or_default();
    program.table_references.extend(plan.table_references);
    Ok(())
}

#[instrument(skip_all, level = Level::TRACE)]
fn emit_update_insns(
    plan: &UpdatePlan,
    t_ctx: &mut TranslateCtx,
    program: &mut ProgramBuilder,
    index_cursors: Vec<(usize, usize)>,
) -> crate::Result<()> {
    let table_ref = plan.table_references.joined_tables().first().unwrap();
    let loop_labels = t_ctx.labels_main_loop.first().unwrap();
    let (cursor_id, index, is_virtual) = match &table_ref.op {
        Operation::Scan { index, .. } => (
            program.resolve_cursor_id(&CursorKey::table(table_ref.internal_id)),
            index.as_ref().map(|index| {
                (
                    index.clone(),
                    program
                        .resolve_cursor_id(&CursorKey::index(table_ref.internal_id, index.clone())),
                )
            }),
            table_ref.virtual_table().is_some(),
        ),
        Operation::Search(search) => match search {
            &Search::RowidEq { .. } | Search::Seek { index: None, .. } => (
                program.resolve_cursor_id(&CursorKey::table(table_ref.internal_id)),
                None,
                false,
            ),
            Search::Seek {
                index: Some(index), ..
            } => (
                program.resolve_cursor_id(&CursorKey::table(table_ref.internal_id)),
                Some((
                    index.clone(),
                    program
                        .resolve_cursor_id(&CursorKey::index(table_ref.internal_id, index.clone())),
                )),
                false,
            ),
        },
    };

    for cond in plan
        .where_clause
        .iter()
        .filter(|c| c.should_eval_before_loop(&[JoinOrderMember::default()]))
    {
        let jump_target = program.allocate_label();
        let meta = ConditionMetadata {
            jump_if_condition_is_true: false,
            jump_target_when_true: jump_target,
            jump_target_when_false: t_ctx.label_main_loop_end.ok_or_else(|| {
                LimboError::InternalError("label_main_loop_end not set".to_string())
            })?,
        };
        translate_condition_expr(
            program,
            &plan.table_references,
            &cond.expr,
            meta,
            &t_ctx.resolver,
        )?;
        program.preassign_label_to_next_insn(jump_target);
    }
    let beg = program.alloc_registers(
        table_ref.table.columns().len()
            + if is_virtual {
                2 // two args before the relevant columns for VUpdate
            } else {
                1 // rowid reg
            },
    );
    program.emit_insn(Insn::RowId {
        cursor_id,
        dest: beg,
    });

    // Check if rowid was provided (through INTEGER PRIMARY KEY as a rowid alias)

    let rowid_alias_index = {
        let rowid_alias_index = table_ref.columns().iter().position(|c| c.is_rowid_alias);
        if let Some(index) = rowid_alias_index {
            plan.set_clauses.iter().position(|(idx, _)| *idx == index)
        } else {
            None
        }
    };
    let rowid_set_clause_reg = if rowid_alias_index.is_some() {
        Some(program.alloc_register())
    } else {
        None
    };
    let has_user_provided_rowid = rowid_alias_index.is_some();

    let check_rowid_not_exists_label = if has_user_provided_rowid {
        Some(program.allocate_label())
    } else {
        None
    };

    if has_user_provided_rowid {
        program.emit_insn(Insn::NotExists {
            cursor: cursor_id,
            rowid_reg: beg,
            target_pc: check_rowid_not_exists_label.unwrap(),
        });
    } else {
        // if no rowid, we're done
        program.emit_insn(Insn::IsNull {
            reg: beg,
            target_pc: t_ctx.label_main_loop_end.ok_or_else(|| {
                LimboError::InternalError("label_main_loop_end not set".to_string())
            })?,
        });
    }

    if is_virtual {
        program.emit_insn(Insn::Copy {
            src_reg: beg,
            dst_reg: beg + 1,
            amount: 0,
        })
    }

    if let Some(offset) = t_ctx.reg_offset {
        program.emit_insn(Insn::IfPos {
            reg: offset,
            target_pc: loop_labels.next,
            decrement_by: 1,
        });
    }

    for cond in plan
        .where_clause
        .iter()
        .filter(|c| c.should_eval_before_loop(&[JoinOrderMember::default()]))
    {
        let jump_target = program.allocate_label();
        let meta = ConditionMetadata {
            jump_if_condition_is_true: false,
            jump_target_when_true: jump_target,
            jump_target_when_false: loop_labels.next,
        };
        translate_condition_expr(
            program,
            &plan.table_references,
            &cond.expr,
            meta,
            &t_ctx.resolver,
        )?;
        program.preassign_label_to_next_insn(jump_target);
    }

    // Row triggers: capture the OLD image while the cursor is still on the
    // pre-update row and before any SET expression has been evaluated.
    let trigger_table = table_ref.table.btree().filter(|btree| {
        crate::translate::trigger::table_has_triggers(&t_ctx.resolver, &btree.name)
    });
    let old_image = trigger_table.as_ref().map(|btree| {
        let cols_start_reg = program.alloc_registers(btree.columns.len().max(1));
        for idx in 0..btree.columns.len() {
            program.emit_column(cursor_id, idx, cols_start_reg + idx);
        }
        crate::translate::trigger::RowImage {
            rowid_reg: beg,
            cols_start_reg,
        }
    });

    // we scan a column at a time, loading either the column's values, or the new value
    // from the Set expression, into registers so we can emit a MakeRecord and update the row.
    let start = if is_virtual { beg + 2 } else { beg + 1 };
    for (idx, table_column) in table_ref.columns().iter().enumerate() {
        let target_reg = start + idx;
        if let Some((_, expr)) = plan.set_clauses.iter().find(|(i, _)| *i == idx) {
            if has_user_provided_rowid
                && (table_column.primary_key || table_column.is_rowid_alias)
                && !is_virtual
            {
                let rowid_set_clause_reg = rowid_set_clause_reg.unwrap();
                translate_expr(
                    program,
                    Some(&plan.table_references),
                    expr,
                    rowid_set_clause_reg,
                    &t_ctx.resolver,
                )?;

                program.emit_insn(Insn::MustBeInt {
                    reg: rowid_set_clause_reg,
                });

                program.emit_null(target_reg, None);
            } else {
                translate_expr(
                    program,
                    Some(&plan.table_references),
                    expr,
                    target_reg,
                    &t_ctx.resolver,
                )?;
                if table_column.notnull {
                    use crate::error::SQLITE_CONSTRAINT_NOTNULL;
                    program.emit_insn(Insn::HaltIfNull {
                        target_reg,
                        err_code: SQLITE_CONSTRAINT_NOTNULL,
                        description: format!(
                            "{}.{}",
                            table_ref.table.get_name(),
                            table_column
                                .name
                                .as_ref()
                                .expect("Column name must be present")
                        ),
                    });
                }
            }
        } else {
            let column_idx_in_index = index.as_ref().and_then(|(idx, _)| {
                idx.columns
                    .iter()
                    .position(|c| Some(&c.name) == table_column.name.as_ref())
            });

            // don't emit null for pkey of virtual tables. they require first two args
            // before the 'record' to be explicitly non-null
            if table_column.is_rowid_alias && !is_virtual {
                program.emit_null(target_reg, None);
            } else if is_virtual {
                program.emit_insn(Insn::VColumn {
                    cursor_id,
                    column: idx,
                    dest: target_reg,
                });
            } else {
                let cursor_id = *index
                    .as_ref()
                    .and_then(|(_, id)| {
                        if column_idx_in_index.is_some() {
                            Some(id)
                        } else {
                            None
                        }
                    })
                    .unwrap_or(&cursor_id);
                program.emit_column(cursor_id, column_idx_in_index.unwrap_or(idx), target_reg);
            }
        }
    }

    // BEFORE UPDATE triggers run once both row images exist and before any
    // index or table write.
    if let (Some(btree), Some(old_image)) = (trigger_table.as_ref(), old_image) {
        let new_image = crate::translate::trigger::RowImage {
            rowid_reg: rowid_set_clause_reg.unwrap_or(beg),
            cols_start_reg: start,
        };
        let changed_columns = changed_column_names(btree.as_ref(), plan);
        crate::translate::trigger::emit_triggers(
            program,
            &t_ctx.resolver,
            &crate::translate::trigger::TriggerFireArgs {
                table: btree.as_ref(),
                time: limbo_sqlite3_parser::ast::TriggerTime::Before,
                event: crate::translate::trigger::TriggerEventKind::Update(&changed_columns),
                old: Some(old_image),
                new: Some(new_image),
                ignore_jump: loop_labels.next,
            },
        )?;
        // A BEFORE trigger body may have written to this very table, moving the
        // cursor (or deleting the row outright). Re-seek to the row being
        // updated; if it is gone, skip it — this is what upstream's
        // `OP_NotExists` after `sqlite3CodeRowTrigger` does.
        program.emit_insn(Insn::NotExists {
            cursor: cursor_id,
            rowid_reg: beg,
            target_pc: loop_labels.next,
        });
    }

    for (index, (idx_cursor_id, record_reg)) in plan.indexes_to_update.iter().zip(&index_cursors) {
        let num_cols = index.columns.len();
        // allocate scratch registers for the index columns plus rowid
        let idx_start_reg = program.alloc_registers(num_cols + 1);

        let rowid_reg = beg;
        let idx_cols_start_reg = beg + 1;

        // copy each index column from the table's column registers into these scratch regs
        for (i, col) in index.columns.iter().enumerate() {
            // copy from the table's column register over to the index's scratch register

            program.emit_insn(Insn::Copy {
                src_reg: idx_cols_start_reg + col.pos_in_table,
                dst_reg: idx_start_reg + i,
                amount: 0,
            });
        }
        // last register is the rowid
        program.emit_insn(Insn::Copy {
            src_reg: rowid_reg,
            dst_reg: idx_start_reg + num_cols,
            amount: 0,
        });

        // this record will be inserted into the index later
        program.emit_insn(Insn::MakeRecord {
            start_reg: idx_start_reg,
            count: num_cols + 1,
            dest_reg: *record_reg,
            index_name: Some(index.name.clone()),
        });

        if !index.unique {
            continue;
        }

        // check if the record already exists in the index for unique indexes and abort if so
        let constraint_check = program.allocate_label();
        program.emit_insn(Insn::NoConflict {
            cursor_id: *idx_cursor_id,
            target_pc: constraint_check,
            record_reg: idx_start_reg,
            num_regs: num_cols,
        });

        let column_names = index.columns.iter().enumerate().fold(
            String::with_capacity(50),
            |mut accum, (idx, col)| {
                if idx > 0 {
                    accum.push_str(", ");
                }
                accum.push_str(&table_ref.table.get_name());
                accum.push('.');
                accum.push_str(&col.name);

                accum
            },
        );

        let idx_rowid_reg = program.alloc_register();
        program.emit_insn(Insn::IdxRowId {
            cursor_id: *idx_cursor_id,
            dest: idx_rowid_reg,
        });

        program.emit_insn(Insn::Eq {
            lhs: rowid_reg,
            rhs: idx_rowid_reg,
            target_pc: constraint_check,
            flags: CmpInsFlags::default(), // TODO: not sure what type of comparison flag is needed
            collation: program.curr_collation(),
        });

        emit_conflict_halt(
            program,
            &plan.or_conflict,
            index.on_conflict,
            SQLITE_CONSTRAINT_PRIMARYKEY, // TODO: distinct between primary key and unique index
            column_names,
        );

        program.preassign_label_to_next_insn(constraint_check);
    }

    if let Some(btree_table) = table_ref.btree() {
        if btree_table.is_strict {
            program.emit_insn(Insn::TypeCheck {
                start_reg: start,
                count: table_ref.columns().len(),
                check_generated: true,
                table_reference: Rc::clone(&btree_table),
            });
        }

        if has_user_provided_rowid {
            let record_label = program.allocate_label();
            let idx = rowid_alias_index.unwrap();
            let target_reg = rowid_set_clause_reg.unwrap();
            program.emit_insn(Insn::Eq {
                lhs: target_reg,
                rhs: beg,
                target_pc: record_label,
                flags: CmpInsFlags::default(),
                collation: program.curr_collation(),
            });

            program.emit_insn(Insn::NotExists {
                cursor: cursor_id,
                rowid_reg: target_reg,
                target_pc: record_label,
            });

            emit_conflict_halt(
                program,
                &plan.or_conflict,
                btree_table.primary_key_conflict,
                SQLITE_CONSTRAINT_PRIMARYKEY,
                format!(
                    "{}.{}",
                    table_ref.table.get_name(),
                    &table_ref
                        .columns()
                        .get(idx)
                        .unwrap()
                        .name
                        .as_ref()
                        .map_or("", |v| v)
                ),
            );

            program.preassign_label_to_next_insn(record_label);
        }

        let record_reg = program.alloc_register();
        program.emit_insn(Insn::MakeRecord {
            start_reg: start,
            count: table_ref.columns().len(),
            dest_reg: record_reg,
            index_name: None,
        });

        if has_user_provided_rowid {
            program.emit_insn(Insn::NotExists {
                cursor: cursor_id,
                rowid_reg: beg,
                target_pc: check_rowid_not_exists_label.unwrap(),
            });
        }

        // For each index -> insert
        for (index, (idx_cursor_id, record_reg)) in plan.indexes_to_update.iter().zip(index_cursors)
        {
            let num_regs = index.columns.len() + 1;
            let start_reg = program.alloc_registers(num_regs);

            // Delete existing index key
            index
                .columns
                .iter()
                .enumerate()
                .for_each(|(reg_offset, column_index)| {
                    program.emit_column(
                        cursor_id,
                        column_index.pos_in_table,
                        start_reg + reg_offset,
                    );
                });

            program.emit_insn(Insn::RowId {
                cursor_id,
                dest: start_reg + num_regs - 1,
            });

            program.emit_insn(Insn::IdxDelete {
                start_reg,
                num_regs,
                cursor_id: idx_cursor_id,
            });

            // Insert new index key (filled further above with values from set_clauses)
            program.emit_insn(Insn::IdxInsert {
                cursor_id: idx_cursor_id,
                record_reg: record_reg,
                unpacked_start: Some(start),
                unpacked_count: Some((index.columns.len() + 1) as u16),
                flags: IdxInsertFlags::new(),
            });
        }

        program.emit_insn(Insn::Delete { cursor_id });

        program.emit_insn(Insn::Insert {
            cursor: cursor_id,
            key_reg: rowid_set_clause_reg.unwrap_or(beg),
            record_reg,
            flag: InsertFlags::new().update(true),
            table_name: table_ref.identifier.clone(),
        });

        if let (Some(btree), Some(old_image)) = (trigger_table.as_ref(), old_image) {
            let new_image = crate::translate::trigger::RowImage {
                rowid_reg: rowid_set_clause_reg.unwrap_or(beg),
                cols_start_reg: start,
            };
            let changed_columns = changed_column_names(btree.as_ref(), plan);
            crate::translate::trigger::emit_triggers(
                program,
                &t_ctx.resolver,
                &crate::translate::trigger::TriggerFireArgs {
                    table: btree.as_ref(),
                    time: limbo_sqlite3_parser::ast::TriggerTime::After,
                    event: crate::translate::trigger::TriggerEventKind::Update(&changed_columns),
                    old: Some(old_image),
                    new: Some(new_image),
                    ignore_jump: loop_labels.next,
                },
            )?;
        }
    } else if let Some(_) = table_ref.virtual_table() {
        let arg_count = table_ref.columns().len() + 2;
        program.emit_insn(Insn::VUpdate {
            cursor_id,
            arg_count,
            start_reg: beg,
            conflict_action: 0u16,
        });
    }

    // RETURNING: emit one result row per updated row, evaluating the RETURNING
    // expressions against the row that was just written by the `Insert`/`VUpdate`
    // above.
    //
    // Naively re-reading the new values via `Column`/`RowId` against `cursor_id`
    // at this point does *not* work: empirically (see the regression tests in
    // tests/update_returning.rs) a cursor's cached record is not guaranteed to
    // reflect a `Insert` that was just performed through the same cursor, so a
    // post-Insert `Column` read can observe the *old* pre-update value. This is
    // exactly the hazard `upsert::emit_upsert_do_update` already works around
    // for `excluded.*`/old-value references in `DO UPDATE` — instead of reading
    // back through the cursor, it redirects column-expression lookups to the
    // registers that were used to build the new record. We do the same here:
    // every new column value (and the new rowid) is already sitting in a fixed
    // register from the "scan a column at a time" loop / rowid computation
    // above, so we temporarily register those registers as resolver overrides
    // and let `translate_expr` (via `Resolver::resolve_cached_expr_reg`) pick
    // them up for any `Expr::Column`/`Expr::RowId` leaf in the RETURNING
    // expressions — the same mechanism `result_row::emit_select_result` feeds
    // into for ordinary SELECT result columns, just with these overrides ahead
    // of it in the lookup order.
    //
    // This must be emitted *before* the LIMIT decrement below: the row that
    // causes the LIMIT counter to hit zero has still been updated and therefore
    // must still produce a RETURNING row, but `DecrJumpZero` jumps straight to
    // `label_main_loop_end` — the end of the whole UPDATE loop, since UPDATE has
    // no post-loop processing — so anything emitted after it would never run for
    // that final row (exactly the ordering `emit_result_row_and_limit` uses for
    // SELECT). Rows skipped entirely (via OFFSET, the WHERE filter, or the
    // `check_rowid_not_exists_label` jumps below, all of which land after this
    // point) never reach this code, so they correctly produce no RETURNING row.
    if let Some(returning) = plan.returning.as_ref().filter(|r| !r.is_empty()) {
        // The new rowid lives in `rowid_set_clause_reg` only when the rowid-alias
        // column was itself SET *and* the table is a real btree table (virtual
        // tables never populate that register — see the per-column loop above).
        // Otherwise the rowid is unchanged, so the pre-update `beg` is still
        // correct (and, for virtual tables, is the best information available
        // since `VUpdate` is opaque to this emitter).
        let new_rowid_reg = if has_user_provided_rowid && !is_virtual {
            rowid_set_clause_reg.unwrap_or(beg)
        } else {
            beg
        };

        t_ctx.resolver.upsert_reg_overrides.clear();
        for (idx, table_column) in table_ref.columns().iter().enumerate() {
            let new_value_reg = if table_column.is_rowid_alias && !is_virtual {
                new_rowid_reg
            } else {
                start + idx
            };
            t_ctx.resolver.upsert_reg_overrides.push((
                ast::Expr::Column {
                    database: None,
                    table: table_ref.internal_id,
                    column: idx,
                    is_rowid_alias: table_column.is_rowid_alias,
                },
                new_value_reg,
            ));
        }
        t_ctx.resolver.upsert_reg_overrides.push((
            ast::Expr::RowId {
                database: None,
                table: table_ref.internal_id,
            },
            new_rowid_reg,
        ));

        let reg_result_cols_start = program.alloc_registers(returning.len());
        for (i, rc) in returning.iter().enumerate() {
            translate_expr(
                program,
                Some(&plan.table_references),
                &rc.expr,
                reg_result_cols_start + i,
                &t_ctx.resolver,
            )?;
        }
        t_ctx.resolver.upsert_reg_overrides.clear();

        program.emit_insn(Insn::ResultRow {
            start_reg: reg_result_cols_start,
            count: returning.len(),
        });
    }

    if let Some(limit_ctx) = t_ctx.limit_ctx {
        program.emit_insn(Insn::DecrJumpZero {
            reg: limit_ctx.reg_limit,
            target_pc: t_ctx.label_main_loop_end.ok_or_else(|| {
                LimboError::InternalError("label_main_loop_end not set".to_string())
            })?,
        })
    }

    if let Some(label) = check_rowid_not_exists_label {
        program.preassign_label_to_next_insn(label);
    }

    Ok(())
}

/// Emit the appropriate conflict-halt instructions for UPDATE conflict resolution.
///
/// `stmt_conflict` is the statement-level `UPDATE OR <action>` clause, if any.
/// `constraint_default` is the specific UNIQUE/PRIMARY KEY constraint's own
/// `ON CONFLICT <action>` (already defaulted to [`ResolveType::Abort`] by the
/// schema layer when the constraint didn't declare one — see
/// `BTreeTable::primary_key_conflict` / `Index::on_conflict`). Per SQLite's
/// precedence rules, an explicit statement-level clause wins if present;
/// otherwise the constraint's own resolution applies.
///
/// For an effective action of ABORT or ROLLBACK, this emits a
/// `Savepoint RollbackTo "_stmt"` before `Halt` so that partial writes by the
/// current statement are undone. For FAIL, it emits only `Halt` (prior writes
/// are kept).
///
/// NOTE: unlike `INSERT`, this engine does not yet implement true row-skipping
/// (IGNORE) or victim-replacement (REPLACE) semantics for `UPDATE` conflicts —
/// that would require the same kind of dedicated machinery `translate::insert`
/// has for locating and deleting/skipping a specific victim row, which does
/// not exist for the generic UPDATE main-loop today. An effective IGNORE/
/// REPLACE therefore still falls back to `Halt` (matching this engine's
/// pre-existing behavior for a statement-level `UPDATE OR IGNORE`/
/// `UPDATE OR REPLACE`, which has the same limitation) rather than silently
/// mismatching the requested semantics in a different way.
fn emit_conflict_halt(
    program: &mut ProgramBuilder,
    stmt_conflict: &Option<limbo_sqlite3_parser::ast::ResolveType>,
    constraint_default: limbo_sqlite3_parser::ast::ResolveType,
    err_code: usize,
    description: String,
) {
    use crate::vdbe::insn::SavepointOp;
    use limbo_sqlite3_parser::ast::ResolveType;
    let effective = stmt_conflict.unwrap_or(constraint_default);
    match effective {
        // A nested statement never opened the "_stmt" savepoint (see
        // `ProgramBuilder::is_nested`), so rolling back to it would fail with
        // "no such savepoint" and mask the real constraint error. The enclosing
        // `Halt` still discards the uncommitted page cache.
        ResolveType::Abort | ResolveType::Rollback if !program.is_nested() => {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::RollbackTo,
                name: "_stmt".to_string(),
            });
            program.emit_insn(Insn::Halt {
                err_code,
                description,
            });
        }
        // FAIL, and (for now, see doc comment above) IGNORE/REPLACE: halt
        // without rolling back prior writes from this statement. Also the
        // nested ABORT/ROLLBACK case guarded above.
        ResolveType::Abort
        | ResolveType::Rollback
        | ResolveType::Fail
        | ResolveType::Ignore
        | ResolveType::Replace => {
            program.emit_insn(Insn::Halt {
                err_code,
                description,
            });
        }
    }
}

/// Initialize the limit/offset counters and registers.
/// In case of compound SELECTs, the limit counter is initialized only once,
/// hence [LimitCtx::initialize_counter] being false in those cases.
///
/// `limit_expr` / `offset_expr` support runtime-bound parameters (?, $1, :name).
/// When present they take precedence over the literal `limit` / `offset` values.
/// The `t_ctx.label_main_loop_end` label **must** be allocated before calling this
/// function so that a LIMIT=0 parameter can emit an immediate jump.
fn init_limit(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    limit: Option<isize>,
    offset: Option<isize>,
    limit_expr: Option<&ast::Expr>,
    offset_expr: Option<&ast::Expr>,
) -> Result<()> {
    // ── Set up LIMIT ─────────────────────────────────────────────────────────
    if let Some(expr) = limit_expr {
        // Parameterized limit: allocate a register, translate the expression.
        if t_ctx.limit_ctx.is_none() {
            t_ctx.limit_ctx = Some(LimitCtx::new(program));
        }
        let limit_reg = t_ctx
            .limit_ctx
            .expect("limit_ctx just initialised")
            .reg_limit;

        {
            let resolver = &t_ctx.resolver;
            translate_expr(program, None, expr, limit_reg, resolver)?;
        }

        // NULL → -1 (unlimited): DecrJumpZero never reaches 0 for -1.
        let not_null_label = program.allocate_label();
        program.emit_insn(Insn::NotNull {
            reg: limit_reg,
            target_pc: not_null_label,
        });
        program.emit_int(-1, limit_reg);
        program.preassign_label_to_next_insn(not_null_label);

        // If the parameter is exactly 0, jump past all post-processing (ORDER BY, GROUP BY, etc.)
        if let Some(abort_label) = t_ctx.label_abort {
            program.emit_insn(Insn::IfNot {
                reg: limit_reg,
                target_pc: abort_label,
                jump_if_null: false,
            });
        }
    } else {
        // Literal limit (original behaviour).
        if t_ctx.limit_ctx.is_none() {
            t_ctx.limit_ctx = limit.map(|_| LimitCtx::new(program));
        }
        let Some(limit_ctx) = t_ctx.limit_ctx else {
            // No limit at all — still handle offset below.
            return init_limit_offset_only(program, t_ctx, offset, offset_expr);
        };
        if limit_ctx.initialize_counter {
            program.emit_insn(Insn::Integer {
                value: limit
                    .expect("limit must be Some when limit_ctx is initialised with a literal")
                    as i64,
                dest: limit_ctx.reg_limit,
            });
        }
    }

    // ── Set up OFFSET ─────────────────────────────────────────────────────────
    if let Some(expr) = offset_expr {
        // Parameterized offset.
        if t_ctx.reg_offset.is_none() {
            let reg = program.alloc_register();
            t_ctx.reg_offset = Some(reg);
            {
                let resolver = &t_ctx.resolver;
                translate_expr(program, None, expr, reg, resolver)?;
            }
            // NULL → 0 (no skip).
            let not_null_label = program.allocate_label();
            program.emit_insn(Insn::NotNull {
                reg,
                target_pc: not_null_label,
            });
            program.emit_int(0, reg);
            program.preassign_label_to_next_insn(not_null_label);

            if let Some(limit_ctx) = t_ctx.limit_ctx {
                let combined_reg = program.alloc_register();
                t_ctx.reg_limit_offset_sum = Some(combined_reg);
                program.emit_insn(Insn::OffsetLimit {
                    limit_reg: limit_ctx.reg_limit,
                    offset_reg: reg,
                    combined_reg,
                });
            }
        }
    } else if t_ctx.reg_offset.is_none() && offset.is_some_and(|n| n != 0) {
        // Literal offset (original behaviour).
        let reg = program.alloc_register();
        t_ctx.reg_offset = Some(reg);
        program.emit_insn(Insn::Integer {
            value: offset.expect("offset is Some here") as i64,
            dest: reg,
        });
        let combined_reg = program.alloc_register();
        t_ctx.reg_limit_offset_sum = Some(combined_reg);
        program.emit_insn(Insn::OffsetLimit {
            limit_reg: t_ctx
                .limit_ctx
                .expect("limit_ctx must be Some when literal offset is active")
                .reg_limit,
            offset_reg: reg,
            combined_reg,
        });
    }

    Ok(())
}

/// Handle the offset-only path when there is no limit at all.
/// This is an uncommon case (OFFSET without LIMIT is unusual in SQLite),
/// but we handle it gracefully.
#[inline]
fn init_limit_offset_only(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    offset: Option<isize>,
    offset_expr: Option<&ast::Expr>,
) -> Result<()> {
    // Without a limit register there is nowhere to store limit+offset for OffsetLimit.
    // We still set up reg_offset so that emit_offset can emit IfPos skips.
    if let Some(expr) = offset_expr {
        if t_ctx.reg_offset.is_none() {
            let reg = program.alloc_register();
            t_ctx.reg_offset = Some(reg);
            let resolver = &t_ctx.resolver;
            translate_expr(program, None, expr, reg, resolver)?;
            // NULL → 0
            let not_null_label = program.allocate_label();
            program.emit_insn(Insn::NotNull {
                reg,
                target_pc: not_null_label,
            });
            program.emit_int(0, reg);
            program.preassign_label_to_next_insn(not_null_label);
        }
    } else if t_ctx.reg_offset.is_none() && offset.is_some_and(|n| n != 0) {
        let reg = program.alloc_register();
        t_ctx.reg_offset = Some(reg);
        program.emit_insn(Insn::Integer {
            value: offset.expect("offset is Some here") as i64,
            dest: reg,
        });
    }
    Ok(())
}
