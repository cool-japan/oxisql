//! Translation of subqueries.
//!
//! Two flavours are handled here:
//! - **FROM-clause subqueries** ([`emit_subqueries`]/[`emit_subquery`]): wrapped in coroutines so
//!   the parent query can read their rows as if scanning a table.
//! - **Expression-position subqueries** ([`plan_expr_subquery`] +
//!   [`emit_scalar_or_exists_subquery`]/[`emit_in_subquery`]): scalar `(SELECT ...)`,
//!   `EXISTS (...)`, and `... IN (SELECT ...)`. These are planned at translation time with the
//!   outer query's tables exposed as [`OuterQueryReference`]s, so correlated column references bind
//!   against the outer scope and read the already-positioned outer cursors. Correlation is detected
//!   after planning (any outer reference marked "used"); correlated subqueries are re-evaluated for
//!   every outer row, uncorrelated ones are guarded by `Once` and evaluated once.
//!
//! NULL note for `IN (SELECT ...)`: `NULL IN/NOT IN (...)` yields NULL, and the
//! found/not-found result is exact. When `x NOT IN/IN (set)` has no match but the set contains a
//! NULL, the result is NULL per SQLite three-valued logic — implemented via a Rewind+Column+IsNull
//! check after materializing the set (NULLs sort first in the ASC B-tree).

use std::sync::Arc;

use limbo_sqlite3_parser::ast::{self, SortOrder};

use crate::{
    schema::{Index, IndexColumn, Table},
    translate::optimizer::optimize_plan,
    vdbe::{
        builder::{CursorType, ProgramBuilder},
        insn::Insn,
        BranchOffset,
    },
    Result,
};

use super::{
    emitter::{emit_query, Resolver, TranslateCtx},
    main_loop::LoopLabels,
    plan::{
        ColumnUsedMask, OuterQueryReference, Plan, QueryDestination, SelectPlan, TableReferences,
    },
    select::prepare_select_plan,
};

/// Emit the subqueries contained in the FROM clause.
/// This is done first so the results can be read in the main query loop.
pub fn emit_subqueries(
    program: &mut ProgramBuilder,
    t_ctx: &mut TranslateCtx,
    tables: &mut TableReferences,
) -> Result<()> {
    for table_reference in tables.joined_tables_mut() {
        if let Table::FromClauseSubquery(from_clause_subquery) = &mut table_reference.table {
            // Emit the subquery and get the start register of the result columns.
            let result_columns_start =
                emit_subquery(program, &mut from_clause_subquery.plan, t_ctx)?;
            // Set the start register of the subquery's result columns.
            // This is done so that translate_expr() can read the result columns of the subquery,
            // as if it were reading from a regular table.
            from_clause_subquery.result_columns_start_reg = Some(result_columns_start);
        }
    }
    Ok(())
}

/// Emit a subquery and return the start register of the result columns.
/// This is done by emitting a coroutine that stores the result columns in sequential registers.
/// Each subquery in a FROM clause has its own separate SelectPlan which is wrapped in a coroutine.
///
/// The resulting bytecode from a subquery is mostly exactly the same as a regular query, except:
/// - it ends in an EndCoroutine instead of a Halt.
/// - instead of emitting ResultRows, the coroutine yields to the main query loop.
/// - the first register of the result columns is returned to the parent query,
///   so that translate_expr() can read the result columns of the subquery,
///   as if it were reading from a regular table.
///
/// Since a subquery has its own SelectPlan, it can contain nested subqueries,
/// which can contain even more nested subqueries, etc.
pub fn emit_subquery<'a>(
    program: &mut ProgramBuilder,
    plan: &mut SelectPlan,
    t_ctx: &mut TranslateCtx<'a>,
) -> Result<usize> {
    let yield_reg = program.alloc_register();
    let coroutine_implementation_start_offset = program.allocate_label();
    match &mut plan.query_destination {
        QueryDestination::CoroutineYield {
            yield_reg: y,
            coroutine_implementation_start,
        } => {
            // The parent query will use this register to jump to/from the subquery.
            *y = yield_reg;
            // The parent query will use this register to reinitialize the coroutine when it needs to run multiple times.
            *coroutine_implementation_start = coroutine_implementation_start_offset;
        }
        _ => unreachable!("emit_subquery called on non-subquery"),
    }
    let end_coroutine_label = program.allocate_label();
    let mut metadata = TranslateCtx {
        labels_main_loop: (0..plan.joined_tables().len())
            .map(|_| LoopLabels::new(program))
            .collect(),
        label_main_loop_end: None,
        label_abort: None,
        meta_group_by: None,
        meta_left_joins: (0..plan.joined_tables().len()).map(|_| None).collect(),
        meta_sort: None,
        reg_agg_start: None,
        reg_nonagg_emit_once_flag: None,
        reg_result_cols_start: None,
        result_column_indexes_in_orderby_sorter: (0..plan.result_columns.len()).collect(),
        result_columns_to_skip_in_orderby_sorter: None,
        limit_ctx: None,
        reg_offset: None,
        reg_limit_offset_sum: None,
        resolver: Resolver::new(t_ctx.resolver.schema, t_ctx.resolver.symbol_table),
    };
    let subquery_body_end_label = program.allocate_label();
    program.emit_insn(Insn::InitCoroutine {
        yield_reg,
        jump_on_definition: subquery_body_end_label,
        start_offset: coroutine_implementation_start_offset,
    });
    program.preassign_label_to_next_insn(coroutine_implementation_start_offset);
    let result_column_start_reg = emit_query(program, plan, &mut metadata)?;
    program.resolve_label(end_coroutine_label, program.offset());
    program.emit_insn(Insn::EndCoroutine { yield_reg });
    program.preassign_label_to_next_insn(subquery_body_end_label);
    Ok(result_column_start_reg)
}

