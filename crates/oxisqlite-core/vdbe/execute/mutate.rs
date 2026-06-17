#![allow(unused_variables)]
use super::super::{make_record, Program, ProgramState, Register};
use crate::error::LimboError;
use crate::storage::btree::{BTreeCursor, BTreeKey};
use crate::types::ImmutableRecord;
use crate::vdbe::insn::InsertFlags;
use crate::{
    types::{CursorResult, SeekKey, SeekOp, Value},
    vdbe::{
        builder::CursorType,
        insn::{IdxInsertFlags, Insn},
    },
};
use crate::{MvStore, Pager, Result};
use std::rc::Rc;

use super::InsnFunctionStepResult;

#[derive(Debug)]
pub enum OpIdxDeleteState {
    Seeking(ImmutableRecord),
    Verifying,
    Deleting,
}
pub fn op_insert(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Insert {
        cursor,
        key_reg,
        record_reg,
        flag,
        table_name: _,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    {
        let mut cursor = state.get_cursor(*cursor);
        let cursor = cursor.as_btree_mut();
        let record = match &state.registers[*record_reg] {
            Register::Record(r) => r,
            _ => unreachable!("Not a record! Cannot insert a non record value."),
        };
        let key = match &state.registers[*key_reg].get_owned_value() {
            Value::Integer(i) => *i,
            _ => unreachable!("expected integer key"),
        };
        return_if_io!(cursor.insert(&BTreeKey::new_table_rowid(key, Some(record)), true));
        if cursor.root_page() != 1 {
            if let Some(rowid) = return_if_io!(cursor.rowid()) {
                program.connection.update_last_rowid(rowid);
                if !flag.has(InsertFlags::UPDATE) {
                    let prev_changes = program.n_change.get();
                    program.n_change.set(prev_changes + 1);
                }
            }
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_delete(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Delete { cursor_id } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_btree_mut();
        return_if_io!(cursor.delete());
    }
    let prev_changes = program.n_change.get();
    program.n_change.set(prev_changes + 1);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_delete(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxDelete {
        cursor_id,
        start_reg,
        num_regs,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    loop {
        tracing::debug!(
            "op_idx_delete(cursor_id={}, start_reg={}, num_regs={}, rootpage={}, state={:?})",
            cursor_id,
            start_reg,
            num_regs,
            state.get_cursor(*cursor_id).as_btree_mut().root_page(),
            state.op_idx_delete_state
        );
        match &state.op_idx_delete_state {
            Some(OpIdxDeleteState::Seeking(record)) => {
                {
                    let mut cursor = state.get_cursor(*cursor_id);
                    let cursor = cursor.as_btree_mut();
                    let found = return_if_io!(
                        cursor.seek(SeekKey::IndexKey(&record), SeekOp::GE { eq_only: true })
                    );
                    tracing::debug!(
                        "op_idx_delete: found={:?}, rootpage={}, key={:?}",
                        found,
                        cursor.root_page(),
                        record
                    );
                }
                state.op_idx_delete_state = Some(OpIdxDeleteState::Verifying);
            }
            Some(OpIdxDeleteState::Verifying) => {
                let rowid = {
                    let mut cursor = state.get_cursor(*cursor_id);
                    let cursor = cursor.as_btree_mut();
                    return_if_io!(cursor.rowid())
                };
                if rowid.is_none() {
                    return Err(LimboError::Corrupt(format!(
                        "IdxDelete: no matching index entry found for record {:?}",
                        make_record(&state.registers, start_reg, num_regs)
                    )));
                }
                state.op_idx_delete_state = Some(OpIdxDeleteState::Deleting);
            }
            Some(OpIdxDeleteState::Deleting) => {
                {
                    let mut cursor = state.get_cursor(*cursor_id);
                    let cursor = cursor.as_btree_mut();
                    return_if_io!(cursor.delete());
                }
                state.pc += 1;
                state.op_idx_delete_state = None;
                return Ok(InsnFunctionStepResult::Step);
            }
            None => {
                let record = make_record(&state.registers, start_reg, num_regs);
                state.op_idx_delete_state = Some(OpIdxDeleteState::Seeking(record));
            }
        }
    }
}
pub fn op_idx_insert(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    if let Insn::IdxInsert {
        cursor_id,
        record_reg,
        flags,
        ..
    } = *insn
    {
        let (_, cursor_type) = program.cursor_ref.get(cursor_id).unwrap();
        let CursorType::BTreeIndex(index_meta) = cursor_type else {
            panic!("IdxInsert: not a BTree index cursor");
        };
        {
            let mut cursor = state.get_cursor(cursor_id);
            let cursor = cursor.as_btree_mut();
            let record = match &state.registers[record_reg] {
                Register::Record(ref r) => r,
                o => {
                    return Err(LimboError::InternalError(format!(
                        "expected record, got {:?}",
                        o
                    )));
                }
            };
            let moved_before = if cursor.is_write_in_progress() {
                true
            } else {
                if index_meta.unique {
                    match cursor.key_exists_in_index(record)? {
                        CursorResult::Ok(true) => {
                            return Err(LimboError::Constraint(
                                "UNIQUE constraint failed: duplicate key".into(),
                            ));
                        }
                        CursorResult::IO => return Ok(InsnFunctionStepResult::IO),
                        CursorResult::Ok(false) => {}
                    };
                    true
                } else {
                    flags.has(IdxInsertFlags::USE_SEEK)
                }
            };
            return_if_io!(cursor.insert(&BTreeKey::new_index_key(record), moved_before));
        }
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_create_btree(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::CreateBtree { db, root, flags } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *db > 0 {
        todo!("temp databases not implemented yet");
    }
    let root_page = return_if_io!(pager.btree_create(flags));
    state.registers[*root] = Register::Value(Value::Integer(root_page as i64));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_destroy(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Destroy {
        root,
        former_root_reg,
        is_temp,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *is_temp == 1 {
        todo!("temp databases not implemented yet.");
    }
    let mut cursor = BTreeCursor::new(None, pager.clone(), *root, Vec::new());
    let former_root_page_result = cursor.btree_destroy()?;
    if let CursorResult::Ok(former_root_page) = former_root_page_result {
        state.registers[*former_root_reg] =
            Register::Value(Value::Integer(former_root_page.unwrap_or(0) as i64));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_drop_table(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::DropTable { db, table_name, .. } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *db > 0 {
        todo!("temp databases not implemented yet");
    }
    let conn = program.connection.clone();
    {
        let mut schema = conn.schema.write();
        schema.remove_indices_for_table(table_name);
        schema.remove_table(table_name);
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
