use std::rc::Rc;

use limbo_sqlite3_parser::ast::{
    DistinctNames, Expr, InsertBody, OneSelect, QualifiedName, ResolveType, ResultColumn, Set,
    Upsert, UpsertDo, UpsertIndex, With,
};

use crate::error::{SQLITE_CONSTRAINT_NOTNULL, SQLITE_CONSTRAINT_PRIMARYKEY};
use crate::schema::{BTreeTable, IndexColumn, Table};
use crate::util::normalize_ident;
use crate::vdbe::builder::{ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::{IdxInsertFlags, InsertFlags, RegisterOrLiteral, SavepointOp};
use crate::vdbe::BranchOffset;
use crate::{
    schema::{Column, Schema},
    vdbe::{
        builder::{CursorType, ProgramBuilder},
        insn::Insn,
    },
};
use crate::{Result, SymbolTable, VirtualTable};

use super::emitter::Resolver;
use super::expr::{translate_expr, translate_expr_no_constant_opt, NoConstantOptReason};
use super::optimizer::rewrite_expr;
use super::plan::QueryDestination;
use super::select::translate_select;
use super::upsert::emit_upsert_do_update;

/// What to do when a specific conflict fires during an upsert.
#[derive(Debug, Clone)]
enum UpsertAction {
    /// Skip the row (ON CONFLICT DO NOTHING).
    Nothing,
    /// Update the conflicting row (ON CONFLICT DO UPDATE SET …).
    Update {
        sets: Vec<Set>,
        where_clause: Option<Expr>,
    },
}

/// Maps each conflict target to its resolved action for a single INSERT.
#[derive(Debug)]
struct UpsertPlan {
    /// Action for the rowid/INTEGER-PK `NotExists` check.
    rowid_action: Option<UpsertAction>,
    /// Actions for specific unique-index `NoConflict` checks (by index name).
    index_actions: Vec<(String, UpsertAction)>,
    /// `ON CONFLICT DO NOTHING` with no target → every conflict check Goto-skips.
    catch_all_nothing: bool,
}

/// Walk the ON CONFLICT chain and build a `UpsertPlan` that records which
/// action should be taken at each possible conflict site.
///
/// Errors are emitted here for:
/// - target-less DO UPDATE (invalid per SQLite spec)
/// - partial-index targets (where_clause on an index target — not yet supported)
/// - targets that do not match any PRIMARY KEY or UNIQUE constraint
fn resolve_upsert_targets(
    schema: &Schema,
    table_name: &str,
    btree_table: &BTreeTable,
    upsert: &Upsert,
) -> Result<UpsertPlan> {
    let mut plan = UpsertPlan {
        rowid_action: None,
        index_actions: Vec::new(),
        catch_all_nothing: false,
    };

    let mut current: Option<&Upsert> = Some(upsert);
    while let Some(clause) = current {
        match clause.index.as_deref() {
            None => {
                // Target-less ON CONFLICT clause.
                match clause.do_clause.as_ref() {
                    UpsertDo::Nothing => {
                        plan.catch_all_nothing = true;
                    }
                    UpsertDo::Set { .. } => {
                        crate::bail_parse_error!(
                            "ON CONFLICT DO UPDATE requires a conflict target"
                        );
                    }
                }
            }
            Some(UpsertIndex {
                targets,
                where_clause,
            }) => {
                if where_clause.is_some() {
                    crate::bail_parse_error!("partial-index ON CONFLICT target is not supported");
                }

                // Collect and normalise the target column names.
                let mut target_cols: std::collections::BTreeSet<String> =
                    std::collections::BTreeSet::new();
                for sorted_col in targets {
                    let col_name = match &sorted_col.expr {
                        Expr::Id(id) => normalize_ident(&id.0),
                        Expr::Name(name) => normalize_ident(&name.0),
                        other => {
                            crate::bail_parse_error!(
                                "unsupported ON CONFLICT target expression: {:?}",
                                other
                            )
                        }
                    };
                    target_cols.insert(col_name);
                }

                let action = match clause.do_clause.as_ref() {
                    UpsertDo::Nothing => UpsertAction::Nothing,
                    UpsertDo::Set { sets, where_clause } => UpsertAction::Update {
                        sets: sets.clone(),
                        where_clause: where_clause.clone(),
                    },
                };

                // Try to match target against the rowid alias / INTEGER PRIMARY KEY.
                let is_rowid_match = btree_table
                    .columns
                    .iter()
                    .find(|c| c.is_rowid_alias)
                    .and_then(|pk_col| pk_col.name.as_deref())
                    .map(|pk_name| {
                        let pk_norm = normalize_ident(pk_name);
                        target_cols.len() == 1 && target_cols.contains(&pk_norm)
                    })
                    .unwrap_or(false);

                if is_rowid_match {
                    if plan.rowid_action.is_some() {
                        crate::bail_parse_error!(
                            "multiple ON CONFLICT clauses for the same target"
                        );
                    }
                    plan.rowid_action = Some(action);
                } else {
                    // Try to match against unique indexes.
                    let mut matched = false;
                    for index in schema.get_indices(table_name) {
                        if !index.unique {
                            continue;
                        }
                        let idx_cols: std::collections::BTreeSet<String> = index
                            .columns
                            .iter()
                            .map(|c| normalize_ident(&c.name))
                            .collect();
                        if idx_cols == target_cols {
                            if plan.index_actions.iter().any(|(n, _)| n == &index.name) {
                                crate::bail_parse_error!(
                                    "multiple ON CONFLICT clauses for the same target"
                                );
                            }
                            plan.index_actions.push((index.name.clone(), action));
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        crate::bail_parse_error!(
                            "ON CONFLICT clause does not match any PRIMARY KEY or UNIQUE constraint"
                        );
                    }
                }
            }
        }
        current = clause.next.as_deref();
    }

    Ok(plan)
}

struct TempTableCtx {
    cursor_id: usize,
    loop_start_label: BranchOffset,
    loop_end_label: BranchOffset,
}

#[allow(clippy::too_many_arguments)]
pub fn translate_insert(
    query_mode: QueryMode,
    schema: &Schema,
    with: Option<With>,
    on_conflict: Option<ResolveType>,
    tbl_name: QualifiedName,
    columns: Option<DistinctNames>,
    mut body: InsertBody,
    _returning: Option<Vec<ResultColumn>>,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: 30,
        approx_num_labels: 5,
    };
    program.extend(&opts);
    if with.is_some() {
        crate::bail_parse_error!("WITH clause is not supported");
    }
    // Determine the requested OR-conflict resolution for `INSERT OR <action>`.
    // All five conflict actions are supported:
    //   * IGNORE    — skip the offending row, leaving no partial table/index state.
    //   * REPLACE   — delete the conflicting victim row(s) (and ALL their index
    //                 entries) before inserting the new row.
    //   * ABORT     — (default) roll back all rows inserted by this statement,
    //                 keep prior transaction work, return error. For multi-row INSERTs,
    //                 a statement-level savepoint ("_stmt") is opened before writes and
    //                 rolled back on conflict.
    //   * FAIL      — keep rows inserted so far, stop on conflict, return error.
    //                 No savepoint needed (partial work is intentionally preserved).
    //   * ROLLBACK  — like ABORT but should roll back the entire transaction.
    //                 Currently uses the same savepoint mechanism as ABORT (rolls back
    //                 only rows from this statement; full-transaction rollback is a
    //                 follow-up).
    //
    // NOTE: `on_conflict` here is only the *statement-level* `INSERT OR
    // <action>` clause. Per SQLite's precedence rules, this wins if present;
    // otherwise each individual UNIQUE/PRIMARY KEY constraint's own `ON
    // CONFLICT <action>` applies (defaulting to ABORT), looked up per
    // conflict-check site further below (`effective_pk_conflict` /
    // `effective_idx_conflict`) rather than as a single blanket action for the
    // whole statement.
    let on_conflict_abort = matches!(on_conflict, None | Some(ResolveType::Abort));
    let _on_conflict_fail = matches!(on_conflict, Some(ResolveType::Fail));
    let on_conflict_rollback = matches!(on_conflict, Some(ResolveType::Rollback));
    // ABORT and ROLLBACK need a statement-level savepoint to revert partial inserts on
    // multi-row INSERTs. Single-row INSERTs have no partial state to revert.
    // FAIL keeps rows inserted so far, so it does NOT need a savepoint.
    // The final `needs_stmt_savepoint` is computed after `inserting_multiple_rows` is known.
    //
    // When the statement doesn't specify an action (`on_conflict: None`), this
    // conservatively opens the savepoint even though a specific constraint's
    // own `ON CONFLICT REPLACE`/`IGNORE` might end up not needing it for any
    // given row: the savepoint is harmless to have open (it's simply released,
    // never rolled back, on rows that resolve via IGNORE/REPLACE) and is
    // required for any OTHER constraint on the same row that falls back to the
    // ABORT default.
    let on_conflict_needs_savepoint = on_conflict_abort || on_conflict_rollback;

    #[cfg(not(feature = "index_experimental"))]
    {
        if schema.table_has_indexes(&tbl_name.name.to_string()) {
            // Let's disable altering a table with indices altogether instead of checking column by
            // column to be extra safe.
            crate::bail_parse_error!(
                "INSERT table disabled for table with indexes and without index_experimental feature flag"
            );
        }
    }
    let table_name = &tbl_name.name;
    let table = match schema.get_table(table_name.0.as_str()) {
        Some(table) => table,
        None => crate::bail_parse_error!("no such table: {}", table_name),
    };
    if matches!(table.as_ref(), crate::schema::Table::View(_)) {
        crate::bail_parse_error!("cannot modify {} because it is a view", table_name.0);
    }

    let mut resolver = Resolver::new(schema, syms);

    if let Some(virtual_table) = &table.virtual_table() {
        program = translate_virtual_table_insert(
            program,
            virtual_table.clone(),
            columns,
            body,
            on_conflict,
            &resolver,
        )?;
        program.epilogue(super::emitter::TransactionMode::Write);
        return Ok(program);
    }

    let Some(btree_table) = table.btree() else {
        crate::bail_parse_error!("no such table: {}", table_name);
    };

    // Route WITHOUT ROWID tables to the index-format INSERT path.
    if !btree_table.has_rowid {
        return translate_insert_without_rowid(
            program,
            schema,
            &btree_table,
            columns,
            body,
            on_conflict,
            &mut resolver,
            on_conflict_needs_savepoint,
        );
    }

    let root_page = btree_table.root_page;

    // Extract the upsert clause from the INSERT body *before* body is consumed
    // by the pre-pass below.  Taking it here avoids borrowing `body` mutably
    // twice and makes the `upsert_plan` available for the rest of the function.
    let upsert_opt: Option<Upsert> = match &mut body {
        InsertBody::Select(_, u) => u.take(),
        InsertBody::DefaultValues => None,
    };
    let upsert_plan: Option<UpsertPlan> = upsert_opt
        .as_ref()
        .map(|u| resolve_upsert_targets(schema, &table_name.0, &*btree_table, u))
        .transpose()?;

    let mut values: Option<Vec<Expr>> = None;
    let inserting_multiple_rows = match &mut body {
        InsertBody::Select(select, _) => match select.body.select.as_mut() {
            // TODO see how to avoid clone
            OneSelect::Values(values_expr) if values_expr.len() <= 1 => {
                if values_expr.is_empty() {
                    crate::bail_parse_error!("no values to insert");
                }
                let mut param_idx = 1;
                for expr in values_expr.iter_mut().flat_map(|v| v.iter_mut()) {
                    rewrite_expr(expr, &mut param_idx)?;
                }
                values = values_expr.pop();
                false
            }
            _ => true,
        },
        InsertBody::DefaultValues => false,
    };
    // Savepoint is only needed for multi-row inserts; single-row has no partial state.
    let needs_stmt_savepoint = on_conflict_needs_savepoint && inserting_multiple_rows;

    let halt_label = program.allocate_label();
    let loop_start_label = program.allocate_label();

    let mut yield_reg_opt = None;
    let mut temp_table_ctx = None;
    let (num_values, cursor_id) = match body {
        InsertBody::Select(select, _) => {
            // Simple Common case of INSERT INTO <table> VALUES (...)
            if matches!(select.body.select.as_ref(),  OneSelect::Values(values) if values.len() <= 1)
            {
                (
                    values.as_ref().unwrap().len(),
                    program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone())),
                )
            } else {
                // Multiple rows - use coroutine for value population
                let yield_reg = program.alloc_register();
                let jump_on_definition_label = program.allocate_label();
                let start_offset_label = program.allocate_label();
                program.emit_insn(Insn::InitCoroutine {
                    yield_reg,
                    jump_on_definition: jump_on_definition_label,
                    start_offset: start_offset_label,
                });

                program.preassign_label_to_next_insn(start_offset_label);

                let query_destination = QueryDestination::CoroutineYield {
                    yield_reg,
                    coroutine_implementation_start: halt_label,
                };
                program.incr_nesting();
                let result = translate_select(
                    query_mode,
                    schema,
                    *select,
                    syms,
                    program,
                    query_destination,
                )?;
                program = result.program;
                program.decr_nesting();

                program.emit_insn(Insn::EndCoroutine { yield_reg });
                program.preassign_label_to_next_insn(jump_on_definition_label);

                let cursor_id =
                    program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone()));

                // From SQLite
                /* Set useTempTable to TRUE if the result of the SELECT statement
                 ** should be written into a temporary table (template 4).  Set to
                 ** FALSE if each output row of the SELECT can be written directly into
                 ** the destination table (template 3).
                 **
                 ** A temp table must be used if the table being updated is also one
                 ** of the tables being read by the SELECT statement.  Also use a
                 ** temp table in the case of row triggers.
                 */
                if program.is_table_open(&table) {
                    let temp_cursor_id =
                        program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone()));
                    temp_table_ctx = Some(TempTableCtx {
                        cursor_id: temp_cursor_id,
                        loop_start_label: program.allocate_label(),
                        loop_end_label: program.allocate_label(),
                    });

                    program.emit_insn(Insn::OpenEphemeral {
                        cursor_id: temp_cursor_id,
                        is_table: true,
                    });

                    // Main loop: fills the ephemeral temp table from the coroutine.
                    // Rollback of the real table writes is handled via the "_stmt" savepoint
                    // opened before OpenWrite; conflict handlers emit Savepoint RollbackTo before Halt.
                    program.preassign_label_to_next_insn(loop_start_label);

                    let yield_label = program.allocate_label();

                    program.emit_insn(Insn::Yield {
                        yield_reg,
                        end_offset: yield_label,
                    });
                    let record_reg = program.alloc_register();
                    program.emit_insn(Insn::MakeRecord {
                        start_reg: yield_reg + 1,
                        count: result.num_result_cols,
                        dest_reg: record_reg,
                        index_name: None,
                    });

                    let rowid_reg = program.alloc_register();
                    program.emit_insn(Insn::NewRowid {
                        cursor: temp_cursor_id,
                        rowid_reg,
                        prev_largest_reg: 0,
                    });

                    program.emit_insn(Insn::Insert {
                        cursor: temp_cursor_id,
                        key_reg: rowid_reg,
                        record_reg,
                        flag: InsertFlags::new(),
                        table_name: "".to_string(),
                    });

                    // loop back
                    program.emit_insn(Insn::Goto {
                        target_pc: loop_start_label,
                    });

                    program.preassign_label_to_next_insn(yield_label);

                    if needs_stmt_savepoint {
                        program.emit_insn(Insn::Savepoint {
                            op: SavepointOp::Begin,
                            name: "_stmt".to_string(),
                        });
                    }
                    program.emit_insn(Insn::OpenWrite {
                        cursor_id,
                        root_page: RegisterOrLiteral::Literal(root_page),
                        name: table_name.0.clone(),
                    });
                } else {
                    if needs_stmt_savepoint {
                        program.emit_insn(Insn::Savepoint {
                            op: SavepointOp::Begin,
                            name: "_stmt".to_string(),
                        });
                    }
                    program.emit_insn(Insn::OpenWrite {
                        cursor_id,
                        root_page: RegisterOrLiteral::Literal(root_page),
                        name: table_name.0.clone(),
                    });

                    // Main loop: each iteration yields a row from the coroutine, processes it,
                    // and inserts it. Rollback of partial inserts is handled via the "_stmt" savepoint
                    // opened before OpenWrite; conflict handlers emit Savepoint RollbackTo before Halt.
                    program.preassign_label_to_next_insn(loop_start_label);
                    program.emit_insn(Insn::Yield {
                        yield_reg,
                        end_offset: halt_label,
                    });
                }

                yield_reg_opt = Some(yield_reg);
                (result.num_result_cols, cursor_id)
            }
        }
        InsertBody::DefaultValues => (
            0,
            program.alloc_cursor_id(CursorType::BTreeTable(btree_table.clone())),
        ),
    };

    // allocate cursor id's for each btree index cursor we'll need to populate the indexes
    // (idx name, root_page, idx cursor id)
    let idx_cursors = schema
        .get_indices(&table_name.0)
        .iter()
        .map(|idx| {
            (
                &idx.name,
                idx.root_page,
                program.alloc_cursor_id(CursorType::BTreeIndex(idx.clone())),
            )
        })
        .collect::<Vec<(&String, usize, usize)>>();

    let column_mappings = resolve_columns_for_insert(&table, &columns, num_values)?;
    // Check if rowid was provided (through INTEGER PRIMARY KEY as a rowid alias)
    let rowid_alias_index = btree_table.columns.iter().position(|c| c.is_rowid_alias);
    let has_user_provided_rowid = {
        assert_eq!(column_mappings.len(), btree_table.columns.len());
        if let Some(index) = rowid_alias_index {
            column_mappings[index].value_index.is_some()
        } else {
            false
        }
    };

    // allocate a register for each column in the table. if not provided by user, they will simply be set as null.
    // allocate an extra register for rowid regardless of whether user provided a rowid alias column.
    let num_cols = btree_table.columns.len();
    let rowid_reg = program.alloc_registers(num_cols + 1);
    let column_registers_start = rowid_reg + 1;
    let rowid_alias_reg = {
        if has_user_provided_rowid {
            Some(column_registers_start + rowid_alias_index.unwrap())
        } else {
            None
        }
    };

    let record_register = program.alloc_register();

    // Names of the columns that make up the table's PRIMARY KEY (normalized for
    // case-insensitive comparison). Used as defense-in-depth so that an unmapped
    // PRIMARY KEY column is never silently populated with NULL, even when the
    // key is declared via a table-level `PRIMARY KEY(...)` constraint.
    let primary_key_columns: Vec<String> = btree_table
        .primary_key_columns
        .iter()
        .map(|(name, _)| normalize_ident(name))
        .collect();

    if inserting_multiple_rows {
        if let Some(ref temp_table_ctx) = temp_table_ctx {
            // Rewind loop to read from ephemeral table
            program.emit_insn(Insn::Rewind {
                cursor_id: temp_table_ctx.cursor_id,
                pc_if_empty: temp_table_ctx.loop_end_label,
            });
            program.preassign_label_to_next_insn(temp_table_ctx.loop_start_label);
        }
        populate_columns_multiple_rows(
            &mut program,
            &column_mappings,
            column_registers_start,
            yield_reg_opt.unwrap() + 1,
            &resolver,
            &temp_table_ctx,
            &primary_key_columns,
        )?;
    } else {
        // Single row - populate registers directly
        if needs_stmt_savepoint {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::Begin,
                name: "_stmt".to_string(),
            });
        }
        program.emit_insn(Insn::OpenWrite {
            cursor_id,
            root_page: RegisterOrLiteral::Literal(root_page),
            name: table_name.0.clone(),
        });

        populate_column_registers(
            &mut program,
            &values.unwrap(),
            &column_mappings,
            column_registers_start,
            rowid_reg,
            &resolver,
            &primary_key_columns,
        )?;
    }
    // Open all the index btrees for writing
    for idx_cursor in idx_cursors.iter() {
        program.emit_insn(Insn::OpenWrite {
            cursor_id: idx_cursor.2,
            root_page: idx_cursor.1.into(),
            name: idx_cursor.0.clone(),
        });
    }
    // Common record insertion logic for both single and multiple rows
    let check_rowid_is_integer_label = rowid_alias_reg.and(Some(program.allocate_label()));
    if let Some(reg) = rowid_alias_reg {
        // for the row record, the rowid alias column (INTEGER PRIMARY KEY) is always set to NULL
        // and its value is copied to the rowid register. in the case where a single row is inserted,
        // the value is written directly to the rowid register (see populate_column_registers()).
        // again, not sure why this only happens in the single row case, but let's mimic sqlite.
        // in the single row case we save a Copy instruction, but in the multiple rows case we do
        // it here in the loop.
        if inserting_multiple_rows {
            program.emit_insn(Insn::Copy {
                src_reg: reg,
                dst_reg: rowid_reg,
                amount: 0, // TODO: rename 'amount' to something else; amount==0 means 1
            });
            // for the row record, the rowid alias column is always set to NULL
            program.emit_insn(Insn::SoftNull { reg });
        }
        // the user provided rowid value might itself be NULL. If it is, we create a new rowid on the next instruction.
        program.emit_insn(Insn::NotNull {
            reg: rowid_reg,
            target_pc: check_rowid_is_integer_label.unwrap(),
        });
    }

    // Create new rowid if a) not provided by user or b) provided by user but is NULL
    program.emit_insn(Insn::NewRowid {
        cursor: cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });

    if let Some(must_be_int_label) = check_rowid_is_integer_label {
        program.resolve_label(must_be_int_label, program.offset());
        // If the user provided a rowid, it must be an integer.
        program.emit_insn(Insn::MustBeInt { reg: rowid_reg });
    }

    // Label that all conflict-resolution paths converge on once this row has
    // been fully processed. For `INSERT OR IGNORE`, a conflicting row jumps here
    // (skipping every IdxInsert and the table Insert) so it leaves NO partial
    // index/table state. It is preassigned to the instruction immediately AFTER
    // the table Insert below, i.e. the natural continue point: single-row falls
    // through to the end; multi-row continues via Next / Goto loop_start.
    let next_record_label = program.allocate_label();

    // ---------------------------------------------------------------------
    // CONFLICT-CHECK PHASE
    //
    // Audit checkpoint (partial-state safety): EVERY conflict check (the rowid
    // NotExists plus each unique-index NoConflict) is emitted here, BEFORE any
    // IdxInsert or the table Insert in the insert phase further below. That way
    // an IGNORE that bails on a late-discovered conflict cannot have written any
    // earlier index entry, and a REPLACE only ever deletes pre-existing victims.
    // ---------------------------------------------------------------------

    // Check uniqueness constraint for rowid if it was provided by user.
    // When the DB allocates it there are no need for separate uniqueness checks.
    if has_user_provided_rowid {
        // NotExists falls through (to pc+1) when the rowid ALREADY exists (the
        // table cursor is then positioned on the conflicting victim row), and
        // jumps to `no_rowid_conflict_label` when it does not exist.
        let no_rowid_conflict_label = program.allocate_label();
        program.emit_insn(Insn::NotExists {
            cursor: cursor_id,
            rowid_reg,
            target_pc: no_rowid_conflict_label,
        });

        // Upsert DO NOTHING on the rowid/PK target or a catch-all clause.
        let rowid_upsert_nothing = upsert_plan.as_ref().map_or(false, |p| {
            p.catch_all_nothing || matches!(p.rowid_action, Some(UpsertAction::Nothing))
        });

        // Precedence: an explicit statement-level `INSERT OR <action>` wins if
        // present; otherwise fall back to the PRIMARY KEY constraint's own
        // `ON CONFLICT <action>` (defaults to ABORT when the constraint didn't
        // specify one — see `BTreeTable::primary_key_conflict`).
        let effective_pk_conflict = on_conflict.unwrap_or(btree_table.primary_key_conflict);

        if rowid_upsert_nothing {
            // Skip this row entirely (upsert DO NOTHING).
            program.emit_insn(Insn::Goto {
                target_pc: next_record_label,
            });
        } else if let Some(UpsertAction::Update { sets, where_clause }) =
            upsert_plan.as_ref().and_then(|p| p.rowid_action.as_ref())
        {
            emit_upsert_do_update(
                &mut program,
                schema,
                &table_name.0,
                &btree_table,
                cursor_id,
                column_registers_start,
                rowid_reg,
                num_cols,
                rowid_alias_index,
                sets,
                where_clause.as_ref(),
                next_record_label,
                needs_stmt_savepoint,
                &idx_cursors,
                &mut resolver,
            )?;
        } else if matches!(effective_pk_conflict, ResolveType::Ignore) {
            // Skip this row entirely; the table cursor is on the victim but we
            // perform no inserts, so no partial state is produced.
            program.emit_insn(Insn::Goto {
                target_pc: next_record_label,
            });
        } else if matches!(effective_pk_conflict, ResolveType::Replace) {
            // REPLACE: the table cursor is positioned on the victim, so delete
            // the victim (and ALL its index entries) and carry on. Because this
            // deletion also removes the victim's unique-index entries, a later
            // NoConflict on those same entries will find nothing — this is what
            // dedups a single existing row that collides on both rowid and a
            // unique index (no double delete).
            emit_replace_victim_deletion(
                &mut program,
                schema,
                &table_name.0,
                cursor_id,
                &idx_cursors,
            );
        } else {
            let rowid_column_name = if let Some(index) = rowid_alias_index {
                btree_table
                    .columns
                    .get(index)
                    .unwrap()
                    .name
                    .as_ref()
                    .expect("column name is None")
            } else {
                "rowid"
            };

            if needs_stmt_savepoint {
                program.emit_insn(Insn::Savepoint {
                    op: SavepointOp::RollbackTo,
                    name: "_stmt".to_string(),
                });
            }
            program.emit_insn(Insn::Halt {
                err_code: SQLITE_CONSTRAINT_PRIMARYKEY,
                description: format!("{}.{}", table_name.0, rowid_column_name),
            });
        }
        program.preassign_label_to_next_insn(no_rowid_conflict_label);
    }

    match table.btree() {
        Some(t) if t.is_strict => {
            program.emit_insn(Insn::TypeCheck {
                start_reg: column_registers_start,
                count: num_cols,
                check_generated: true,
                table_reference: Rc::clone(&t),
            });
        }
        _ => (),
    }

    // Pre-build the index records for every index, then run all the conflict
    // checks, then perform all the insertions. Keeping the per-index scratch
    // registers and MakeRecord results alive across these phases lets us emit
    // every NoConflict before any IdxInsert (see the partial-state checkpoint).
    struct IndexInsertPlan {
        idx_cursor_id: usize,
        idx_start_reg: usize,
        num_cols: usize,
        record_reg: usize,
        unique: bool,
        conflict_description: String,
        /// Normalised index name — used to look up upsert actions.
        index_name: String,
        /// This index's own `ON CONFLICT <action>` (from the UNIQUE/PRIMARY KEY
        /// constraint it backs; `Abort` for an explicit `CREATE INDEX` or when
        /// the constraint didn't specify one). Used only when there is no
        /// statement-level `INSERT OR <action>` — see `on_conflict` below.
        on_conflict: ResolveType,
    }

    let index_col_mappings = resolve_indicies_for_insert(schema, table.as_ref(), &column_mappings)?;
    let mut index_insert_plans: Vec<IndexInsertPlan> = Vec::with_capacity(index_col_mappings.len());
    for index_col_mapping in index_col_mappings {
        // find which cursor we opened earlier for this index
        let idx_cursor_id = idx_cursors
            .iter()
            .find(|(name, _, _)| *name == &index_col_mapping.idx_name)
            .map(|(_, _, c_id)| *c_id)
            .expect("no cursor found for index");

        let num_cols = index_col_mapping.columns.len();
        // allocate scratch registers for the index columns plus rowid
        let idx_start_reg = program.alloc_registers(num_cols + 1);

        // copy each index column from the table's column registers into these scratch regs
        for (i, col) in index_col_mapping.columns.iter().enumerate() {
            // copy from the table's column register over to the index's scratch register

            program.emit_insn(Insn::Copy {
                src_reg: column_registers_start + col.0,
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

        let index = schema
            .get_index(&table_name.0, &index_col_mapping.idx_name)
            .expect("index should be present");

        let column_names = index_col_mapping.columns.iter().enumerate().fold(
            String::with_capacity(50),
            |mut accum, (idx, (index, _))| {
                if idx > 0 {
                    accum.push_str(", ");
                }

                accum.push_str(&btree_table.name);
                accum.push('.');

                let name = btree_table
                    .columns
                    .get(*index)
                    .unwrap()
                    .name
                    .as_ref()
                    .expect("column name is None");
                accum.push_str(name);

                accum
            },
        );

        let record_reg = program.alloc_register();
        // Clone the name before it is moved into MakeRecord so we can store it
        // in IndexInsertPlan for upsert action look-ups.
        let this_index_name = index_col_mapping.idx_name.clone();
        program.emit_insn(Insn::MakeRecord {
            start_reg: idx_start_reg,
            count: num_cols + 1,
            dest_reg: record_reg,
            index_name: Some(index_col_mapping.idx_name),
        });

        index_insert_plans.push(IndexInsertPlan {
            idx_cursor_id,
            idx_start_reg,
            num_cols,
            record_reg,
            unique: index.unique,
            conflict_description: column_names,
            index_name: this_index_name,
            on_conflict: index.on_conflict,
        });
    }

    // Unique-index conflict checks (run for every unique index BEFORE any insert).
    for plan in index_insert_plans.iter() {
        if !plan.unique {
            continue;
        }
        // NoConflict jumps to `no_idx_conflict_label` when there is NO conflict
        // (or the key contains NULLs); it falls through (to pc+1) when a
        // conflicting index entry exists, leaving the INDEX cursor positioned on
        // that entry.
        let no_idx_conflict_label = program.allocate_label();
        program.emit_insn(Insn::NoConflict {
            cursor_id: plan.idx_cursor_id,
            target_pc: no_idx_conflict_label,
            record_reg: plan.idx_start_reg,
            num_regs: plan.num_cols,
        });

        // Precedence: an explicit statement-level `INSERT OR <action>` wins if
        // present; otherwise fall back to this index's own backing UNIQUE/
        // PRIMARY KEY constraint's `ON CONFLICT <action>` (defaults to ABORT —
        // see `IndexInsertPlan::on_conflict` / `Index::on_conflict`).
        let effective_idx_conflict = on_conflict.unwrap_or(plan.on_conflict);

        // Upsert action for this specific index target, or catch-all.
        let idx_upsert_nothing = upsert_plan.as_ref().map_or(false, |p| {
            p.catch_all_nothing
                || p.index_actions
                    .iter()
                    .find(|(n, _)| n == &plan.index_name)
                    .map_or(false, |(_, a)| matches!(a, UpsertAction::Nothing))
        });
        let idx_upsert_update = upsert_plan.as_ref().map_or(false, |p| {
            !p.catch_all_nothing
                && p.index_actions
                    .iter()
                    .find(|(n, _)| n == &plan.index_name)
                    .map_or(false, |(_, a)| matches!(a, UpsertAction::Update { .. }))
        });

        if idx_upsert_nothing {
            // Upsert DO NOTHING — skip this row.
            program.emit_insn(Insn::Goto {
                target_pc: next_record_label,
            });
        } else if idx_upsert_update {
            // Position table cursor on the victim row via the index.
            let victim_rowid_reg = program.alloc_register();
            program.emit_insn(Insn::IdxRowId {
                cursor_id: plan.idx_cursor_id,
                dest: victim_rowid_reg,
            });
            let victim_seeked_label = program.allocate_label();
            program.emit_insn(Insn::SeekRowid {
                cursor_id,
                src_reg: victim_rowid_reg,
                target_pc: victim_seeked_label,
            });
            // SeekRowid falls through when found — cursor is now on the victim.
            if let Some((_, UpsertAction::Update { sets, where_clause })) = upsert_plan
                .as_ref()
                .and_then(|p| p.index_actions.iter().find(|(n, _)| n == &plan.index_name))
            {
                emit_upsert_do_update(
                    &mut program,
                    schema,
                    &table_name.0,
                    &btree_table,
                    cursor_id,
                    column_registers_start,
                    rowid_reg,
                    num_cols,
                    rowid_alias_index,
                    sets,
                    where_clause.as_ref(),
                    next_record_label,
                    needs_stmt_savepoint,
                    &idx_cursors,
                    &mut resolver,
                )?;
            }
            program.preassign_label_to_next_insn(victim_seeked_label);
        } else if matches!(effective_idx_conflict, ResolveType::Ignore) {
            program.emit_insn(Insn::Goto {
                target_pc: next_record_label,
            });
        } else if matches!(effective_idx_conflict, ResolveType::Replace) {
            // REPLACE at a unique-index conflict: the index cursor is on the
            // conflicting entry. Read its rowid, seek the TABLE cursor onto that
            // victim row, then delete the victim (and ALL its index entries).
            //
            // Audit checkpoint (victim dedup): this interleaved check->delete
            // structure naturally dedups — deletions only ever REMOVE entries and
            // can never turn an earlier "no conflict" into a conflict, so a row
            // that collides on multiple unique indexes pointing at DIFFERENT
            // existing rows deletes every distinct victim, while a single row
            // colliding on several keys is deleted once (later NoConflicts then
            // find nothing).
            let victim_rowid_reg = program.alloc_register();
            program.emit_insn(Insn::IdxRowId {
                cursor_id: plan.idx_cursor_id,
                dest: victim_rowid_reg,
            });
            // SeekRowid positions the table cursor on the victim. It should
            // always be found (the index entry references a live row); route the
            // not-found branch to the same place as the found path so that, in
            // the worst case, we simply skip a spurious delete rather than
            // corrupt anything.
            let victim_seeked_label = program.allocate_label();
            program.emit_insn(Insn::SeekRowid {
                cursor_id,
                src_reg: victim_rowid_reg,
                target_pc: victim_seeked_label,
            });
            emit_replace_victim_deletion(
                &mut program,
                schema,
                &table_name.0,
                cursor_id,
                &idx_cursors,
            );
            program.preassign_label_to_next_insn(victim_seeked_label);
        } else {
            if needs_stmt_savepoint {
                program.emit_insn(Insn::Savepoint {
                    op: SavepointOp::RollbackTo,
                    name: "_stmt".to_string(),
                });
            }
            program.emit_insn(Insn::Halt {
                err_code: SQLITE_CONSTRAINT_PRIMARYKEY,
                description: plan.conflict_description.clone(),
            });
        }

        program.preassign_label_to_next_insn(no_idx_conflict_label);
    }

    for (i, col) in column_mappings
        .iter()
        .enumerate()
        .filter(|(_, col)| col.column.notnull && !col.column.is_rowid_alias)
    {
        let target_reg = i + column_registers_start;
        program.emit_insn(Insn::HaltIfNull {
            target_reg,
            err_code: SQLITE_CONSTRAINT_NOTNULL,
            description: format!(
                "{}.{}",
                table_name,
                col.column
                    .name
                    .as_ref()
                    .expect("Column name must be present")
            ),
        });
    }

    // ---------------------------------------------------------------------
    // INSERT PHASE
    // ---------------------------------------------------------------------

    // Audit checkpoint (cursor-position hazard): for REPLACE, a unique-index
    // conflict deletion moved the TABLE cursor via SeekRowid (and a rowid
    // conflict deletion left it on the now-deleted victim). The final table
    // Insert (op_insert) is emitted with moved_before=true and otherwise relies
    // on the positioning established by NotExists / NewRowid, so we MUST re-seek
    // the table cursor to the target rowid before inserting. SeekRowid leaves the
    // cursor on the correct leaf page even when the rowid is absent (which it now
    // is), making moved_before=true valid. Both the found and not-found branches
    // converge on the very next instruction, so this is purely a reposition.
    //
    // Whether a REPLACE can actually happen at runtime now depends on a
    // per-constraint lookup (statement-level `OR REPLACE` wins if present,
    // else each constraint's own `ON CONFLICT REPLACE`), so this must check
    // every conflict site that could possibly resolve to REPLACE for this
    // statement, not just a single blanket flag. Emitting the reseek is always
    // safe even when no delete actually happened for a given row (see above),
    // so it is fine to over-approximate here.
    let any_replace_possible = (has_user_provided_rowid
        && matches!(
            on_conflict.unwrap_or(btree_table.primary_key_conflict),
            ResolveType::Replace
        ))
        || index_insert_plans.iter().any(|plan| {
            plan.unique
                && matches!(
                    on_conflict.unwrap_or(plan.on_conflict),
                    ResolveType::Replace
                )
        });
    if any_replace_possible {
        let reseek_done_label = program.allocate_label();
        program.emit_insn(Insn::SeekRowid {
            cursor_id,
            src_reg: rowid_reg,
            target_pc: reseek_done_label,
        });
        program.preassign_label_to_next_insn(reseek_done_label);
    }

    // Now perform every index insertion using the unpacked registers.
    for plan in index_insert_plans.iter() {
        // Seek-skip: a UNIQUE index's cursor was already positioned at the
        // target key by its own `NoConflict` check above -- but only when
        // that check's "no conflict" jump is the *only* way execution could
        // reach this point for this cursor:
        // - Non-unique indexes never get a preceding `NoConflict` check at
        //   all (the loop above `continue`s past them), so their cursors are
        //   never pre-positioned and must always seek.
        // - The `idx_upsert_nothing` / `idx_upsert_update` / `Ignore` branches
        //   all `Goto next_record_label`, skipping this row's INSERT PHASE
        //   entirely, so they never reach this loop.
        // - The `else` (Abort) branch halts the program, so it never reaches
        //   this loop either.
        // - The one remaining fallthrough is `ResolveType::Replace`, which
        //   can delete this row's entry from EVERY index cursor (see
        //   `emit_replace_victim_deletion`) using the *victim* row's column
        //   values -- values unrelated to the row we're about to insert here
        //   for any index other than the one whose conflict triggered the
        //   replace. That leaves the cursor's post-delete position not
        //   provably tied to this insert's key, so REPLACE being possible
        //   anywhere in this statement disqualifies every plan, not just the
        //   one that conflicted.
        //
        // Note: at the time of writing, `op_idx_insert`'s runtime handling
        // for a *unique* index cursor unconditionally re-verifies via
        // `key_exists_in_index` regardless of this flag (see
        // `vdbe/execute/mutate.rs`), as a defense-in-depth duplicate check.
        // Setting the flag here is still correct and forward-compatible --
        // it precisely captures the translate-time guarantee -- but fully
        // realizing the perf win additionally requires that runtime check to
        // honor `USE_SEEK` for unique cursors too, which is out of scope for
        // this file.
        let already_positioned_by_conflict_check = plan.unique && !any_replace_possible;
        program.emit_insn(Insn::IdxInsert {
            cursor_id: plan.idx_cursor_id,
            record_reg: plan.record_reg,
            unpacked_start: Some(plan.idx_start_reg), // TODO: enable optimization
            unpacked_count: Some((plan.num_cols + 1) as u16),
            flags: IdxInsertFlags::new().use_seek(already_positioned_by_conflict_check),
        });
    }

    // Create and insert the record
    program.emit_insn(Insn::MakeRecord {
        start_reg: column_registers_start,
        count: num_cols,
        dest_reg: record_register,
        index_name: None,
    });

    program.emit_insn(Insn::Insert {
        cursor: cursor_id,
        key_reg: rowid_reg,
        record_reg: record_register,
        flag: InsertFlags::new(),
        table_name: table_name.to_string(),
    });

    // IGNORE jumps here, skipping all inserts; a successful insert falls through.
    program.preassign_label_to_next_insn(next_record_label);

    if inserting_multiple_rows {
        if let Some(temp_table_ctx) = temp_table_ctx {
            program.emit_insn(Insn::Next {
                cursor_id: temp_table_ctx.cursor_id,
                pc_if_next: temp_table_ctx.loop_start_label,
            });
            program.preassign_label_to_next_insn(temp_table_ctx.loop_end_label);

            program.emit_insn(Insn::Close {
                cursor_id: temp_table_ctx.cursor_id,
            });
        } else {
            // For multiple rows which not require a temp table, loop back
            program.emit_insn(Insn::Goto {
                target_pc: loop_start_label,
            });
        }
    }

    // Release statement savepoint after all rows processed successfully.
    // IMPORTANT: halt_label must be resolved BEFORE emitting the Release instruction,
    // so that the direct-coroutine Yield (end_offset: halt_label) jumps to the Release
    // when the coroutine finishes. For the temp-table path, control falls through here
    // naturally and executes the Release as well. Without this ordering, the Release
    // would be unreachable via the Yield-done path.
    program.resolve_label(halt_label, program.offset());
    if needs_stmt_savepoint {
        program.emit_insn(Insn::Savepoint {
            op: SavepointOp::Release,
            name: "_stmt".to_string(),
        });
    }
    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}

/// Emit the code that removes a single conflicting "victim" row during
/// `INSERT OR REPLACE`.
///
/// Preconditions: the table cursor (`table_cursor_id`) is already positioned on
/// the victim row (e.g. by `NotExists` falling through at a rowid conflict, or by
/// `IdxRowId` + `SeekRowid` at a unique-index conflict).
///
/// This mirrors [`super::emitter::emit_delete_insns`]: it reads the OLD column
/// values of the victim directly from the positioned table cursor and deletes
/// the victim's entry from EVERY index (unique and non-unique) before deleting
/// the table row itself. Removing entries can never invalidate an earlier
/// "no conflict" decision, so interleaving these deletions with the remaining
/// conflict checks is safe (and naturally dedups victims — see
/// `translate_insert`).
///
/// `idx_cursors` is the `(name, root_page, cursor_id)` list opened for writing in
/// `translate_insert`; only indexes that have a materialized cursor are touched.
fn emit_replace_victim_deletion(
    program: &mut ProgramBuilder,
    schema: &Schema,
    table_name: &str,
    table_cursor_id: usize,
    idx_cursors: &[(&String, usize, usize)],
) {
    // Delete from all indexes first, then the table row (matching emit_delete_insns).
    for index in schema.get_indices(table_name) {
        // Find the cursor we opened for this index; skip if it was not
        // materialized (indexes are only opened under index_experimental).
        let Some((_, _, idx_cursor_id)) =
            idx_cursors.iter().find(|(name, _, _)| **name == index.name)
        else {
            continue;
        };
        let num_regs = index.columns.len() + 1;
        let start_reg = program.alloc_registers(num_regs);
        // Emit the OLD values of the columns that make up the index, read from
        // the positioned table cursor.
        for (reg_offset, idx_col) in index.columns.iter().enumerate() {
            program.emit_column(
                table_cursor_id,
                idx_col.pos_in_table,
                start_reg + reg_offset,
            );
        }
        // The trailing register is the victim's rowid.
        program.emit_insn(Insn::RowId {
            cursor_id: table_cursor_id,
            dest: start_reg + num_regs - 1,
        });
        program.emit_insn(Insn::IdxDelete {
            start_reg,
            num_regs,
            cursor_id: *idx_cursor_id,
        });
    }
    // Finally remove the victim's table row.
    program.emit_insn(Insn::Delete {
        cursor_id: table_cursor_id,
    });
}

#[derive(Debug)]
/// Represents how a column should be populated during an INSERT.
/// Contains both the column definition and optionally the index into the VALUES tuple.
struct ColumnMapping<'a> {
    /// Reference to the column definition from the table schema
    column: &'a Column,
    /// If Some(i), use the i-th value from the VALUES tuple
    /// If None, use NULL (column was not specified in INSERT statement)
    value_index: Option<usize>,
    /// The default value for the column, if defined
    default_value: Option<&'a Expr>,
}

/// Resolves how each column in a table should be populated during an INSERT.
/// Returns a Vec of ColumnMapping, one for each column in the table's schema.
///
/// For each column, specifies:
/// 1. The column definition (type, constraints, etc)
/// 2. Where to get the value from:
///    - Some(i) -> use i-th value from the VALUES tuple
///    - None -> use NULL (column wasn't specified in INSERT)
///
/// Two cases are handled:
/// 1. No column list specified (INSERT INTO t VALUES ...):
///    - Values are assigned to columns in table definition order
///    - If fewer values than columns, remaining columns map to None
/// 2. Column list specified (INSERT INTO t (col1, col3) VALUES ...):
///    - Named columns map to their corresponding value index
///    - Unspecified columns map to None
fn resolve_columns_for_insert<'a>(
    table: &'a Table,
    columns: &Option<DistinctNames>,
    num_values: usize,
) -> Result<Vec<ColumnMapping<'a>>> {
    let table_columns = table.columns();
    // Case 1: No columns specified - map values to columns in order
    if columns.is_none() {
        if num_values != table_columns.len() {
            crate::bail_parse_error!(
                "table {} has {} columns but {} values were supplied",
                &table.get_name(),
                table_columns.len(),
                num_values
            );
        }

        // Map each column to either its corresponding value index or None
        return Ok(table_columns
            .iter()
            .enumerate()
            .map(|(i, col)| ColumnMapping {
                column: col,
                value_index: if i < num_values { Some(i) } else { None },
                default_value: col.default.as_ref(),
            })
            .collect());
    }

    // Case 2: Columns specified - map named columns to their values
    let mut mappings: Vec<_> = table_columns
        .iter()
        .map(|col| ColumnMapping {
            column: col,
            value_index: None,
            default_value: col.default.as_ref(),
        })
        .collect();

    // Map each named column to its value index
    for (value_index, column_name) in columns.as_ref().unwrap().iter().enumerate() {
        let column_name = normalize_ident(column_name.0.as_str());
        let table_index = table_columns.iter().position(|c| {
            c.name
                .as_ref()
                .map_or(false, |name| name.eq_ignore_ascii_case(&column_name))
        });

        let Some(table_index) = table_index else {
            crate::bail_parse_error!(
                "table {} has no column named {}",
                &table.get_name(),
                column_name
            );
        };

        mappings[table_index].value_index = Some(value_index);
    }

    Ok(mappings)
}