/// The kind of subquery appearing in expression position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprSubqueryKind {
    /// `(SELECT ...)` used as a scalar value. Reads the first column of the first row,
    /// or NULL if the subquery returns no rows.
    Scalar,
    /// `EXISTS (SELECT ...)`. Evaluates to 1 if the subquery returns at least one row,
    /// otherwise 0.
    Exists,
}

/// A planned expression-position subquery, ready to be emitted.
///
/// The subquery's [`SelectPlan`] is planned with the outer query's tables exposed as
/// [`OuterQueryReference`]s. Correlation is detected after planning by checking whether any
/// outer reference had a column bound to it (i.e. is "used").
pub struct PlannedExprSubquery {
    pub plan: SelectPlan,
    /// Whether the subquery references any column from the outer scope.
    /// Correlated subqueries must be re-evaluated for every outer row, so they are emitted
    /// at the expression-evaluation site without a `Once` guard.
    pub is_correlated: bool,
}

/// Build the [`OuterQueryReference`]s visible to a subquery, given the outer query's table
/// references. The outer query's joined tables become outer references for the subquery, and any
/// outer references already in scope for the outer query are forwarded so that deeply nested
/// correlated subqueries can reach grandparent scopes.
fn outer_query_refs_for_subquery(outer: &TableReferences) -> Vec<OuterQueryReference> {
    let mut refs = Vec::with_capacity(outer.joined_tables().len() + outer.outer_query_refs().len());
    for joined in outer.joined_tables() {
        refs.push(OuterQueryReference {
            identifier: joined.identifier.clone(),
            internal_id: joined.internal_id,
            table: joined.table.clone(),
            col_used_mask: ColumnUsedMask::new(),
        });
    }
    // Forward grandparent scopes (already-correlated outer references).
    for outer_ref in outer.outer_query_refs() {
        refs.push(OuterQueryReference {
            identifier: outer_ref.identifier.clone(),
            internal_id: outer_ref.internal_id,
            table: outer_ref.table.clone(),
            col_used_mask: ColumnUsedMask::new(),
        });
    }
    refs
}

/// Plan a subquery that appears in expression position (scalar, EXISTS or IN).
///
/// The subquery is planned with the outer query's tables exposed as outer references so that
/// correlated column references bind correctly, and optimized like any other SELECT. The plan's
/// destination is set to [`QueryDestination::CoroutineYield`] so it can be driven row-by-row by
/// the caller via `InitCoroutine`/`Yield`.
pub fn plan_expr_subquery(
    program: &mut ProgramBuilder,
    resolver: &Resolver,
    outer: &TableReferences,
    select: &ast::Select,
) -> Result<PlannedExprSubquery> {
    let outer_refs = outer_query_refs_for_subquery(outer);
    let mut plan = match prepare_select_plan(
        resolver.schema,
        select.clone(),
        resolver.symbol_table,
        &outer_refs,
        &mut program.table_reference_counter,
        QueryDestination::CoroutineYield {
            yield_reg: usize::MAX, // set during emission by emit_subquery
            coroutine_implementation_start: BranchOffset::Placeholder,
        },
    )? {
        Plan::Select(plan) => plan,
        Plan::CompoundSelect { .. } => {
            crate::bail_parse_error!("compound SELECT not supported in subquery expression yet")
        }
        _ => crate::bail_parse_error!("unsupported subquery in expression position"),
    };

    let mut full_plan = Plan::Select(plan);
    optimize_plan(&mut full_plan, resolver.schema)?;
    plan = match full_plan {
        Plan::Select(plan) => plan,
        _ => unreachable!("optimize_plan must not change the plan variant"),
    };

    // A subquery is correlated iff it references at least one column from the outer scope.
    let is_correlated = plan
        .table_references
        .outer_query_refs()
        .iter()
        .any(|r| r.is_used());

    Ok(PlannedExprSubquery {
        plan,
        is_correlated,
    })
}

