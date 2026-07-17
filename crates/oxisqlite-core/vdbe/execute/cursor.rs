#![allow(unused_variables)]
use super::super::insn::RegisterOrLiteral;
use super::super::{get_new_rowid, make_record, Program, ProgramState, Register};
use crate::pseudo::PseudoCursor;
use crate::storage::database::FileMemoryStorage;
use crate::storage::page_cache::DumbLruPageCache;
use crate::storage::pager::CreateBTreeFlags;
use crate::storage::wal::DummyWAL;
use crate::types::ImmutableRecord;
use crate::{bail_constraint_error, must_be_btree_cursor, MvStore, Pager, Result};
use crate::{
    error::{LimboError, SQLITE_CONSTRAINT},
    types::compare_immutable,
};
use crate::{maybe_init_database_file, BufferPool, MvCursor, OpenFlags, RefValue, Row, IO};
use crate::{schema::Affinity, storage::btree::BTreeCursor};
use crate::{
    types::{Cursor, CursorResult, SeekKey, SeekOp, Value, ValueType},
    vdbe::{builder::CursorType, insn::Insn},
};
use parking_lot::RwLock;
use rand::rng as thread_rng;
use std::cell::RefCell;
use std::{rc::Rc, sync::Arc};

use super::numeric::{apply_affinity_char, apply_numeric_affinity, extract_int_value};
use super::InsnFunctionStepResult;