/// Represents how a column in an index should be populated during an INSERT.
/// Similar to ColumnMapping above but includes the index name, as well as multiple
/// possible value indices for each.
#[derive(Debug, Default)]
struct IndexColMapping {
    idx_name: String,
    columns: Vec<(usize, IndexColumn)>,
    value_indicies: Vec<Option<usize>>,
}

impl IndexColMapping {
    fn new(name: String) -> Self {
        IndexColMapping {
            idx_name: name,
            ..Default::default()
        }
    }
}

/// Example:
/// Table 'test': (a, b, c);
/// Index 'idx': test(a, b);
///________________________________
/// Insert (a, c): (2, 3)
/// Record: (2, NULL, 3)
/// IndexColMapping: (a, b) = (2, NULL)
fn resolve_indicies_for_insert(
    schema: &Schema,
    table: &Table,
    columns: &[ColumnMapping<'_>],
) -> Result<Vec<IndexColMapping>> {
    let mut index_col_mappings = Vec::new();
    // Iterate over all indices for this table
    for index in schema.get_indices(table.get_name()) {
        let mut idx_map = IndexColMapping::new(index.name.clone());
        // For each column in the index (in the order defined by the index),
        // try to find the corresponding column in the insert’s column mapping.
        for idx_col in &index.columns {
            let target_name = normalize_ident(idx_col.name.as_str());
            if let Some((i, col_mapping)) = columns.iter().enumerate().find(|(_, mapping)| {
                mapping
                    .column
                    .name
                    .as_ref()
                    .map_or(false, |name| name.eq_ignore_ascii_case(&target_name))
            }) {
                idx_map.columns.push((i, idx_col.clone()));
                idx_map.value_indicies.push(col_mapping.value_index);
            } else {
                return Err(crate::LimboError::ParseError(format!(
                    "Column {} not found in index {}",
                    target_name, index.name
                )));
            }
        }
        // Add the mapping if at least one column was found.
        if !idx_map.columns.is_empty() {
            index_col_mappings.push(idx_map);
        }
    }
    Ok(index_col_mappings)
}

fn populate_columns_multiple_rows(
    program: &mut ProgramBuilder,
    column_mappings: &[ColumnMapping],
    column_registers_start: usize,
    yield_reg: usize,
    resolver: &Resolver,
    temp_table_ctx: &Option<TempTableCtx>,
    primary_key_columns: &[String],
) -> Result<()> {
    let mut value_index_seen = 0;
    let mut other_values_seen = 0;
    for (i, mapping) in column_mappings.iter().enumerate() {
        let target_reg = column_registers_start + i;

        other_values_seen += 1;
        if let Some(value_index) = mapping.value_index {
            // Decrement as we have now seen a value index instead
            other_values_seen -= 1;
            if let Some(temp_table_ctx) = temp_table_ctx {
                program.emit_column(
                    temp_table_ctx.cursor_id,
                    value_index_seen,
                    column_registers_start + i,
                );
            } else {
                program.emit_insn(Insn::Copy {
                    src_reg: yield_reg + value_index_seen,
                    dst_reg: column_registers_start + value_index + other_values_seen,
                    amount: 0,
                });
            }

            value_index_seen += 1;
        } else if mapping.column.is_rowid_alias {
            program.emit_insn(Insn::SoftNull { reg: target_reg });
        } else if let Some(default_expr) = mapping.default_value {
            translate_expr(program, None, default_expr, target_reg, resolver)?;
        } else {
            // Column was not specified and has no DEFAULT - use NULL if it is
            // nullable, otherwise error.
            //
            // Rowid-alias columns may be NULL here because the engine
            // autogenerates a rowid for them. A column belonging to the table's
            // PRIMARY KEY is never silently NULL-able: we consult both the
            // per-column `primary_key` flag AND the table's `primary_key_columns`
            // list (case-insensitively) so that an unmapped PK column is rejected
            // even if some earlier path failed to set the flag.
            let is_nullable = column_is_nullable(mapping.column, primary_key_columns);
            if is_nullable {
                program.emit_insn(Insn::Null {
                    dest: target_reg,
                    dest_end: None,
                });
            } else {
                crate::bail_parse_error!(
                    "column {} is not nullable",
                    mapping.column.name.as_ref().expect("column name is None")
                );
            }
        }
    }
    Ok(())
}

/// Determines whether an *unmapped* column (no VALUES entry, no DEFAULT) may be
/// populated with NULL during an INSERT.
///
/// A column is NULL-able unless it is part of the table's PRIMARY KEY, with one
/// exception: a rowid-alias column (`INTEGER PRIMARY KEY`) is treated as
/// NULL-able because the engine autogenerates a rowid for it.
///
/// PRIMARY KEY membership is checked via both the per-column `primary_key` flag
/// and the table's `primary_key_columns` list (compared case-insensitively).
/// The latter is defense-in-depth: a table-level `PRIMARY KEY(col)` constraint
/// must never let an unmapped key column silently default to NULL even if the
/// per-column flag was not propagated for some reason.
fn column_is_nullable(column: &Column, primary_key_columns: &[String]) -> bool {
    if column.is_rowid_alias {
        return true;
    }
    if column.primary_key {
        return false;
    }
    let in_table_pk = column.name.as_ref().is_some_and(|name| {
        primary_key_columns
            .iter()
            .any(|pk| pk.eq_ignore_ascii_case(name))
    });
    !in_table_pk
}

/// Populates the column registers with values for a single row
#[allow(clippy::too_many_arguments)]
fn populate_column_registers(
    program: &mut ProgramBuilder,
    value: &[Expr],
    column_mappings: &[ColumnMapping],
    column_registers_start: usize,
    rowid_reg: usize,
    resolver: &Resolver,
    primary_key_columns: &[String],
) -> Result<()> {
    for (i, mapping) in column_mappings.iter().enumerate() {
        let target_reg = column_registers_start + i;

        // Column has a value in the VALUES tuple
        if let Some(value_index) = mapping.value_index {
            // When inserting a single row, SQLite writes the value provided for the rowid alias column (INTEGER PRIMARY KEY)
            // directly into the rowid register and writes a NULL into the rowid alias column.
            let write_directly_to_rowid_reg = mapping.column.is_rowid_alias;
            let reg = if write_directly_to_rowid_reg {
                rowid_reg
            } else {
                target_reg
            };
            translate_expr_no_constant_opt(
                program,
                None,
                value.get(value_index).expect("value index out of bounds"),
                reg,
                resolver,
                NoConstantOptReason::RegisterReuse,
            )?;
            if write_directly_to_rowid_reg {
                program.emit_insn(Insn::SoftNull { reg: target_reg });
            }
        } else if let Some(default_expr) = mapping.default_value {
            translate_expr_no_constant_opt(
                program,
                None,
                default_expr,
                target_reg,
                resolver,
                NoConstantOptReason::RegisterReuse,
            )?;
        } else {
            // Column was not specified and has no DEFAULT - use NULL if it is
            // nullable, otherwise error. See `column_is_nullable`: PRIMARY KEY
            // columns (including those named only in a table-level
            // `PRIMARY KEY(...)` clause) are never silently NULL-able, while a
            // rowid-alias column is, because a rowid is autogenerated.
            let is_nullable = column_is_nullable(mapping.column, primary_key_columns);
            if is_nullable {
                program.emit_insn(Insn::Null {
                    dest: target_reg,
                    dest_end: None,
                });
                program.mark_last_insn_constant();
            } else {
                crate::bail_parse_error!(
                    "column {} is not nullable",
                    mapping.column.name.as_ref().expect("column name is None")
                );
            }
        }
    }
    Ok(())
}

// TODO: comeback here later to apply the same improvements on select
//
// Parity note (vtab-INSERT vs. the ordinary BTree INSERT path above): the
// BTree path additionally supports `INSERT INTO t SELECT ...` (a
// SELECT-sourced, not just VALUES-sourced, insert) via a coroutine that
// yields one row at a time from `translate_select`. Virtual tables still
// only support a literal `VALUES` clause (or `DEFAULT VALUES`) here -- that
// gap is real but is a materially larger feature (it needs the same
// coroutine plumbing the BTree path uses, adapted to call `VUpdate` per
// yielded row instead of `Insn::Insert`) and is left as a follow-up rather
// than guessed at. What *is* fixed here: this function used to silently
// process only the LAST row of a multi-row `VALUES` list (`values.pop()`
// discarding every earlier row without error) -- `INSERT INTO vtab VALUES
// (1),(2),(3)` inserted only `(3)`. It now emits one `VUpdate` per row, like
// real SQLite's vtab INSERT codegen (`sqlite3Insert()` looping per row for
// the non-SELECT case) and like this crate's own BTree path's per-row
// `Insn::Insert` emission.
fn translate_virtual_table_insert(
    mut program: ProgramBuilder,
    virtual_table: Rc<VirtualTable>,
    columns: Option<DistinctNames>,
    mut body: InsertBody,
    on_conflict: Option<ResolveType>,
    resolver: &Resolver,
) -> Result<ProgramBuilder> {
    let rows: Vec<Vec<Expr>> = match &mut body {
        InsertBody::Select(select, None) => match select.body.select.as_mut() {
            OneSelect::Values(values) => std::mem::take(values),
            _ => crate::bail_parse_error!("Virtual tables only support VALUES clause in INSERT"),
        },
        InsertBody::DefaultValues => vec![vec![]],
        InsertBody::Select(_, Some(_)) => {
            crate::bail_parse_error!("ON CONFLICT is not supported for virtual tables")
        }
    };
    // All rows in a single VALUES clause must share the same arity (real
    // SQLite rejects a mismatch with "all VALUES must have the same number of
    // terms"). `column_mappings` is derived once from the first row's arity
    // and then reused for every row below, so a later row with a different
    // arity would otherwise silently desync from `column_mappings` and hit
    // the `.expect("value index out of bounds")` in `populate_column_registers`.
    // Guard it explicitly instead, with a real error message.
    let num_values = rows.first().map_or(0, |row| row.len());
    if let Some(mismatched) = rows.iter().find(|row| row.len() != num_values) {
        crate::bail_parse_error!(
            "all VALUES must have the same number of terms (expected {}, got {})",
            num_values,
            mismatched.len()
        );
    }
    let table = Table::Virtual(virtual_table.clone());
    let column_mappings = resolve_columns_for_insert(&table, &columns, num_values)?;
    let conflict_action = on_conflict.as_ref().map(|c| c.bit_value()).unwrap_or(0) as u16;
    let cursor_id = program.alloc_cursor_id(CursorType::VirtualTable(virtual_table.clone()));

    /* *
     * Inserts for virtual tables are done in a single VUpdate step per row.
     * argv[0] = (NULL for insert)
     * argv[1] = (NULL for insert)
     * argv[2..] = column values
     * */
    for row in &rows {
        let registers_start = program.alloc_registers(2);

        program.emit_insn(Insn::Null {
            dest: registers_start,
            dest_end: Some(registers_start + 1),
        });

        let values_reg = program.alloc_registers(column_mappings.len());
        // Virtual tables have no rowid-based PRIMARY KEY bookkeeping here; the module
        // performs its own constraint handling via VUpdate.
        populate_column_registers(
            &mut program,
            row,
            &column_mappings,
            values_reg,
            registers_start,
            resolver,
            &[],
        )?;

        program.emit_insn(Insn::VUpdate {
            cursor_id,
            arg_count: column_mappings.len() + 2,
            start_reg: registers_start,
            conflict_action,
        });
    }

    let halt_label = program.allocate_label();
    program.resolve_label(halt_label, program.offset());

    Ok(program)
}

/// Translate an INSERT into a WITHOUT ROWID table.
///
/// WITHOUT ROWID tables use an index-format B-Tree where:
/// - The key is the PRIMARY KEY columns packed into an index record (no trailing rowid).
/// - The payload/record is the FULL row stored in column declaration order.
///
/// Constraint (validated at CREATE TABLE time): PK column(s) must be the first
/// declared column(s) so that key comparison (on the first pk_count values of the
/// record) works correctly.
///
/// Implementation:
/// 1. Open the table cursor as `CursorType::BTreeIndex` with a synthetic index
///    whose columns = the PK columns and `has_rowid = false`.
/// 2. Populate column registers with the provided values (same as the regular path).
/// 3. Check uniqueness: `NoConflict` on the PK columns (like a unique index check).
/// 4. Write: `MakeRecord` of ALL columns, then `IdxInsert`.
#[allow(clippy::too_many_arguments)]
fn translate_insert_without_rowid(
    mut program: ProgramBuilder,
    schema: &Schema,
    btree_table: &Rc<BTreeTable>,
    columns: Option<DistinctNames>,
    mut body: InsertBody,
    on_conflict: Option<ResolveType>,
    resolver: &mut Resolver,
    on_conflict_needs_savepoint: bool,
) -> Result<ProgramBuilder> {
    use crate::schema::Index;

    // Precedence: an explicit statement-level `INSERT OR <action>` wins if
    // present; otherwise fall back to the PRIMARY KEY constraint's own
    // `ON CONFLICT <action>` (defaults to ABORT — see
    // `BTreeTable::primary_key_conflict`). A WITHOUT ROWID table's implicit PK
    // index IS the table itself, so this is the only conflict site here.
    let effective_pk_conflict = on_conflict.unwrap_or(btree_table.primary_key_conflict);
    let on_conflict_ignore = matches!(effective_pk_conflict, ResolveType::Ignore);
    let on_conflict_replace = matches!(effective_pk_conflict, ResolveType::Replace);
    let table_name = btree_table.name.clone();

    // Build the synthetic "PK index" whose columns = PK columns, has_rowid = false.
    // This is the same representation used at read time so the cursor initialises the
    // right IndexKeyInfo (sort order, num_cols = pk_count, has_rowid = false).
    let synthetic_idx = Index::synthetic_for_without_rowid(btree_table);
    let root_page = btree_table.root_page;
    // `synthetic_idx.columns` contains ALL table columns (needed for emit_column lookup),
    // so we must derive the PK count from the table's primary_key_columns list, not from
    // the synthetic index's column count.
    let num_pk_cols = btree_table.primary_key_columns.len();
    let num_cols = btree_table.columns.len();

    // Primary-key column names (normalised) for NOT NULL enforcement.
    let primary_key_columns: Vec<String> = btree_table
        .primary_key_columns
        .iter()
        .map(|(name, _)| normalize_ident(name))
        .collect();

    // Determine num_values from the body before any mutation.
    let num_values_hint = match &body {
        InsertBody::Select(sel, _) => match sel.body.select.as_ref() {
            OneSelect::Values(vals) if !vals.is_empty() => vals[0].len(),
            _ => num_cols,
        },
        InsertBody::DefaultValues => 0,
    };

    // Resolve INSERT columns → table column mapping (same helper as the rowid path).
    let table_ref = crate::schema::Table::BTree(btree_table.clone());
    let column_mappings = resolve_columns_for_insert(&table_ref, &columns, num_values_hint)?;

    // Detect single vs multi-row and extract values for the single-row case.
    let mut values: Option<Vec<Expr>> = None;
    let inserting_multiple_rows = match &mut body {
        InsertBody::Select(select, _) => match select.body.select.as_mut() {
            OneSelect::Values(values_expr) if values_expr.len() <= 1 => {
                if values_expr.is_empty() {
                    crate::bail_parse_error!("no values to insert");
                }
                let mut param_idx = 1;
                for expr in values_expr.iter_mut().flat_map(|v| v.iter_mut()) {
                    super::optimizer::rewrite_expr(expr, &mut param_idx)?;
                }
                values = values_expr.pop();
                false
            }
            _ => true,
        },
        InsertBody::DefaultValues => false,
    };
    let needs_stmt_savepoint = on_conflict_needs_savepoint && inserting_multiple_rows;

    // --- Open the cursor as BTreeIndex ---
    let cursor_id = program.alloc_cursor_id(CursorType::BTreeIndex(synthetic_idx.clone()));

    let halt_label = program.allocate_label();
    let loop_start_label = program.allocate_label();

    // Allocate column registers.  For WITHOUT ROWID, all columns share consecutive
    // registers starting at `col_regs_start`.  There is no separate rowid register.
    let col_regs_start = program.alloc_registers(num_cols);

    // Multi-row path: emit the coroutine that yields rows, then open cursor.
    let yield_reg_opt: Option<usize> = if inserting_multiple_rows {
        let InsertBody::Select(select, _) = body else {
            unreachable!("inserting_multiple_rows is only true for Select body");
        };
        let yield_reg = program.alloc_register();
        let jump_on_definition_label = program.allocate_label();
        let start_offset_label = program.allocate_label();
        program.emit_insn(Insn::InitCoroutine {
            yield_reg,
            jump_on_definition: jump_on_definition_label,
            start_offset: start_offset_label,
        });
        program.preassign_label_to_next_insn(start_offset_label);
        let query_destination = QueryDestination::CoroutineYield {
            yield_reg,
            coroutine_implementation_start: halt_label,
        };
        program.incr_nesting();
        let result = translate_select(
            QueryMode::Normal,
            schema,
            *select,
            resolver.symbol_table,
            program,
            query_destination,
        )?;
        program = result.program;
        program.decr_nesting();
        program.emit_insn(Insn::EndCoroutine { yield_reg });
        program.preassign_label_to_next_insn(jump_on_definition_label);
        if needs_stmt_savepoint {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::Begin,
                name: "_stmt".to_string(),
            });
        }
        program.emit_insn(Insn::OpenWrite {
            cursor_id,
            root_page: RegisterOrLiteral::Literal(root_page),
            name: table_name.clone(),
        });
        program.preassign_label_to_next_insn(loop_start_label);
        program.emit_insn(Insn::Yield {
            yield_reg,
            end_offset: halt_label,
        });
        Some(yield_reg)
    } else {
        // Single-row path: open cursor first.
        if needs_stmt_savepoint {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::Begin,
                name: "_stmt".to_string(),
            });
        }
        program.emit_insn(Insn::OpenWrite {
            cursor_id,
            root_page: RegisterOrLiteral::Literal(root_page),
            name: table_name.clone(),
        });
        None
    };

    // Populate column registers.
    if let Some(yield_reg) = yield_reg_opt {
        // Multi-row: copy values from the coroutine's yield registers.
        populate_columns_multiple_rows(
            &mut program,
            &column_mappings,
            col_regs_start,
            yield_reg + 1,
            resolver,
            &None,
            &primary_key_columns,
        )?;
    } else {
        // Single-row: evaluate expressions directly.
        // `rowid_reg` is unused for WITHOUT ROWID; pass `col_regs_start` as a dummy.
        populate_column_registers(
            &mut program,
            &values.unwrap_or_default(),
            &column_mappings,
            col_regs_start,
            col_regs_start,
            resolver,
            &primary_key_columns,
        )?;
    }

    // NOT NULL enforcement: PK columns in WITHOUT ROWID tables are implicitly NOT NULL.
    for (idx, col) in btree_table.columns.iter().enumerate() {
        let col_name = col.name.as_deref().unwrap_or("");
        let is_pk_col = primary_key_columns
            .iter()
            .any(|pk| pk == &normalize_ident(col_name));
        let is_notnull = col.notnull || is_pk_col;
        if is_notnull && !col.is_rowid_alias {
            program.emit_insn(Insn::HaltIfNull {
                target_reg: col_regs_start + idx,
                err_code: SQLITE_CONSTRAINT_NOTNULL,
                description: format!("{}.{}", table_name, col_name),
            });
        }
    }

    // --- Conflict check: NoConflict on PK columns ---
    // `NoConflict` jumps to `no_conflict_label` when there is NO conflict
    // (or any PK column is NULL — impossible due to the NOT NULL check above,
    // but the instruction handles it gracefully by jumping).
    let no_conflict_label = program.allocate_label();
    program.emit_insn(Insn::NoConflict {
        cursor_id,
        target_pc: no_conflict_label,
        record_reg: col_regs_start,
        num_regs: num_pk_cols,
    });

    // A conflict was found — handle according to on_conflict policy.
    let next_record_label = program.allocate_label();
    if on_conflict_ignore {
        program.emit_insn(Insn::Goto {
            target_pc: next_record_label,
        });
    } else if on_conflict_replace {
        // Delete the conflicting row (cursor is positioned on it by NoConflict fall-through).
        program.emit_insn(Insn::Delete { cursor_id });
        // The cursor is now invalidated; re-seek so IdxInsert starts from the right position.
        let reseek_label = program.allocate_label();
        program.emit_insn(Insn::SeekGE {
            is_index: true,
            cursor_id,
            start_reg: col_regs_start,
            num_regs: num_pk_cols,
            target_pc: reseek_label,
            eq_only: false,
        });
        program.preassign_label_to_next_insn(reseek_label);
    } else {
        // ABORT / FAIL / ROLLBACK: halt with PRIMARY KEY constraint error.
        if needs_stmt_savepoint {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::RollbackTo,
                name: "_stmt".to_string(),
            });
        }
        let pk_desc = btree_table
            .primary_key_columns
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        program.emit_insn(Insn::Halt {
            err_code: SQLITE_CONSTRAINT_PRIMARYKEY,
            description: format!("{}.{}", table_name, pk_desc),
        });
    }
    program.preassign_label_to_next_insn(no_conflict_label);

    // --- Build the full-row record and insert ---
    // The record contains ALL table columns in declaration order (PK cols first
    // per the validation constraint, then remaining cols).
    let record_reg = program.alloc_register();
    program.emit_insn(Insn::MakeRecord {
        start_reg: col_regs_start,
        count: num_cols,
        dest_reg: record_reg,
        index_name: Some(format!("__without_rowid_pk_{}", table_name)),
    });
    // Seek-skip: unlike the rowid-table path (which juggles several distinct
    // index cursors), WITHOUT ROWID has a single cursor that is both the
    // conflict-check target AND the insert target. Every path that reaches
    // this point leaves it correctly positioned for the insert:
    // - the `NoConflict` jump (no existing conflicting key) positions it
    //   directly, or
    // - the `Replace` path explicitly re-seeks via `SeekGE` right above after
    //   deleting the conflicting row (see the comment there).
    // `Ignore` and Abort/Fail/Rollback never reach this instruction at all
    // (they `Goto`/`Halt` away above). So this is unconditionally safe here.
    program.emit_insn(Insn::IdxInsert {
        cursor_id,
        record_reg,
        unpacked_start: Some(col_regs_start),
        unpacked_count: Some(num_cols as u16),
        flags: IdxInsertFlags::new().use_seek(true),
    });

    // IGNORE lands here after skipping the insert.
    program.preassign_label_to_next_insn(next_record_label);

    if inserting_multiple_rows {
        // Loop back to yield the next row from the coroutine.
        program.emit_insn(Insn::Goto {
            target_pc: loop_start_label,
        });
    }

    program.resolve_label(halt_label, program.offset());
    if needs_stmt_savepoint {
        program.emit_insn(Insn::Savepoint {
            op: SavepointOp::Release,
            name: "_stmt".to_string(),
        });
    }
    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}

mod tests;