/// Emit a scalar or EXISTS subquery, writing the result into `target_register`.
///
/// Mechanism: the subquery body is emitted as a coroutine (see [`emit_subquery`]). The caller
/// drives it with a single `Yield`:
/// - Scalar: `target_register` is pre-set to NULL; if a row is yielded, the first result column is
///   copied into `target_register`; if the coroutine ends with no rows, `target_register` stays
///   NULL. SQLite scalar-subquery semantics take only the first row.
/// - EXISTS: `target_register` is pre-set to 0; if a row is yielded it becomes 1; otherwise it
///   stays 0. EXISTS short-circuits after the first row.
///
/// Uncorrelated subqueries do not depend on the outer row, so the whole evaluation is wrapped in a
/// `Once` guard and the cached register is reused on subsequent outer rows. Correlated subqueries
/// must be recomputed for every outer row, so no `Once` guard is emitted.
pub fn emit_scalar_or_exists_subquery(
    program: &mut ProgramBuilder,
    resolver: &Resolver,
    planned: &mut PlannedExprSubquery,
    kind: ExprSubqueryKind,
    target_register: usize,
) -> Result<()> {
    let label_done = program.allocate_label();

    // Uncorrelated subqueries are evaluated at most once; reuse the result on re-entry.
    let label_after_once = if planned.is_correlated {
        None
    } else {
        let label = program.allocate_label();
        program.emit_insn(Insn::Once {
            target_pc_when_reentered: label,
        });
        Some(label)
    };

    let mut t_ctx = TranslateCtx::new(
        program,
        resolver.schema,
        resolver.symbol_table,
        planned.plan.table_references.joined_tables().len(),
        planned.plan.result_columns.len(),
    );

    let result_cols_start = emit_subquery(program, &mut planned.plan, &mut t_ctx)?;

    let yield_reg = match &planned.plan.query_destination {
        QueryDestination::CoroutineYield { yield_reg, .. } => *yield_reg,
        _ => unreachable!("expression subquery must use CoroutineYield destination"),
    };

    // Default value (taken when the subquery produces no row).
    match kind {
        ExprSubqueryKind::Scalar => program.emit_insn(Insn::Null {
            dest: target_register,
            dest_end: None,
        }),
        ExprSubqueryKind::Exists => program.emit_int(0, target_register),
    }

    // Drive the coroutine once. If it has already ended (no rows), jump past the row-handling.
    program.emit_insn(Insn::Yield {
        yield_reg,
        end_offset: label_done,
    });

    // A row was produced.
    match kind {
        ExprSubqueryKind::Scalar => program.emit_insn(Insn::Copy {
            src_reg: result_cols_start,
            dst_reg: target_register,
            amount: 0,
        }),
        ExprSubqueryKind::Exists => program.emit_int(1, target_register),
    }

    program.preassign_label_to_next_insn(label_done);
    if let Some(label_after_once) = label_after_once {
        program.preassign_label_to_next_insn(label_after_once);
    }
    Ok(())
}

