#![allow(unused_variables)]
use super::super::sorter::Sorter;
use super::super::{Program, ProgramState, Register};
use crate::{
    ext::ExtValue,
    function::{AggFunc, ExtFunc},
};
#[cfg(feature = "json")]
use crate::{json::convert_dbtype_to_raw_jsonb, json::json_from_raw_bytes_agg};
use crate::{
    types::{AggContext, Cursor, ExternalAggState, Value},
    vdbe::insn::Insn,
};
use crate::{MvStore, Pager, Result};
use std::{borrow::BorrowMut, rc::Rc};

use super::InsnFunctionStepResult;

pub fn op_agg_step(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::AggStep {
        acc_reg,
        col,
        delimiter,
        func,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Register::Value(Value::Null) = state.registers[*acc_reg] {
        state.registers[*acc_reg] = match func {
            AggFunc::Avg => {
                Register::Aggregate(AggContext::Avg(Value::Float(0.0), Value::Integer(0)))
            }
            AggFunc::Sum => Register::Aggregate(AggContext::Sum(Value::Null)),
            AggFunc::Total => Register::Aggregate(AggContext::Sum(Value::Float(0.0))),
            AggFunc::Count | AggFunc::Count0 => {
                Register::Aggregate(AggContext::Count(Value::Integer(0)))
            }
            AggFunc::Max => {
                let col = state.registers[*col].get_owned_value();
                match col {
                    Value::Integer(_) => Register::Aggregate(AggContext::Max(None)),
                    Value::Float(_) => Register::Aggregate(AggContext::Max(None)),
                    Value::Text(_) => Register::Aggregate(AggContext::Max(None)),
                    _ => {
                        unreachable!();
                    }
                }
            }
            AggFunc::Min => {
                let col = state.registers[*col].get_owned_value();
                match col {
                    Value::Integer(_) => Register::Aggregate(AggContext::Min(None)),
                    Value::Float(_) => Register::Aggregate(AggContext::Min(None)),
                    Value::Text(_) => Register::Aggregate(AggContext::Min(None)),
                    _ => {
                        unreachable!();
                    }
                }
            }
            AggFunc::GroupConcat | AggFunc::StringAgg => {
                Register::Aggregate(AggContext::GroupConcat(Value::build_text("")))
            }
            #[cfg(feature = "json")]
            AggFunc::JsonGroupArray | AggFunc::JsonbGroupArray => {
                Register::Aggregate(AggContext::GroupConcat(Value::Blob(vec![])))
            }
            #[cfg(feature = "json")]
            AggFunc::JsonGroupObject | AggFunc::JsonbGroupObject => {
                Register::Aggregate(AggContext::GroupConcat(Value::Blob(vec![])))
            }
            AggFunc::External(func) => match func.as_ref() {
                ExtFunc::Aggregate {
                    init,
                    step,
                    finalize,
                    argc,
                } => Register::Aggregate(AggContext::External(ExternalAggState {
                    state: unsafe { (init)() },
                    argc: *argc,
                    step_fn: *step,
                    finalize_fn: *finalize,
                    finalized_value: None,
                })),
                _ => unreachable!("scalar function called in aggregate context"),
            },
        };
    }
    match func {
        AggFunc::Avg => {
            let col = state.registers[*col].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                panic!(
                    "Unexpected value {:?} in AggStep at register {}",
                    state.registers[*acc_reg], *acc_reg
                );
            };
            let AggContext::Avg(acc, count) = agg.borrow_mut() else {
                unreachable!();
            };
            *acc = acc.exec_add(col.get_owned_value());
            *count += 1;
        }
        AggFunc::Sum | AggFunc::Total => {
            let col = state.registers[*col].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                panic!(
                    "Unexpected value {:?} at register {:?} in AggStep",
                    state.registers[*acc_reg], *acc_reg
                );
            };
            let AggContext::Sum(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            match col {
                Register::Value(owned_value) => {
                    *acc += owned_value;
                }
                _ => unreachable!(),
            }
        }
        AggFunc::Count | AggFunc::Count0 => {
            let col = state.registers[*col].get_owned_value().clone();
            if matches!(&state.registers[*acc_reg], Register::Value(Value::Null)) {
                state.registers[*acc_reg] =
                    Register::Aggregate(AggContext::Count(Value::Integer(0)));
            }
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                panic!(
                    "Unexpected value {:?} in AggStep at register {}",
                    state.registers[*acc_reg], *acc_reg
                );
            };
            let AggContext::Count(count) = agg.borrow_mut() else {
                unreachable!();
            };
            if !(matches!(func, AggFunc::Count) && matches!(col, Value::Null)) {
                *count += 1;
            }
        }
        AggFunc::Max => {
            let col = state.registers[*col].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                panic!(
                    "Unexpected value {:?} in AggStep at register {}",
                    state.registers[*acc_reg], *acc_reg
                );
            };
            let AggContext::Max(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            match (acc.as_mut(), col.get_owned_value()) {
                (None, value) => {
                    *acc = Some(value.clone());
                }
                (Some(Value::Integer(ref mut current_max)), Value::Integer(value)) => {
                    if *value > *current_max {
                        *current_max = value.clone();
                    }
                }
                (Some(Value::Float(ref mut current_max)), Value::Float(value)) => {
                    if *value > *current_max {
                        *current_max = *value;
                    }
                }
                (Some(Value::Text(ref mut current_max)), Value::Text(value)) => {
                    if value.value > current_max.value {
                        *current_max = value.clone();
                    }
                }
                _ => {
                    eprintln!("Unexpected types in max aggregation");
                }
            }
        }
        AggFunc::Min => {
            let col = state.registers[*col].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                panic!(
                    "Unexpected value {:?} in AggStep",
                    state.registers[*acc_reg]
                );
            };
            let AggContext::Min(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            match (acc.as_mut(), col.get_owned_value()) {
                (None, value) => {
                    *acc.borrow_mut() = Some(value.clone());
                }
                (Some(Value::Integer(ref mut current_min)), Value::Integer(value)) => {
                    if *value < *current_min {
                        *current_min = *value;
                    }
                }
                (Some(Value::Float(ref mut current_min)), Value::Float(value)) => {
                    if *value < *current_min {
                        *current_min = *value;
                    }
                }
                (Some(Value::Text(ref mut current_min)), Value::Text(text)) => {
                    if text.value < current_min.value {
                        *current_min = text.clone();
                    }
                }
                _ => {
                    eprintln!("Unexpected types in min aggregation");
                }
            }
        }
        AggFunc::GroupConcat | AggFunc::StringAgg => {
            let col = state.registers[*col].get_owned_value().clone();
            let delimiter = state.registers[*delimiter].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                unreachable!();
            };
            let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            if acc.to_string().is_empty() {
                *acc = col;
            } else {
                match delimiter {
                    Register::Value(owned_value) => {
                        *acc += owned_value;
                    }
                    _ => unreachable!(),
                }
                *acc += col;
            }
        }
        #[cfg(feature = "json")]
        AggFunc::JsonGroupObject | AggFunc::JsonbGroupObject => {
            let key = state.registers[*col].clone();
            let value = state.registers[*delimiter].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                unreachable!();
            };
            let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            let mut key_vec = convert_dbtype_to_raw_jsonb(&key.get_owned_value())?;
            let mut val_vec = convert_dbtype_to_raw_jsonb(&value.get_owned_value())?;
            match acc {
                Value::Blob(vec) => {
                    if vec.is_empty() {
                        vec.push(12);
                        vec.append(&mut key_vec);
                        vec.append(&mut val_vec);
                    } else {
                        vec.append(&mut key_vec);
                        vec.append(&mut val_vec);
                    }
                }
                _ => unreachable!(),
            };
        }
        #[cfg(feature = "json")]
        AggFunc::JsonGroupArray | AggFunc::JsonbGroupArray => {
            let col = state.registers[*col].clone();
            let Register::Aggregate(agg) = state.registers[*acc_reg].borrow_mut() else {
                unreachable!();
            };
            let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                unreachable!();
            };
            let mut data = convert_dbtype_to_raw_jsonb(&col.get_owned_value())?;
            match acc {
                Value::Blob(vec) => {
                    if vec.is_empty() {
                        vec.push(11);
                        vec.append(&mut data)
                    } else {
                        vec.append(&mut data);
                    }
                }
                _ => unreachable!(),
            };
        }
        AggFunc::External(_) => {
            let (step_fn, state_ptr, argc) = {
                let Register::Aggregate(agg) = &state.registers[*acc_reg] else {
                    unreachable!();
                };
                let AggContext::External(agg_state) = agg else {
                    unreachable!();
                };
                (agg_state.step_fn, agg_state.state, agg_state.argc)
            };
            if argc == 0 {
                unsafe { step_fn(state_ptr, 0, std::ptr::null()) };
            } else {
                let register_slice = &state.registers[*col..*col + argc];
                let mut ext_values: Vec<ExtValue> = Vec::with_capacity(argc);
                for ov in register_slice.iter() {
                    ext_values.push(ov.get_owned_value().to_ffi());
                }
                let argv_ptr = ext_values.as_ptr();
                unsafe { step_fn(state_ptr, argc as i32, argv_ptr) };
                for ext_value in ext_values {
                    unsafe { ext_value.__free_internal_type() };
                }
            }
        }
    };
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_agg_final(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::AggFinal { register, func } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    match state.registers[*register].borrow_mut() {
        Register::Aggregate(agg) => match func {
            AggFunc::Avg => {
                let AggContext::Avg(acc, count) = agg.borrow_mut() else {
                    unreachable!();
                };
                *acc /= count.clone();
                state.registers[*register] = Register::Value(acc.clone());
            }
            AggFunc::Sum | AggFunc::Total => {
                let AggContext::Sum(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                let value = match acc {
                    Value::Integer(i) => Value::Integer(*i),
                    Value::Float(f) => Value::Float(*f),
                    _ => Value::Float(0.0),
                };
                state.registers[*register] = Register::Value(value);
            }
            AggFunc::Count | AggFunc::Count0 => {
                let AggContext::Count(count) = agg.borrow_mut() else {
                    unreachable!();
                };
                state.registers[*register] = Register::Value(count.clone());
            }
            AggFunc::Max => {
                let AggContext::Max(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                match acc {
                    Some(value) => {
                        state.registers[*register] = Register::Value(value.clone());
                    }
                    None => state.registers[*register] = Register::Value(Value::Null),
                }
            }
            AggFunc::Min => {
                let AggContext::Min(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                match acc {
                    Some(value) => {
                        state.registers[*register] = Register::Value(value.clone());
                    }
                    None => state.registers[*register] = Register::Value(Value::Null),
                }
            }
            AggFunc::GroupConcat | AggFunc::StringAgg => {
                let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                state.registers[*register] = Register::Value(acc.clone());
            }
            #[cfg(feature = "json")]
            AggFunc::JsonGroupObject => {
                let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                let data = acc.to_blob().expect("Should be blob");
                state.registers[*register] = Register::Value(json_from_raw_bytes_agg(data, false)?);
            }
            #[cfg(feature = "json")]
            AggFunc::JsonbGroupObject => {
                let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                let data = acc.to_blob().expect("Should be blob");
                state.registers[*register] = Register::Value(json_from_raw_bytes_agg(data, true)?);
            }
            #[cfg(feature = "json")]
            AggFunc::JsonGroupArray => {
                let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                let data = acc.to_blob().expect("Should be blob");
                state.registers[*register] = Register::Value(json_from_raw_bytes_agg(data, false)?);
            }
            #[cfg(feature = "json")]
            AggFunc::JsonbGroupArray => {
                let AggContext::GroupConcat(acc) = agg.borrow_mut() else {
                    unreachable!();
                };
                let data = acc.to_blob().expect("Should be blob");
                state.registers[*register] = Register::Value(json_from_raw_bytes_agg(data, true)?);
            }
            AggFunc::External(_) => {
                agg.compute_external()?;
                let AggContext::External(agg_state) = agg else {
                    unreachable!();
                };
                match &agg_state.finalized_value {
                    Some(value) => {
                        state.registers[*register] = Register::Value(value.clone());
                    }
                    None => state.registers[*register] = Register::Value(Value::Null),
                }
            }
        },
        Register::Value(Value::Null) => match func {
            AggFunc::Total => {
                state.registers[*register] = Register::Value(Value::Float(0.0));
            }
            AggFunc::Count | AggFunc::Count0 => {
                state.registers[*register] = Register::Value(Value::Integer(0));
            }
            _ => {}
        },
        other => {
            panic!("Unexpected value {:?} in AggFinal", other);
        }
    };
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_sorter_open(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SorterOpen {
        cursor_id,
        columns: _,
        order,
        collations,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let cursor = Sorter::new(
        order,
        collations
            .iter()
            .map(|collation| collation.unwrap_or_default())
            .collect(),
    );
    let mut cursors = state.cursors.borrow_mut();
    cursors
        .get_mut(*cursor_id)
        .unwrap()
        .replace(Cursor::new_sorter(cursor));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_sorter_data(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SorterData {
        cursor_id,
        dest_reg,
        pseudo_cursor,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let record = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_sorter_mut();
        cursor.record().map(|r| r.clone())
    };
    let record = match record {
        Some(record) => record,
        None => {
            state.pc += 1;
            return Ok(InsnFunctionStepResult::Step);
        }
    };
    state.registers[*dest_reg] = Register::Record(record.clone());
    {
        let mut pseudo_cursor = state.get_cursor(*pseudo_cursor);
        pseudo_cursor.as_pseudo_mut().insert(record);
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_sorter_insert(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SorterInsert {
        cursor_id,
        record_reg,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_sorter_mut();
        let record = match &state.registers[*record_reg] {
            Register::Record(record) => record,
            _ => unreachable!("SorterInsert on non-record register"),
        };
        cursor.insert(record);
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_sorter_sort(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SorterSort {
        cursor_id,
        pc_if_empty,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let is_empty = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_sorter_mut();
        let is_empty = cursor.is_empty();
        if !is_empty {
            cursor.sort();
        }
        is_empty
    };
    if is_empty {
        state.pc = pc_if_empty.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_sorter_next(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SorterNext {
        cursor_id,
        pc_if_next,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(pc_if_next.is_offset());
    let has_more = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_sorter_mut();
        cursor.next();
        cursor.has_more()
    };
    if has_more {
        state.pc = pc_if_next.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_init_coroutine(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::InitCoroutine {
        yield_reg,
        jump_on_definition,
        start_offset,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    assert!(jump_on_definition.is_offset());
    let start_offset = start_offset.to_offset_int();
    state.registers[*yield_reg] = Register::Value(Value::Integer(start_offset as i64));
    state.ended_coroutine.unset(*yield_reg);
    let jump_on_definition = jump_on_definition.to_offset_int();
    state.pc = if jump_on_definition == 0 {
        state.pc + 1
    } else {
        jump_on_definition
    };
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_end_coroutine(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::EndCoroutine { yield_reg } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Value::Integer(pc) = state.registers[*yield_reg].get_owned_value() {
        state.ended_coroutine.set(*yield_reg);
        let pc: u32 = (*pc)
            .try_into()
            .unwrap_or_else(|_| panic!("EndCoroutine: pc overflow: {}", pc));
        state.pc = pc - 1;
    } else {
        unreachable!();
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_yield(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Yield {
        yield_reg,
        end_offset,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if let Value::Integer(pc) = state.registers[*yield_reg].get_owned_value() {
        if state.ended_coroutine.get(*yield_reg) {
            state.pc = end_offset.to_offset_int();
        } else {
            let pc: u32 = (*pc)
                .try_into()
                .unwrap_or_else(|_| panic!("Yield: pc overflow: {}", pc));
            (state.pc, state.registers[*yield_reg]) =
                (pc, Register::Value(Value::Integer((state.pc + 1) as i64)));
        }
    } else {
        unreachable!(
            "yield_reg {} contains non-integer value: {:?}",
            *yield_reg, state.registers[*yield_reg]
        );
    }
    Ok(InsnFunctionStepResult::Step)
}