pub fn op_open_read(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::OpenRead {
        cursor_id,
        root_page,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let (_, cursor_type) = program.cursor_ref.get(*cursor_id).unwrap();
    let mv_cursor = match state.mv_tx_id {
        Some(tx_id) => {
            let table_id = *root_page as u64;
            let mv_store = mv_store.unwrap().clone();
            let mv_cursor = Rc::new(RefCell::new(
                MvCursor::new(mv_store.clone(), tx_id, table_id).unwrap(),
            ));
            Some(mv_cursor)
        }
        None => None,
    };
    let mut cursors = state.cursors.borrow_mut();
    match cursor_type {
        CursorType::BTreeTable(_) => {
            let cursor = BTreeCursor::new_table(mv_cursor, pager.clone(), *root_page);
            cursors
                .get_mut(*cursor_id)
                .unwrap()
                .replace(Cursor::new_btree(cursor));
        }
        CursorType::BTreeIndex(index) => {
            let conn = program.connection.clone();
            let schema = conn.schema.try_read().ok_or(LimboError::SchemaLocked)?;
            let table = schema
                .get_table(&index.table_name)
                .map_or(None, |table| table.btree());
            let collations = table.map_or(Vec::new(), |table| {
                index
                    .columns
                    .iter()
                    .map(|c| {
                        table
                            .columns
                            .get(c.pos_in_table)
                            .unwrap()
                            .collation
                            .unwrap_or_default()
                    })
                    .collect()
            });
            let cursor = BTreeCursor::new_index(
                mv_cursor,
                pager.clone(),
                *root_page,
                index.as_ref(),
                collations,
            );
            cursors
                .get_mut(*cursor_id)
                .unwrap()
                .replace(Cursor::new_btree(cursor));
        }
        CursorType::Pseudo(_) => {
            panic!("OpenRead on pseudo cursor");
        }
        CursorType::Sorter => {
            panic!("OpenRead on sorter cursor");
        }
        CursorType::VirtualTable(_) => {
            panic!("OpenRead on virtual table cursor, use Insn:VOpen instead");
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_open_pseudo(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::OpenPseudo {
        cursor_id,
        content_reg: _,
        num_fields: _,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    {
        let mut cursors = state.cursors.borrow_mut();
        let cursor = PseudoCursor::new();
        cursors
            .get_mut(*cursor_id)
            .unwrap()
            .replace(Cursor::new_pseudo(cursor));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_rewind(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Rewind {
        cursor_id,
        pc_if_empty,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(pc_if_empty.is_offset());
    let is_empty = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Rewind");
        let cursor = cursor.as_btree_mut();
        return_if_io!(cursor.rewind());
        cursor.is_empty()
    };
    if is_empty {
        state.pc = pc_if_empty.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_last(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Last {
        cursor_id,
        pc_if_empty,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(pc_if_empty.is_offset());
    let is_empty = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Last");
        let cursor = cursor.as_btree_mut();
        return_if_io!(cursor.last());
        cursor.is_empty()
    };
    if is_empty {
        state.pc = pc_if_empty.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_column(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Column {
        cursor_id,
        column,
        dest,
        default,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Some((index_cursor_id, table_cursor_id)) = state.deferred_seeks[*cursor_id].take() {
        let deferred_seek = 'd: {
            let rowid = {
                let mut index_cursor = state.get_cursor(index_cursor_id);
                let index_cursor = index_cursor.as_btree_mut();
                match index_cursor.rowid()? {
                    CursorResult::IO => {
                        break 'd Some((index_cursor_id, table_cursor_id));
                    }
                    CursorResult::Ok(rowid) => rowid,
                }
            };
            let Some(rowid) = rowid else {
                let (num_indexed_cols, pk_record) = {
                    let (_, index_cursor_type) = program
                        .cursor_ref
                        .get(index_cursor_id)
                        .expect("index cursor must be registered");
                    let num_indexed_cols = if let CursorType::BTreeIndex(idx) = index_cursor_type {
                        idx.columns.len()
                    } else {
                        break 'd None;
                    };
                    let mut io_pending = false;
                    let pk_regs: Vec<Register> = {
                        let mut idx_cur = state.get_cursor(index_cursor_id);
                        let idx_cur = idx_cur.as_btree_mut();
                        let record_result = idx_cur.record()?;
                        let regs = match record_result {
                            CursorResult::IO => {
                                io_pending = true;
                                Vec::new()
                            }
                            CursorResult::Ok(record_opt) => {
                                if let Some(record) = record_opt {
                                    record
                                        .values
                                        .iter()
                                        .skip(num_indexed_cols)
                                        .map(|rv| Register::Value(rv.to_owned()))
                                        .collect()
                                } else {
                                    Vec::new()
                                }
                            }
                        };
                        regs
                    };
                    if io_pending {
                        break 'd Some((index_cursor_id, table_cursor_id));
                    }
                    if pk_regs.is_empty() {
                        break 'd None;
                    }
                    let pk_record = ImmutableRecord::from_registers(&pk_regs);
                    (num_indexed_cols, pk_record)
                };
                let _ = num_indexed_cols;
                let mut table_cursor = state.get_cursor(table_cursor_id);
                let table_cursor = table_cursor.as_btree_mut();
                match table_cursor
                    .seek(SeekKey::IndexKey(&pk_record), SeekOp::GE { eq_only: true })?
                {
                    CursorResult::Ok(_) => break 'd None,
                    CursorResult::IO => break 'd Some((index_cursor_id, table_cursor_id)),
                }
            };
            let mut table_cursor = state.get_cursor(table_cursor_id);
            let table_cursor = table_cursor.as_btree_mut();
            match table_cursor.seek(SeekKey::TableRowId(rowid), SeekOp::GE { eq_only: true })? {
                CursorResult::Ok(_) => None,
                CursorResult::IO => Some((index_cursor_id, table_cursor_id)),
            }
        };
        if let Some(deferred_seek) = deferred_seek {
            state.deferred_seeks[*cursor_id] = Some(deferred_seek);
            return Ok(InsnFunctionStepResult::IO);
        }
    }
    let (_, cursor_type) = program.cursor_ref.get(*cursor_id).unwrap();
    match cursor_type {
        CursorType::BTreeTable(_) | CursorType::BTreeIndex(_) => {
            let value = 'value: {
                let mut cursor =
                    must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Column");
                let cursor = cursor.as_btree_mut();
                let record = return_if_io!(cursor.record());
                let Some(record) = record.as_ref() else {
                    break 'value Value::Null;
                };
                if cursor.get_null_flag() {
                    break 'value Value::Null;
                }
                if let Some(value) = record.get_value_opt(*column) {
                    break 'value value.to_owned();
                }
                default.clone().unwrap_or(Value::Null)
            };
            match (&value, &mut state.registers[*dest]) {
                (Value::Text(text_ref), Register::Value(Value::Text(text_reg))) => {
                    text_reg.value.clear();
                    text_reg.value.extend_from_slice(text_ref.value.as_slice());
                }
                (Value::Blob(raw_slice), Register::Value(Value::Blob(blob_reg))) => {
                    blob_reg.clear();
                    blob_reg.extend_from_slice(raw_slice.as_slice());
                }
                _ => {
                    let reg = &mut state.registers[*dest];
                    *reg = Register::Value(value);
                }
            }
        }
        CursorType::Sorter => {
            let record = {
                let mut cursor = state.get_cursor(*cursor_id);
                let cursor = cursor.as_sorter_mut();
                cursor.record().cloned()
            };
            if let Some(record) = record {
                state.registers[*dest] = Register::Value(match record.get_value_opt(*column) {
                    Some(val) => val.to_owned(),
                    None => default.clone().unwrap_or(Value::Null),
                });
            } else {
                state.registers[*dest] = Register::Value(Value::Null);
            }
        }
        CursorType::Pseudo(_) => {
            let value = {
                let mut cursor = state.get_cursor(*cursor_id);
                let cursor = cursor.as_pseudo_mut();
                if let Some(record) = cursor.record() {
                    record.get_value(*column).to_owned()
                } else {
                    Value::Null
                }
            };
            state.registers[*dest] = Register::Value(value);
        }
        CursorType::VirtualTable(_) => {
            panic!("Insn:Column on virtual table cursor, use Insn:VColumn instead");
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_type_check(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::TypeCheck {
        start_reg,
        count,
        check_generated,
        table_reference,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert_eq!(table_reference.is_strict, true);
    state.registers[*start_reg..*start_reg + *count]
        .iter_mut()
        .zip(table_reference.columns.iter())
        .try_for_each(|(reg, col)| {
            if !col.is_rowid_alias
                && col.primary_key
                && matches!(reg.get_owned_value(), Value::Null)
            {
                bail_constraint_error!(
                    "NOT NULL constraint failed: {}.{} ({})",
                    &table_reference.name,
                    col.name.as_ref().map(|s| s.as_str()).unwrap_or(""),
                    SQLITE_CONSTRAINT
                )
            } else if col.is_rowid_alias && matches!(reg.get_owned_value(), Value::Null) {
                return Ok(());
            }
            let col_affinity = col.affinity();
            let ty_str = col.ty_str.as_str();
            let applied = apply_affinity_char(reg, col_affinity);
            let value_type = reg.get_owned_value().value_type();
            match (ty_str, value_type) {
                ("INTEGER" | "INT", ValueType::Integer) => {}
                ("REAL", ValueType::Float) => {}
                ("BLOB", ValueType::Blob) => {}
                ("TEXT", ValueType::Text) => {}
                ("ANY", _) => {}
                (t, v) => {
                    bail_constraint_error!(
                        "cannot store {} value in {} column {}.{} ({})",
                        v,
                        t,
                        &table_reference.name,
                        col.name.as_ref().map(|s| s.as_str()).unwrap_or(""),
                        SQLITE_CONSTRAINT
                    )
                }
            };
            Ok(())
        })?;
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_make_record(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::MakeRecord {
        start_reg,
        count,
        dest_reg,
        ..
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let record = make_record(&state.registers, start_reg, count);
    state.registers[*dest_reg] = Register::Record(record);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_result_row(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ResultRow { start_reg, count } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let row = Row {
        // SAFETY-relevant: `Row::get`/`get_value`/`get_values` index/slice up to `count`
        // elements starting at `values`, so the pointer's provenance must cover the whole
        // `count`-element range, not just the single `Register` at `start_reg`. Slicing here
        // (rather than `&state.registers[*start_reg] as *const Register`) is what grants that
        // provenance under Stacked Borrows.
        values: state.registers[*start_reg..*start_reg + *count].as_ptr(),
        count: *count,
    };
    state.result_row = Some(row);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Row)
}
pub fn op_next(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Next {
        cursor_id,
        pc_if_next,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(pc_if_next.is_offset());
    let is_empty = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Next");
        let cursor = cursor.as_btree_mut();
        cursor.set_null_flag(false);
        return_if_io!(cursor.next());
        cursor.is_empty()
    };
    if !is_empty {
        state.pc = pc_if_next.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_prev(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Prev {
        cursor_id,
        pc_if_prev,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(pc_if_prev.is_offset());
    let is_empty = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Prev");
        let cursor = cursor.as_btree_mut();
        cursor.set_null_flag(false);
        return_if_io!(cursor.prev());
        cursor.is_empty()
    };
    if !is_empty {
        state.pc = pc_if_prev.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_row_id(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::RowId { cursor_id, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Some((index_cursor_id, table_cursor_id)) = state.deferred_seeks[*cursor_id].take() {
        let deferred_seek = 'd: {
            let rowid = {
                let mut index_cursor = state.get_cursor(index_cursor_id);
                let index_cursor = index_cursor.as_btree_mut();
                let record = match index_cursor.record()? {
                    CursorResult::IO => {
                        break 'd Some((index_cursor_id, table_cursor_id));
                    }
                    CursorResult::Ok(record) => record,
                };
                let record = record.as_ref().unwrap();
                let rowid = record.get_values().last().unwrap();
                match rowid {
                    RefValue::Integer(rowid) => *rowid,
                    _ => unreachable!(),
                }
            };
            let mut table_cursor = state.get_cursor(table_cursor_id);
            let table_cursor = table_cursor.as_btree_mut();
            match table_cursor.seek(SeekKey::TableRowId(rowid), SeekOp::GE { eq_only: true })? {
                CursorResult::Ok(_) => None,
                CursorResult::IO => Some((index_cursor_id, table_cursor_id)),
            }
        };
        if let Some(deferred_seek) = deferred_seek {
            state.deferred_seeks[*cursor_id] = Some(deferred_seek);
            return Ok(InsnFunctionStepResult::IO);
        }
    }
    let mut cursors = state.cursors.borrow_mut();
    if let Some(Cursor::BTree(btree_cursor)) = cursors.get_mut(*cursor_id).unwrap() {
        if let Some(ref rowid) = return_if_io!(btree_cursor.rowid()) {
            state.registers[*dest] = Register::Value(Value::Integer(*rowid as i64));
        } else {
            state.registers[*dest] = Register::Value(Value::Null);
        }
    } else if let Some(Cursor::Virtual(virtual_cursor)) = cursors.get_mut(*cursor_id).unwrap() {
        let rowid = virtual_cursor.rowid();
        if rowid != 0 {
            state.registers[*dest] = Register::Value(Value::Integer(rowid));
        } else {
            state.registers[*dest] = Register::Value(Value::Null);
        }
    } else {
        return Err(LimboError::InternalError(
            "RowId: cursor is not a table or virtual cursor".to_string(),
        ));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_row_id(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxRowId { cursor_id, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let mut cursors = state.cursors.borrow_mut();
    let cursor = cursors.get_mut(*cursor_id).unwrap().as_mut().unwrap();
    let cursor = cursor.as_btree_mut();
    let rowid = return_if_io!(cursor.rowid());
    state.registers[*dest] = match rowid {
        Some(rowid) => Register::Value(Value::Integer(rowid as i64)),
        None => Register::Value(Value::Null),
    };
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_seek_rowid(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SeekRowid {
        cursor_id,
        src_reg,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let pc = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        let rowid = match state.registers[*src_reg].get_owned_value() {
            Value::Integer(rowid) => Some(*rowid),
            Value::Null => None,
            other => {
                let mut temp_reg = Register::Value(other.clone());
                let converted = apply_affinity_char(&mut temp_reg, Affinity::Numeric);
                if converted {
                    match temp_reg.get_owned_value() {
                        Value::Integer(i) => Some(*i),
                        Value::Float(f) => Some(*f as i64),
                        _ => {
                            unreachable!(
                                "apply_affinity_char with Numeric should produce an integer if it returns true"
                            )
                        }
                    }
                } else {
                    None
                }
            }
        };
        match rowid {
            Some(rowid) => {
                let found = return_if_io!(
                    cursor.seek(SeekKey::TableRowId(rowid), SeekOp::GE { eq_only: true })
                );
                if !found {
                    target_pc.to_offset_int()
                } else {
                    state.pc + 1
                }
            }
            None => target_pc.to_offset_int(),
        }
    };
    state.pc = pc;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_deferred_seek(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::DeferredSeek {
        index_cursor_id,
        table_cursor_id,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.deferred_seeks[*table_cursor_id] = Some((*index_cursor_id, *table_cursor_id));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_seek(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let (Insn::SeekGE {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
        is_index,
        ..
    }
    | Insn::SeekGT {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
        is_index,
    }
    | Insn::SeekLE {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
        is_index,
        ..
    }
    | Insn::SeekLT {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
        is_index,
    }) = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(
        target_pc.is_offset(),
        "target_pc should be an offset, is: {:?}",
        target_pc
    );
    let eq_only = match insn {
        Insn::SeekGE { eq_only, .. } | Insn::SeekLE { eq_only, .. } => *eq_only,
        _ => false,
    };
    let op = match insn {
        Insn::SeekGE { eq_only, .. } => SeekOp::GE { eq_only: *eq_only },
        Insn::SeekGT { .. } => SeekOp::GT,
        Insn::SeekLE { eq_only, .. } => SeekOp::LE { eq_only: *eq_only },
        Insn::SeekLT { .. } => SeekOp::LT,
        _ => unreachable!("unexpected Insn {:?}", insn),
    };
    let op_name = match op {
        SeekOp::GE { .. } => "SeekGE",
        SeekOp::GT => "SeekGT",
        SeekOp::LE { .. } => "SeekLE",
        SeekOp::LT => "SeekLT",
    };
    if *is_index {
        let found = {
            let mut cursor = state.get_cursor(*cursor_id);
            let cursor = cursor.as_btree_mut();
            let record_from_regs = make_record(&state.registers, start_reg, num_regs);
            let found = return_if_io!(cursor.seek(SeekKey::IndexKey(&record_from_regs), op));
            found
        };
        if !found {
            state.pc = target_pc.to_offset_int();
        } else {
            state.pc += 1;
        }
    } else {
        let pc = {
            let original_value = state.registers[*start_reg].get_owned_value().clone();
            let mut temp_value = original_value.clone();
            let conversion_successful = if matches!(temp_value, Value::Text(_)) {
                let mut temp_reg = Register::Value(temp_value);
                let converted = apply_numeric_affinity(&mut temp_reg, false);
                temp_value = temp_reg.get_owned_value().clone();
                converted
            } else {
                true
            };
            let int_key = extract_int_value(&temp_value);
            let lost_precision = !conversion_successful || !matches!(temp_value, Value::Integer(_));
            let actual_op = if lost_precision {
                match &temp_value {
                    Value::Float(f) => {
                        let int_key_as_float = int_key as f64;
                        let c = if int_key_as_float > *f {
                            1
                        } else if int_key_as_float < *f {
                            -1
                        } else {
                            0
                        };
                        if c > 0 {
                            match op {
                                SeekOp::GT => SeekOp::GE { eq_only: false },
                                SeekOp::LE { .. } => SeekOp::LT,
                                other => other,
                            }
                        } else if c < 0 {
                            match op {
                                SeekOp::LT => SeekOp::LE { eq_only: false },
                                SeekOp::GE { .. } => SeekOp::GT,
                                other => other,
                            }
                        } else {
                            op
                        }
                    }
                    Value::Text(_) | Value::Blob(_) => match op {
                        SeekOp::GT | SeekOp::GE { .. } => {
                            state.pc = target_pc.to_offset_int();
                            return Ok(InsnFunctionStepResult::Step);
                        }
                        SeekOp::LT | SeekOp::LE { .. } => {
                            {
                                let mut cursor = state.get_cursor(*cursor_id);
                                let cursor = cursor.as_btree_mut();
                                return_if_io!(cursor.last());
                            }
                            state.pc += 1;
                            return Ok(InsnFunctionStepResult::Step);
                        }
                    },
                    _ => op,
                }
            } else {
                op
            };
            let rowid = if matches!(original_value, Value::Null) {
                match actual_op {
                    SeekOp::GE { .. } | SeekOp::GT => {
                        state.pc = target_pc.to_offset_int();
                        return Ok(InsnFunctionStepResult::Step);
                    }
                    SeekOp::LE { .. } | SeekOp::LT => {
                        state.pc = target_pc.to_offset_int();
                        return Ok(InsnFunctionStepResult::Step);
                    }
                }
            } else {
                int_key
            };
            let mut cursor = state.get_cursor(*cursor_id);
            let cursor = cursor.as_btree_mut();
            let found = return_if_io!(cursor.seek(SeekKey::TableRowId(rowid), actual_op));
            if !found {
                target_pc.to_offset_int()
            } else {
                state.pc + 1
            }
        };
        state.pc = pc;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_ge(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxGE {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let pc = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        let record_from_regs = make_record(&state.registers, start_reg, num_regs);
        let pc = if let Some(idx_record) = return_if_io!(cursor.record()) {
            let idx_values = idx_record.get_values();
            let idx_values = &idx_values[..record_from_regs.len()];
            let record_values = record_from_regs.get_values();
            let ord = compare_immutable(
                &idx_values,
                &record_values,
                cursor.key_sort_order(),
                cursor.collations(),
            );
            if ord.is_ge() {
                target_pc.to_offset_int()
            } else {
                state.pc + 1
            }
        } else {
            target_pc.to_offset_int()
        };
        pc
    };
    state.pc = pc;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_seek_end(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    if let Insn::SeekEnd { cursor_id } = *insn {
        let mut cursor = state.get_cursor(cursor_id);
        let cursor = cursor.as_btree_mut();
        return_if_io!(cursor.seek_end());
    } else {
        unreachable!("unexpected Insn {:?}", insn)
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_le(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxLE {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let pc = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        let record_from_regs = make_record(&state.registers, start_reg, num_regs);
        let pc = if let Some(ref idx_record) = return_if_io!(cursor.record()) {
            let idx_values = idx_record.get_values();
            let idx_values = &idx_values[..record_from_regs.len()];
            let record_values = record_from_regs.get_values();
            let ord = compare_immutable(
                &idx_values,
                &record_values,
                cursor.key_sort_order(),
                cursor.collations(),
            );
            if ord.is_le() {
                target_pc.to_offset_int()
            } else {
                state.pc + 1
            }
        } else {
            target_pc.to_offset_int()
        };
        pc
    };
    state.pc = pc;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_gt(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxGT {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let pc = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        let record_from_regs = make_record(&state.registers, start_reg, num_regs);
        let pc = if let Some(ref idx_record) = return_if_io!(cursor.record()) {
            let idx_values = idx_record.get_values();
            let idx_values = &idx_values[..record_from_regs.len()];
            let record_values = record_from_regs.get_values();
            let ord = compare_immutable(
                &idx_values,
                &record_values,
                cursor.key_sort_order(),
                cursor.collations(),
            );
            if ord.is_gt() {
                target_pc.to_offset_int()
            } else {
                state.pc + 1
            }
        } else {
            target_pc.to_offset_int()
        };
        pc
    };
    state.pc = pc;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_lt(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxLT {
        cursor_id,
        start_reg,
        num_regs,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let pc = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        let record_from_regs = make_record(&state.registers, start_reg, num_regs);
        let pc = if let Some(ref idx_record) = return_if_io!(cursor.record()) {
            let idx_values = idx_record.get_values();
            let idx_values = &idx_values[..record_from_regs.len()];
            let record_values = record_from_regs.get_values();
            let ord = compare_immutable(
                &idx_values,
                &record_values,
                cursor.key_sort_order(),
                cursor.collations(),
            );
            if ord.is_lt() {
                target_pc.to_offset_int()
            } else {
                state.pc + 1
            }
        } else {
            target_pc.to_offset_int()
        };
        pc
    };
    state.pc = pc;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_new_rowid(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::NewRowid {
        cursor, rowid_reg, ..
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let rowid = {
        let mut cursor = state.get_cursor(*cursor);
        let cursor = cursor.as_btree_mut();
        let rowid = return_if_io!(get_new_rowid(cursor, thread_rng()));
        rowid
    };
    state.registers[*rowid_reg] = Register::Value(Value::Integer(rowid));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_no_conflict(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::NoConflict {
        cursor_id,
        target_pc,
        record_reg,
        num_regs,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let mut cursor_ref = state.get_cursor(*cursor_id);
    let cursor = cursor_ref.as_btree_mut();
    let record = if *num_regs == 0 {
        let record = match &state.registers[*record_reg] {
            Register::Record(r) => r,
            _ => {
                return Err(LimboError::InternalError(
                    "NoConflict: exepected a record in the register".into(),
                ));
            }
        };
        record
    } else {
        &make_record(&state.registers, record_reg, num_regs)
    };
    let contains_nulls = record
        .get_values()
        .iter()
        .any(|val| matches!(val, RefValue::Null));
    if contains_nulls {
        drop(cursor_ref);
        state.pc = target_pc.to_offset_int();
        return Ok(InsnFunctionStepResult::Step);
    }
    let conflict =
        return_if_io!(cursor.seek(SeekKey::IndexKey(record), SeekOp::GE { eq_only: true }));
    drop(cursor_ref);
    if !conflict {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_not_exists(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::NotExists {
        cursor,
        rowid_reg,
        target_pc,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let exists = {
        let mut cursor_borrow =
            must_be_btree_cursor!(*cursor, program.cursor_ref, state, "NotExists");
        let cursor_btree = cursor_borrow.as_btree_mut();
        let rowid_val = state.registers[*rowid_reg].get_owned_value();
        let exists_raw = cursor_btree.exists(&rowid_val);
        let exists = return_if_io!(exists_raw);
        exists
    };
    if exists {
        state.pc += 1;
    } else {
        state.pc = target_pc.to_offset_int();
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_open_write(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::OpenWrite {
        cursor_id,
        root_page,
        ..
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let root_page = match root_page {
        RegisterOrLiteral::Literal(lit) => *lit as u64,
        RegisterOrLiteral::Register(reg) => match &state.registers[*reg].get_owned_value() {
            Value::Integer(val) => *val as u64,
            _ => {
                return Err(LimboError::InternalError(
                    "OpenWrite: the value in root_page is not an integer".into(),
                ));
            }
        },
    };
    let (_, cursor_type) = program.cursor_ref.get(*cursor_id).unwrap();
    let mut cursors = state.cursors.borrow_mut();
    let maybe_index = match cursor_type {
        CursorType::BTreeIndex(index) => Some(index),
        _ => None,
    };
    let mv_cursor = match state.mv_tx_id {
        Some(tx_id) => {
            let table_id = root_page;
            let mv_store = mv_store.unwrap().clone();
            let mv_cursor = Rc::new(RefCell::new(
                MvCursor::new(mv_store.clone(), tx_id, table_id).unwrap(),
            ));
            Some(mv_cursor)
        }
        None => None,
    };
    if let Some(index) = maybe_index {
        let conn = program.connection.clone();
        let schema = conn.schema.try_read().ok_or(LimboError::SchemaLocked)?;
        let table = schema
            .get_table(&index.table_name)
            .map_or(None, |table| table.btree());
        let collations = table.map_or(Vec::new(), |table| {
            index
                .columns
                .iter()
                .map(|c| {
                    table
                        .columns
                        .get(c.pos_in_table)
                        .unwrap()
                        .collation
                        .unwrap_or_default()
                })
                .collect()
        });
        let cursor = BTreeCursor::new_index(
            mv_cursor,
            pager.clone(),
            root_page as usize,
            index.as_ref(),
            collations,
        );
        cursors
            .get_mut(*cursor_id)
            .unwrap()
            .replace(Cursor::new_btree(cursor));
    } else {
        let cursor = BTreeCursor::new_table(mv_cursor, pager.clone(), root_page as usize);
        cursors
            .get_mut(*cursor_id)
            .unwrap()
            .replace(Cursor::new_btree(cursor));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_close(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Close { cursor_id } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let mut cursors = state.cursors.borrow_mut();
    cursors.get_mut(*cursor_id).unwrap().take();
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_open_ephemeral(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let (cursor_id, is_table) = match insn {
        Insn::OpenEphemeral {
            cursor_id,
            is_table,
        } => (*cursor_id, *is_table),
        Insn::OpenAutoindex { cursor_id } => (*cursor_id, false),
        _ => unreachable!("unexpected Insn {:?}", insn),
    };
    let conn = program.connection.clone();
    let io = conn.pager.io.get_memory_io();
    let file = io.open_file("", OpenFlags::Create, true)?;
    maybe_init_database_file(&file, &(io.clone() as Arc<dyn IO>))?;
    let db_file = Arc::new(FileMemoryStorage::new(file));
    let db_header = Pager::begin_open(db_file.clone())?;
    let buffer_pool = Rc::new(BufferPool::new(db_header.lock().get_page_size() as usize));
    let page_cache = Arc::new(RwLock::new(DumbLruPageCache::default()));
    let pager = Rc::new(Pager::finish_open(
        db_header,
        db_file,
        Rc::new(RefCell::new(DummyWAL)),
        io,
        page_cache,
        buffer_pool,
    )?);
    let flag = if is_table {
        &CreateBTreeFlags::new_table()
    } else {
        &CreateBTreeFlags::new_index()
    };
    let root_page = return_if_io!(pager.btree_create(flag));
    let (_, cursor_type) = program.cursor_ref.get(cursor_id).unwrap();
    let mv_cursor = match state.mv_tx_id {
        Some(tx_id) => {
            let table_id = root_page as u64;
            let mv_store = mv_store.unwrap().clone();
            let mv_cursor = Rc::new(RefCell::new(
                MvCursor::new(mv_store.clone(), tx_id, table_id).unwrap(),
            ));
            Some(mv_cursor)
        }
        None => None,
    };
    let mut cursor = if let CursorType::BTreeIndex(index) = cursor_type {
        BTreeCursor::new_index(
            mv_cursor,
            pager,
            root_page as usize,
            index,
            index
                .columns
                .iter()
                .map(|c| c.collation.unwrap_or_default())
                .collect(),
        )
    } else {
        BTreeCursor::new_table(mv_cursor, pager, root_page as usize)
    };
    cursor.rewind()?;
    let mut cursors: std::cell::RefMut<'_, Vec<Option<Cursor>>> = state.cursors.borrow_mut();
    match cursor_type {
        CursorType::BTreeTable(_) => {
            cursors
                .get_mut(cursor_id)
                .unwrap()
                .replace(Cursor::new_btree(cursor));
        }
        CursorType::BTreeIndex(_) => {
            cursors
                .get_mut(cursor_id)
                .unwrap()
                .replace(Cursor::new_btree(cursor));
        }
        CursorType::Pseudo(_) => {
            panic!("OpenEphemeral on pseudo cursor");
        }
        CursorType::Sorter => {
            panic!("OpenEphemeral on sorter cursor");
        }
        CursorType::VirtualTable(_) => {
            panic!("OpenEphemeral on virtual table cursor, use Insn::VOpen instead");
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
/// Execute the [Insn::Once] instruction.
///
/// This instruction is used to execute a block of code only once.
/// If the instruction is executed again, it will jump to the target program counter.
pub fn op_once(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Once {
        target_pc_when_reentered,
    } = insn
    else {
        unreachable!("unexpected Insn: {:?}", insn)
    };
    assert!(target_pc_when_reentered.is_offset());
    let offset = state.pc;
    if state.once.iter().any(|o| o == offset) {
        state.pc = target_pc_when_reentered.to_offset_int();
        return Ok(InsnFunctionStepResult::Step);
    }
    state.once.push(offset);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_found(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let (cursor_id, target_pc, record_reg, num_regs) = match insn {
        Insn::NotFound {
            cursor_id,
            target_pc,
            record_reg,
            num_regs,
        } => (cursor_id, target_pc, record_reg, num_regs),
        Insn::Found {
            cursor_id,
            target_pc,
            record_reg,
            num_regs,
        } => (cursor_id, target_pc, record_reg, num_regs),
        _ => unreachable!("unexpected Insn {:?}", insn),
    };
    let not = matches!(insn, Insn::NotFound { .. });
    let found = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        if *num_regs == 0 {
            let record = match &state.registers[*record_reg] {
                Register::Record(r) => r,
                _ => {
                    return Err(LimboError::InternalError(
                        "NotFound: exepected a record in the register".into(),
                    ));
                }
            };
            return_if_io!(cursor.seek(SeekKey::IndexKey(&record), SeekOp::GE { eq_only: true }))
        } else {
            let record = make_record(&state.registers, record_reg, num_regs);
            return_if_io!(cursor.seek(SeekKey::IndexKey(&record), SeekOp::GE { eq_only: true }))
        }
    };
    let do_jump = (!found && not) || (found && !not);
    if do_jump {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_count(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Count {
        cursor_id,
        target_reg,
        exact,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let count = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "Count");
        let cursor = cursor.as_btree_mut();
        let count = return_if_io!(cursor.count());
        count
    };
    state.registers[*target_reg] = Register::Value(Value::Integer(count as i64));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
