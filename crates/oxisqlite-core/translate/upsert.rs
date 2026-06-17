/// VDBE bytecode emitter for ON CONFLICT DO UPDATE (upsert Slice 2).
///
/// This module contains `emit_upsert_do_update`, the only non-trivial helper
/// that is too large to live inline in `translate/insert.rs` without pushing
/// that file past the 2000-line workspace policy.
use limbo_sqlite3_parser::ast::{Expr, Id, Name, Set};

use crate::schema::{BTreeTable, Schema};
use crate::translate::emitter::Resolver;
use crate::translate::expr::translate_expr;
use crate::vdbe::builder::ProgramBuilder;
use crate::vdbe::insn::{CmpInsFlags, IdxInsertFlags, InsertFlags, Insn, SavepointOp};
use crate::vdbe::BranchOffset;
use crate::Result;

/// Emit the VDBE bytecode that performs the DO UPDATE rewrite for an upsert.
///
/// Preconditions:
/// - The table cursor (`cursor_id`) is positioned on the victim row.
/// - `column_registers_start` + column_index holds the incoming INSERT values
///   (used for `excluded.*` references).
/// - `rowid_reg` holds the new rowid (used for `excluded.<rowid_alias_col>`).
///
/// On completion the routine jumps to `next_record_label` (the row has been
/// handled — either updated or WHERE-skipped).
#[allow(clippy::too_many_arguments)]
pub fn emit_upsert_do_update(
    program: &mut ProgramBuilder,
    schema: &Schema,
    table_name: &str,
    btree_table: &BTreeTable,
    cursor_id: usize,
    column_registers_start: usize,
    rowid_reg: usize,
    num_cols: usize,
    rowid_alias_index: Option<usize>,
    sets: &[Set],
    where_clause: Option<&Expr>,
    next_record_label: BranchOffset,
    needs_stmt_savepoint: bool,
    idx_cursors: &[(&String, usize, usize)],
    resolver: &mut Resolver,
) -> Result<()> {
    // ------------------------------------------------------------------
    // Step 1: Read OLD values from victim cursor (already positioned).
    // ------------------------------------------------------------------
    let old_start = program.alloc_registers(num_cols);
    for (i, col) in btree_table.columns.iter().enumerate() {
        if col.is_rowid_alias {
            // Rowid-alias slot is always NULL in the record.
            program.emit_null(old_start + i, None);
        } else {
            program.emit_column(cursor_id, i, old_start + i);
        }
    }
    let old_rowid_reg = program.alloc_register();
    program.emit_insn(Insn::RowId {
        cursor_id,
        dest: old_rowid_reg,
    });

    // ------------------------------------------------------------------
    // Step 2: Populate resolver overrides.
    // ------------------------------------------------------------------
    resolver.upsert_reg_overrides.clear();

    for (i, col) in btree_table.columns.iter().enumerate() {
        let col_name = match col.name.as_deref() {
            Some(n) => n,
            None => continue,
        };

        // Bare column name → old value (for "n = n + 1" style).
        resolver
            .upsert_reg_overrides
            .push((Expr::Id(Id(col_name.to_string())), old_start + i));

        // Table-qualified → old value.
        resolver.upsert_reg_overrides.push((
            Expr::Qualified(Name(table_name.to_string()), Name(col_name.to_string())),
            old_start + i,
        ));

        // excluded.col → new INSERT value.
        let excluded_reg = if col.is_rowid_alias {
            rowid_reg
        } else {
            column_registers_start + i
        };
        resolver.upsert_reg_overrides.push((
            Expr::Qualified(Name("excluded".to_string()), Name(col_name.to_string())),
            excluded_reg,
        ));
    }

    // Rowid aliases → old rowid.
    for alias in &["rowid", "oid", "_rowid_"] {
        resolver
            .upsert_reg_overrides
            .push((Expr::Id(Id((*alias).to_string())), old_rowid_reg));
    }

    // ------------------------------------------------------------------
    // Step 3: DO UPDATE WHERE guard.
    // ------------------------------------------------------------------
    if let Some(where_expr) = where_clause {
        let cond_reg = program.alloc_register();
        translate_expr(program, None, where_expr, cond_reg, resolver)?;
        program.emit_insn(Insn::IfNot {
            reg: cond_reg,
            target_pc: next_record_label,
            jump_if_null: true,
        });
    }

    // ------------------------------------------------------------------
    // Step 4: Build SET column mapping.
    // set_exprs[i] = Some(expr) if column i is SET, None if unchanged.
    // ------------------------------------------------------------------
    let mut set_exprs: Vec<Option<Expr>> = vec![None; num_cols];
    let mut new_rowid_expr: Option<Expr> = None;

    for set in sets {
        for col_name_struct in set.col_names.iter() {
            let col_name_norm = col_name_struct.0.to_lowercase();
            let found = btree_table.columns.iter().enumerate().find(|(_, col)| {
                col.name
                    .as_deref()
                    .map_or(false, |n| n.eq_ignore_ascii_case(&col_name_norm))
            });
            match found {
                Some((col_idx, col)) if col.is_rowid_alias => {
                    new_rowid_expr = Some(set.expr.clone());
                    let _ = col_idx;
                }
                Some((col_idx, _)) => {
                    set_exprs[col_idx] = Some(set.expr.clone());
                }
                None => {
                    // TODO: reject SET on generated columns once schema.Column exposes an is_generated flag
                    crate::bail_parse_error!(
                        "no such column in DO UPDATE SET: {}",
                        col_name_struct.0
                    );
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Step 5: Build new row into `new_start` registers.
    // ------------------------------------------------------------------
    let new_start = program.alloc_registers(num_cols);
    let new_rowid_reg_local = program.alloc_register();

    // Default: copy old rowid.
    program.emit_insn(Insn::Copy {
        src_reg: old_rowid_reg,
        dst_reg: new_rowid_reg_local,
        amount: 0,
    });

    for i in 0..num_cols {
        let col = &btree_table.columns[i];
        if col.is_rowid_alias {
            // Always NULL in the record regardless of SET.
            program.emit_null(new_start + i, None);
        } else if let Some(ref expr) = set_exprs[i].clone() {
            translate_expr(program, None, expr, new_start + i, resolver)?;
            // NOT NULL constraint check.
            if col.notnull {
                program.emit_insn(Insn::HaltIfNull {
                    target_reg: new_start + i,
                    err_code: crate::error::SQLITE_CONSTRAINT_NOTNULL,
                    description: format!(
                        "{}.{}",
                        table_name,
                        col.name.as_ref().expect("column name")
                    ),
                });
            }
        } else {
            // Unchanged — copy old value.
            program.emit_insn(Insn::Copy {
                src_reg: old_start + i,
                dst_reg: new_start + i,
                amount: 0,
            });
        }
    }

    // ------------------------------------------------------------------
    // Step 6: If rowid alias was SET, translate the new rowid expression.
    // ------------------------------------------------------------------
    if let Some(ref rowid_expr) = new_rowid_expr.clone() {
        translate_expr(program, None, rowid_expr, new_rowid_reg_local, resolver)?;
        program.emit_insn(Insn::MustBeInt {
            reg: new_rowid_reg_local,
        });
        // If new_rowid == old_rowid, skip conflict check.
        let rowid_unchanged_label = program.allocate_label();
        let rowid_ok_label = program.allocate_label();
        let eq_lhs = program.alloc_register();
        let eq_rhs = program.alloc_register();
        program.emit_insn(Insn::Copy {
            src_reg: new_rowid_reg_local,
            dst_reg: eq_lhs,
            amount: 0,
        });
        program.emit_insn(Insn::Copy {
            src_reg: old_rowid_reg,
            dst_reg: eq_rhs,
            amount: 0,
        });
        program.emit_insn(Insn::Eq {
            lhs: eq_lhs,
            rhs: eq_rhs,
            target_pc: rowid_unchanged_label,
            flags: CmpInsFlags::default(),
            collation: None,
        });
        // Rowid changed: verify no conflict.
        program.emit_insn(Insn::NotExists {
            cursor: cursor_id,
            rowid_reg: new_rowid_reg_local,
            target_pc: rowid_ok_label,
        });
        // Conflict with new rowid.
        if needs_stmt_savepoint {
            program.emit_insn(Insn::Savepoint {
                op: SavepointOp::RollbackTo,
                name: "_stmt".to_string(),
            });
        }
        program.emit_insn(Insn::Halt {
            err_code: crate::error::SQLITE_CONSTRAINT_PRIMARYKEY,
            description: format!("{}.rowid", table_name),
        });
        program.preassign_label_to_next_insn(rowid_unchanged_label);
        program.preassign_label_to_next_insn(rowid_ok_label);
    }

    // rowid_alias_index was used to determine excluded_reg in Step 2.
    // The value is embedded in the overrides; suppress the unused-variable warning.
    let _ = rowid_alias_index;

    // ------------------------------------------------------------------
    // Step 7: Index maintenance — delete old index entries.
    // ------------------------------------------------------------------
    for index in schema.get_indices(table_name) {
        let Some((_, _, idx_cursor_id)) =
            idx_cursors.iter().find(|(name, _, _)| **name == index.name)
        else {
            continue;
        };
        let num_regs = index.columns.len() + 1;
        let del_start = program.alloc_registers(num_regs);
        for (reg_offset, idx_col) in index.columns.iter().enumerate() {
            program.emit_insn(Insn::Copy {
                src_reg: old_start + idx_col.pos_in_table,
                dst_reg: del_start + reg_offset,
                amount: 0,
            });
        }
        program.emit_insn(Insn::Copy {
            src_reg: old_rowid_reg,
            dst_reg: del_start + num_regs - 1,
            amount: 0,
        });
        program.emit_insn(Insn::IdxDelete {
            start_reg: del_start,
            num_regs,
            cursor_id: *idx_cursor_id,
        });
    }

    // ------------------------------------------------------------------
    // Step 8: Rewrite the table row (Delete + Insert).
    // ------------------------------------------------------------------
    let record_reg = program.alloc_register();
    program.emit_insn(Insn::MakeRecord {
        start_reg: new_start,
        count: num_cols,
        dest_reg: record_reg,
        index_name: None,
    });
    // Re-seek cursor before delete (cursor may have moved during column reads).
    let reseek_label = program.allocate_label();
    program.emit_insn(Insn::SeekRowid {
        cursor_id,
        src_reg: old_rowid_reg,
        target_pc: reseek_label,
    });
    program.preassign_label_to_next_insn(reseek_label);
    // Delete old row (cursor is positioned on it).
    program.emit_insn(Insn::Delete { cursor_id });
    // Insert new row.
    program.emit_insn(Insn::Insert {
        cursor: cursor_id,
        key_reg: new_rowid_reg_local,
        record_reg,
        flag: InsertFlags::new().update(true),
        table_name: table_name.to_string(),
    });

    // ------------------------------------------------------------------
    // Step 9: Insert new index entries.
    // ------------------------------------------------------------------
    for index in schema.get_indices(table_name) {
        let Some((_, _, idx_cursor_id)) =
            idx_cursors.iter().find(|(name, _, _)| **name == index.name)
        else {
            continue;
        };
        let num_idx_cols = index.columns.len();
        let ins_start = program.alloc_registers(num_idx_cols + 1);
        for (reg_offset, idx_col) in index.columns.iter().enumerate() {
            program.emit_insn(Insn::Copy {
                src_reg: new_start + idx_col.pos_in_table,
                dst_reg: ins_start + reg_offset,
                amount: 0,
            });
        }
        program.emit_insn(Insn::Copy {
            src_reg: new_rowid_reg_local,
            dst_reg: ins_start + num_idx_cols,
            amount: 0,
        });
        let idx_record_reg = program.alloc_register();
        program.emit_insn(Insn::MakeRecord {
            start_reg: ins_start,
            count: num_idx_cols + 1,
            dest_reg: idx_record_reg,
            index_name: None,
        });
        program.emit_insn(Insn::IdxInsert {
            cursor_id: *idx_cursor_id,
            record_reg: idx_record_reg,
            unpacked_start: Some(ins_start),
            unpacked_count: Some((num_idx_cols + 1) as u16),
            flags: IdxInsertFlags::new(),
        });
    }

    // ------------------------------------------------------------------
    // Step 10: Clear overrides and jump to next_record_label.
    // ------------------------------------------------------------------
    resolver.upsert_reg_overrides.clear();
    program.emit_insn(Insn::Goto {
        target_pc: next_record_label,
    });

    Ok(())
}
