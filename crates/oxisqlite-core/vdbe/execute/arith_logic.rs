#![allow(unused_variables)]
use super::super::{Program, ProgramState, Register};
use crate::error::LimboError;
use crate::schema::Affinity;
use crate::translate::collate::CollationSeq;
use crate::vdbe::builder::CursorType;
use crate::{must_be_btree_cursor, MvStore, Pager, Result};
use crate::{
    types::Value,
    util::{cast_real_to_integer, checked_cast_text_to_numeric},
    vdbe::insn::Insn,
};
use std::rc::Rc;

use super::numeric::{
    apply_affinity_char, apply_numeric_affinity, is_numeric_value, stringify_register,
};
use super::InsnFunctionStepResult;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum ComparisonOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}
impl ComparisonOp {
    pub(super) fn compare(&self, lhs: &Value, rhs: &Value, collation: &CollationSeq) -> bool {
        match (lhs, rhs) {
            (Value::Text(lhs_text), Value::Text(rhs_text)) => {
                let order = collation.compare_strings(lhs_text.as_str(), rhs_text.as_str());
                match self {
                    ComparisonOp::Eq => order.is_eq(),
                    ComparisonOp::Ne => order.is_ne(),
                    ComparisonOp::Lt => order.is_lt(),
                    ComparisonOp::Le => order.is_le(),
                    ComparisonOp::Gt => order.is_gt(),
                    ComparisonOp::Ge => order.is_ge(),
                }
            }
            (_, _) => match self {
                ComparisonOp::Eq => *lhs == *rhs,
                ComparisonOp::Ne => *lhs != *rhs,
                ComparisonOp::Lt => *lhs < *rhs,
                ComparisonOp::Le => *lhs <= *rhs,
                ComparisonOp::Gt => *lhs > *rhs,
                ComparisonOp::Ge => *lhs >= *rhs,
            },
        }
    }
    pub(super) fn compare_integers(&self, lhs: &Value, rhs: &Value) -> bool {
        match self {
            ComparisonOp::Eq => lhs == rhs,
            ComparisonOp::Ne => lhs != rhs,
            ComparisonOp::Lt => lhs < rhs,
            ComparisonOp::Le => lhs <= rhs,
            ComparisonOp::Gt => lhs > rhs,
            ComparisonOp::Ge => lhs >= rhs,
        }
    }
    pub(super) fn handle_nulls(
        &self,
        lhs: &Value,
        rhs: &Value,
        null_eq: bool,
        jump_if_null: bool,
    ) -> bool {
        match self {
            ComparisonOp::Eq => {
                let both_null = lhs == rhs;
                (null_eq && both_null) || (!null_eq && jump_if_null)
            }
            ComparisonOp::Ne => {
                let at_least_one_null = lhs != rhs;
                (null_eq && at_least_one_null) || (!null_eq && jump_if_null)
            }
            ComparisonOp::Lt | ComparisonOp::Le | ComparisonOp::Gt | ComparisonOp::Ge => {
                jump_if_null
            }
        }
    }
}
pub fn op_init(
    _program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Init { target_pc } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    state.pc = target_pc.to_offset_int();
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_add(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Add { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_add(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_subtract(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Subtract { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_subtract(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_multiply(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Multiply { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_multiply(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_divide(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Divide { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_divide(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_remainder(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Remainder { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_remainder(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_bit_and(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::BitAnd { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_bit_and(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_bit_or(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::BitOr { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_bit_or(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_bit_not(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::BitNot { reg, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] =
        Register::Value(state.registers[*reg].get_owned_value().exec_bit_not());
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    match insn {
        Insn::Null { dest, dest_end } | Insn::BeginSubrtn { dest, dest_end } => {
            if let Some(dest_end) = dest_end {
                for i in *dest..=*dest_end {
                    state.registers[i] = Register::Value(Value::Null);
                }
            } else {
                state.registers[*dest] = Register::Value(Value::Null);
            }
        }
        _ => unreachable!("unexpected Insn {:?}", insn),
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_null_row(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::NullRow { cursor_id } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "NullRow");
        let cursor = cursor.as_btree_mut();
        cursor.set_null_flag(true);
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_compare(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Compare {
        start_reg_a,
        start_reg_b,
        count,
        collation,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let start_reg_a = *start_reg_a;
    let start_reg_b = *start_reg_b;
    let count = *count;
    let collation = collation.unwrap_or_default();
    if start_reg_a + count > start_reg_b {
        return Err(LimboError::InternalError(
            "Compare registers overlap".to_string(),
        ));
    }
    let mut cmp = None;
    for i in 0..count {
        let a = state.registers[start_reg_a + i].get_owned_value();
        let b = state.registers[start_reg_b + i].get_owned_value();
        cmp = match (a, b) {
            (Value::Text(left), Value::Text(right)) => {
                Some(collation.compare_strings(left.as_str(), right.as_str()))
            }
            _ => Some(a.cmp(b)),
        };
        if cmp != Some(std::cmp::Ordering::Equal) {
            break;
        }
    }
    state.last_compare = cmp;
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_jump(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Jump {
        target_pc_lt,
        target_pc_eq,
        target_pc_gt,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc_lt.is_offset());
    assert!(target_pc_eq.is_offset());
    assert!(target_pc_gt.is_offset());
    let cmp = state.last_compare.take();
    if cmp.is_none() {
        return Err(LimboError::InternalError(
            "Jump without compare".to_string(),
        ));
    }
    let target_pc = match cmp.unwrap() {
        std::cmp::Ordering::Less => *target_pc_lt,
        std::cmp::Ordering::Equal => *target_pc_eq,
        std::cmp::Ordering::Greater => *target_pc_gt,
    };
    state.pc = target_pc.to_offset_int();
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_move(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Move {
        source_reg,
        dest_reg,
        count,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let source_reg = *source_reg;
    let dest_reg = *dest_reg;
    let count = *count;
    for i in 0..count {
        state.registers[dest_reg + i] = std::mem::replace(
            &mut state.registers[source_reg + i],
            Register::Value(Value::Null),
        );
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_if_pos(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IfPos {
        reg,
        target_pc,
        decrement_by,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let reg = *reg;
    let target_pc = *target_pc;
    match state.registers[reg].get_owned_value() {
        Value::Integer(n) if *n > 0 => {
            state.pc = target_pc.to_offset_int();
            state.registers[reg] = Register::Value(Value::Integer(*n - *decrement_by as i64));
        }
        Value::Integer(_) => {
            state.pc += 1;
        }
        _ => {
            return Err(LimboError::InternalError(
                "IfPos: the value in the register is not an integer".into(),
            ));
        }
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_not_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::NotNull { reg, target_pc } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    let reg = *reg;
    let target_pc = *target_pc;
    match &state.registers[reg].get_owned_value() {
        Value::Null => {
            state.pc += 1;
        }
        _ => {
            state.pc = target_pc.to_offset_int();
        }
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_comparison(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let (lhs, rhs, target_pc, flags, collation, op) = match insn {
        Insn::Eq {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Eq,
        ),
        Insn::Ne {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Ne,
        ),
        Insn::Lt {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Lt,
        ),
        Insn::Le {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Le,
        ),
        Insn::Gt {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Gt,
        ),
        Insn::Ge {
            lhs,
            rhs,
            target_pc,
            flags,
            collation,
        } => (
            *lhs,
            *rhs,
            *target_pc,
            *flags,
            collation.unwrap_or_default(),
            ComparisonOp::Ge,
        ),
        _ => unreachable!("unexpected Insn {:?}", insn),
    };
    assert!(target_pc.is_offset());
    let nulleq = flags.has_nulleq();
    let jump_if_null = flags.has_jump_if_null();
    let affinity = flags.get_affinity();
    let lhs_value = state.registers[lhs].get_owned_value();
    let rhs_value = state.registers[rhs].get_owned_value();
    if matches!(lhs_value, Value::Integer(_)) && matches!(rhs_value, Value::Integer(_)) {
        if op.compare_integers(lhs_value, rhs_value) {
            state.pc = target_pc.to_offset_int();
        } else {
            state.pc += 1;
        }
        return Ok(InsnFunctionStepResult::Step);
    }
    if matches!(lhs_value, Value::Null) || matches!(rhs_value, Value::Null) {
        if op.handle_nulls(lhs_value, rhs_value, nulleq, jump_if_null) {
            state.pc = target_pc.to_offset_int();
        } else {
            state.pc += 1;
        }
        return Ok(InsnFunctionStepResult::Step);
    }
    let mut lhs_temp_reg = state.registers[lhs].clone();
    let mut rhs_temp_reg = state.registers[rhs].clone();
    let mut lhs_converted = false;
    let mut rhs_converted = false;
    match affinity {
        Affinity::Numeric | Affinity::Integer => {
            let lhs_is_text = matches!(lhs_temp_reg.get_owned_value(), Value::Text(_));
            let rhs_is_text = matches!(rhs_temp_reg.get_owned_value(), Value::Text(_));
            if lhs_is_text || rhs_is_text {
                if lhs_is_text {
                    lhs_converted = apply_numeric_affinity(&mut lhs_temp_reg, false);
                }
                if rhs_is_text {
                    rhs_converted = apply_numeric_affinity(&mut rhs_temp_reg, false);
                }
            }
        }
        Affinity::Text => {
            let lhs_is_text = matches!(lhs_temp_reg.get_owned_value(), Value::Text(_));
            let rhs_is_text = matches!(rhs_temp_reg.get_owned_value(), Value::Text(_));
            if lhs_is_text || rhs_is_text {
                if is_numeric_value(&lhs_temp_reg) {
                    lhs_converted = stringify_register(&mut lhs_temp_reg);
                }
                if is_numeric_value(&rhs_temp_reg) {
                    rhs_converted = stringify_register(&mut rhs_temp_reg);
                }
            }
        }
        Affinity::Real => {
            if matches!(lhs_temp_reg.get_owned_value(), Value::Text(_)) {
                lhs_converted = apply_numeric_affinity(&mut lhs_temp_reg, false);
            }
            if matches!(rhs_temp_reg.get_owned_value(), Value::Text(_)) {
                rhs_converted = apply_numeric_affinity(&mut rhs_temp_reg, false);
            }
            if let Value::Integer(i) = lhs_temp_reg.get_owned_value() {
                lhs_temp_reg = Register::Value(Value::Float(*i as f64));
                lhs_converted = true;
            }
            if let Value::Integer(i) = rhs_temp_reg.get_owned_value() {
                rhs_temp_reg = Register::Value(Value::Float(*i as f64));
                rhs_converted = true;
            }
        }
        Affinity::Blob => {}
    }
    let should_jump = op.compare(
        lhs_temp_reg.get_owned_value(),
        rhs_temp_reg.get_owned_value(),
        &collation,
    );
    if lhs_converted {
        state.registers[lhs] = lhs_temp_reg;
    }
    if rhs_converted {
        state.registers[rhs] = rhs_temp_reg;
    }
    if should_jump {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_if(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::If {
        reg,
        target_pc,
        jump_if_null,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    if state.registers[*reg]
        .get_owned_value()
        .exec_if(*jump_if_null, false)
    {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_if_not(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IfNot {
        reg,
        target_pc,
        jump_if_null,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    if state.registers[*reg]
        .get_owned_value()
        .exec_if(*jump_if_null, true)
    {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_goto(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Goto { target_pc } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    state.pc = target_pc.to_offset_int();
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_gosub(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Gosub {
        target_pc,
        return_reg,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    state.registers[*return_reg] = Register::Value(Value::Integer((state.pc + 1) as i64));
    state.pc = target_pc.to_offset_int();
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_return(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Return {
        return_reg,
        can_fallthrough,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Value::Integer(pc) = state.registers[*return_reg].get_owned_value() {
        let pc: u32 = (*pc)
            .try_into()
            .unwrap_or_else(|_| panic!("Return register is negative: {}", pc));
        state.pc = pc;
    } else {
        if !*can_fallthrough {
            return Err(LimboError::InternalError(
                "Return register is not an integer".to_string(),
            ));
        }
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_integer(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Integer { value, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(Value::Integer(*value));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_real(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Real { value, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(Value::Float(*value));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_real_affinity(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::RealAffinity { register } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Value::Integer(i) = &state.registers[*register].get_owned_value() {
        state.registers[*register] = Register::Value(Value::Float(*i as f64));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_string8(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::String8 { value, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(Value::build_text(value));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_blob(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Blob { value, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(Value::Blob(value.clone()));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_decr_jump_zero(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::DecrJumpZero { reg, target_pc } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(target_pc.is_offset());
    match state.registers[*reg].get_owned_value() {
        Value::Integer(n) => {
            let n = n - 1;
            state.registers[*reg] = Register::Value(Value::Integer(n));
            if n == 0 {
                state.pc = target_pc.to_offset_int();
            } else {
                state.pc += 1;
            }
        }
        _ => unreachable!("DecrJumpZero on non-integer register"),
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_int_64(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Int64 {
        _p1,
        out_reg,
        _p3,
        value,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*out_reg] = Register::Value(Value::Integer(*value));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_must_be_int(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::MustBeInt { reg } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    match &state.registers[*reg].get_owned_value() {
        Value::Integer(_) => {}
        Value::Float(f) => match cast_real_to_integer(*f) {
            Ok(i) => state.registers[*reg] = Register::Value(Value::Integer(i)),
            Err(_) => {
                crate::bail_parse_error!(
                    "MustBeInt: the value in register cannot be cast to integer"
                )
            }
        },
        Value::Text(text) => match checked_cast_text_to_numeric(text.as_str()) {
            Ok(Value::Integer(i)) => {
                state.registers[*reg] = Register::Value(Value::Integer(i));
            }
            Ok(Value::Float(f)) => {
                state.registers[*reg] = Register::Value(Value::Integer(f as i64));
            }
            _ => {
                crate::bail_parse_error!(
                    "MustBeInt: the value in register cannot be cast to integer"
                )
            }
        },
        _ => {
            crate::bail_parse_error!("MustBeInt: the value in register cannot be cast to integer");
        }
    };
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_soft_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SoftNull { reg } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*reg] = Register::Value(Value::Null);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_offset_limit(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::OffsetLimit {
        limit_reg,
        combined_reg,
        offset_reg,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let limit_val = match state.registers[*limit_reg].get_owned_value() {
        Value::Integer(val) => val,
        _ => {
            return Err(LimboError::InternalError(
                "OffsetLimit: the value in limit_reg is not an integer".into(),
            ));
        }
    };
    let offset_val = match state.registers[*offset_reg].get_owned_value() {
        Value::Integer(val) if *val < 0 => 0,
        Value::Integer(val) if *val >= 0 => *val,
        _ => {
            return Err(LimboError::InternalError(
                "OffsetLimit: the value in offset_reg is not an integer".into(),
            ));
        }
    };
    let offset_limit_sum = limit_val.overflowing_add(offset_val);
    if *limit_val <= 0 || offset_limit_sum.1 {
        state.registers[*combined_reg] = Register::Value(Value::Integer(-1));
    } else {
        state.registers[*combined_reg] = Register::Value(Value::Integer(offset_limit_sum.0));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_copy(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Copy {
        src_reg,
        dst_reg,
        amount,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    for i in 0..=*amount {
        state.registers[*dst_reg + i] = state.registers[*src_reg + i].clone();
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_is_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IsNull { reg, target_pc } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if matches!(state.registers[*reg], Register::Value(Value::Null)) {
        state.pc = target_pc.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_shift_right(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ShiftRight { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_shift_right(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_shift_left(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ShiftLeft { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_shift_left(state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_variable(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Variable { index, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(state.get_parameter(*index));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_zero_or_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ZeroOrNull { rg1, rg2, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *state.registers[*rg1].get_owned_value() == Value::Null
        || *state.registers[*rg2].get_owned_value() == Value::Null
    {
        state.registers[*dest] = Register::Value(Value::Null)
    } else {
        state.registers[*dest] = Register::Value(Value::Integer(0));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_not(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Not { reg, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] =
        Register::Value(state.registers[*reg].get_owned_value().exec_boolean_not());
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_concat(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Concat { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_concat(&state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_and(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::And { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_and(&state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_or(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Or { lhs, rhs, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    state.registers[*dest] = Register::Value(
        state.registers[*lhs]
            .get_owned_value()
            .exec_or(&state.registers[*rhs].get_owned_value()),
    );
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_noop(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_affinity(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Affinity {
        start_reg,
        count,
        affinities,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if affinities.len() != count.get() {
        return Err(LimboError::InternalError(
            "Affinity: the length of affinities does not match the count".into(),
        ));
    }
    for (i, affinity_char) in affinities.chars().enumerate().take(count.get()) {
        let reg_index = *start_reg + i;
        let affinity = Affinity::from_char(affinity_char)?;
        apply_affinity_char(&mut state.registers[reg_index], affinity);
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