/// Emit an `IN (SELECT ...)` / `NOT IN (SELECT ...)` test.
///
/// The left-hand-side value is provided in `lhs_register`. The subquery rows are materialized into
/// an ephemeral one-column index (for an uncorrelated subquery this is built once, guarded by
/// `Once`; for a correlated subquery it is rebuilt for every outer row). The result is written into
/// `target_register` as 1 (true), 0 (false), or NULL.
///
/// NULL handling follows SQLite's three-valued logic:
/// - If the LHS is NULL, the result is NULL.
/// - `x IN (...)`: 1 if a matching row exists; NULL if no match but the set contains NULL; else 0.
/// - `x NOT IN (...)`: 0 if a matching row exists; NULL if no match but the set contains NULL; else 1.
///
/// NULLs sort first (LESS THAN every other value) in the ASC-ordered ephemeral B-tree, so a
/// single `Rewind` + `Column` + `IsNull` check after building the set is sufficient to detect
/// whether the set contains any NULL.
#[allow(clippy::too_many_arguments)]
pub fn emit_in_subquery(
    program: &mut ProgramBuilder,
    resolver: &Resolver,
    planned: &mut PlannedExprSubquery,
    lhs_register: usize,
    target_register: usize,
    negated: bool,
) -> Result<()> {
    if planned.plan.result_columns.len() != 1 {
        crate::bail_parse_error!("subquery on the right-hand side of IN must return one column");
    }

    // Ephemeral index that will hold the subquery's result values.
    let index = Arc::new(Index {
        name: format!("in_subquery_{}", program.offset().to_offset_int()),
        table_name: String::new(),
        ephemeral: true,
        root_page: 0,
        columns: vec![IndexColumn {
            name: "value".to_string(),
            order: SortOrder::Asc,
            pos_in_table: 0,
            collation: None,
            default: None,
        }],
        unique: false,
        has_rowid: false,
    });
    let index_cursor_id = program.alloc_cursor_id(CursorType::BTreeIndex(index.clone()));

    // reg_null_in_set: 0 means the set contains no NULL; 1 means it contains at least one.
    // reg_temp: scratch register used to read the first B-tree row during the NULL check.
    let reg_null_in_set = program.alloc_register();
    let reg_temp = program.alloc_register();

    // Build the materialized set. Uncorrelated sets are built once.
    //
    // For uncorrelated: the Once guard jumps to label_after_build on every re-entry, skipping
    // the build entirely. reg_null_in_set is initialised inside the guard and retains its value
    // across re-entries, so the NULL check also runs only once.
    //
    // For correlated: no Once guard; the set is rebuilt (and reg_null_in_set re-initialised) for
    // every outer row.
    let label_after_build = program.allocate_label();
    if !planned.is_correlated {
        program.emit_insn(Insn::Once {
            target_pc_when_reentered: label_after_build,
        });
    }

    // Initialise the null flag before building the set (inside Once for uncorrelated).
    program.emit_int(0, reg_null_in_set);

    program.emit_insn(Insn::OpenEphemeral {
        cursor_id: index_cursor_id,
        is_table: false,
    });

    // Point the subquery at the ephemeral index so each row is inserted into it.
    planned.plan.query_destination = QueryDestination::EphemeralIndex {
        cursor_id: index_cursor_id,
        index: index.clone(),
    };

    let mut t_ctx = TranslateCtx::new(
        program,
        resolver.schema,
        resolver.symbol_table,
        planned.plan.table_references.joined_tables().len(),
        planned.plan.result_columns.len(),
    );
    emit_query(program, &mut planned.plan, &mut t_ctx)?;

    // After building the set, scan the first row to detect a NULL value.
    // Because NULLs collate less than every other value in the ASC index, a NULL in the set
    // will always be the first entry after Rewind.
    let label_set_null_flag = program.allocate_label();
    program.emit_insn(Insn::Rewind {
        cursor_id: index_cursor_id,
        pc_if_empty: label_after_build,
    });
    program.emit_insn(Insn::Column {
        cursor_id: index_cursor_id,
        column: 0,
        dest: reg_temp,
        default: None,
    });
    program.emit_insn(Insn::IsNull {
        reg: reg_temp,
        target_pc: label_set_null_flag,
    });
    // First row is not NULL; skip to probe.
    program.emit_insn(Insn::Goto {
        target_pc: label_after_build,
    });
    // First row IS NULL: record that the set contains NULL.
    program.preassign_label_to_next_insn(label_set_null_flag);
    program.emit_int(1, reg_null_in_set);

    program.preassign_label_to_next_insn(label_after_build);

    // Probe the set with the LHS value.
    let label_true = program.allocate_label();
    let label_null = program.allocate_label();
    let label_done = program.allocate_label();

    // SQLite: `NULL IN (...)` and `NULL NOT IN (...)` are NULL.
    program.emit_insn(Insn::IsNull {
        reg: lhs_register,
        target_pc: label_null,
    });

    program.emit_insn(Insn::Found {
        cursor_id: index_cursor_id,
        target_pc: label_true,
        record_reg: lhs_register,
        num_regs: 1,
    });
    // Not found. If the set contains a NULL, the result is also NULL (three-valued logic).
    program.emit_insn(Insn::If {
        reg: reg_null_in_set,
        target_pc: label_null,
        jump_if_null: false,
    });
    // No NULL in set → definite result.
    program.emit_int(if negated { 1 } else { 0 }, target_register);
    program.emit_insn(Insn::Goto {
        target_pc: label_done,
    });
    // Found.
    program.preassign_label_to_next_insn(label_true);
    program.emit_int(if negated { 0 } else { 1 }, target_register);
    program.emit_insn(Insn::Goto {
        target_pc: label_done,
    });
    // LHS was NULL, or LHS had no match and the set contains NULL.
    program.preassign_label_to_next_insn(label_null);
    program.emit_insn(Insn::Null {
        dest: target_register,
        dest_end: None,
    });

    program.preassign_label_to_next_insn(label_done);
    Ok(())
}
