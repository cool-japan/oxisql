#![allow(unused_variables)]
use super::super::likeop::{construct_like_escape_arg, exec_glob, exec_like_with_escape};
use super::super::{Program, ProgramState, Register};
use crate::function::AlterTableFunc;
use crate::info;
use crate::types::Text;
use crate::util::normalize_ident;
use crate::{bail_constraint_error, MvStore, Pager, Result, DATABASE_VERSION};
use crate::{
    error::LimboError,
    ext::ExtValue,
    function::{ExtFunc, MathFunc, MathFuncArity, ScalarFunc, VectorFunc},
    functions::{
        datetime::{
            exec_date, exec_datetime_full, exec_julianday, exec_strftime, exec_time, exec_unixepoch,
        },
        printf::exec_printf,
    },
};
#[cfg(feature = "json")]
use crate::{
    function::JsonFunc, json::get_json, json::is_json_valid, json::json_array,
    json::json_array_length, json::json_arrow_extract, json::json_arrow_shift_extract,
    json::json_error_position, json::json_extract, json::json_insert, json::json_object,
    json::json_patch, json::json_quote, json::json_remove, json::json_replace, json::json_set,
    json::json_type, json::jsonb, json::jsonb_array, json::jsonb_extract, json::jsonb_insert,
    json::jsonb_object, json::jsonb_patch, json::jsonb_remove, json::jsonb_replace,
    json::jsonb_set,
};
use crate::{
    types::{Cursor, Value},
    vdbe::{builder::CursorType, insn::Insn},
    vector::{vector32, vector64, vector_distance_cos, vector_extract},
};
use fallible_iterator::FallibleIterator;
use limbo_sqlite3_parser::ast;
use limbo_sqlite3_parser::ast::fmt::ToTokens;
use limbo_sqlite3_parser::lexer::sql::Parser;
use std::{borrow::BorrowMut, rc::Rc};

use super::numeric::execute_sqlite_version;
use super::values::{exec_char, exec_concat_strings, exec_concat_ws};
use super::InsnFunctionStepResult;

pub fn op_drop_index(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::DropIndex { index, db: _ } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let mut schema = program.connection.schema.write();
    schema.remove_index(&index);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vopen(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VOpen { cursor_id } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let (_, cursor_type) = program.cursor_ref.get(*cursor_id).unwrap();
    let CursorType::VirtualTable(virtual_table) = cursor_type else {
        panic!("VOpen on non-virtual table cursor");
    };
    let cursor = virtual_table.open(program.connection.clone())?;
    state
        .cursors
        .borrow_mut()
        .get_mut(*cursor_id)
        .unwrap_or_else(|| panic!("cursor id {} out of bounds", *cursor_id))
        .replace(Cursor::Virtual(cursor));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vcreate(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VCreate {
        module_name,
        table_name,
        args_reg,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let module_name = state.registers[*module_name].get_owned_value().to_string();
    let table_name = state.registers[*table_name].get_owned_value().to_string();
    let args = if let Some(args_reg) = args_reg {
        if let Register::Record(rec) = &state.registers[*args_reg] {
            rec.get_values().iter().map(|v| v.to_ffi()).collect()
        } else {
            return Err(LimboError::InternalError(
                "VCreate: args_reg is not a record".to_string(),
            ));
        }
    } else {
        vec![]
    };
    let conn = program.connection.clone();
    let table =
        crate::VirtualTable::table(Some(&table_name), &module_name, args, &conn.syms.borrow())?;
    {
        conn.syms
            .borrow_mut()
            .vtabs
            .insert(table_name, table.clone());
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vfilter(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VFilter {
        cursor_id,
        pc_if_empty,
        arg_count,
        args_reg,
        idx_str,
        idx_num,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let has_rows = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_virtual_mut();
        let mut args = Vec::with_capacity(*arg_count);
        for i in 0..*arg_count {
            args.push(state.registers[args_reg + i].get_owned_value().clone());
        }
        let idx_str = if let Some(idx_str) = idx_str {
            Some(state.registers[*idx_str].get_owned_value().to_string())
        } else {
            None
        };
        cursor.filter(*idx_num as i32, idx_str, *arg_count, args)?
    };
    if !has_rows {
        state.pc = pc_if_empty.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vcolumn(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VColumn {
        cursor_id,
        column,
        dest,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let value = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_virtual_mut();
        cursor.column(*column)?
    };
    state.registers[*dest] = Register::Value(value);
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vupdate(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VUpdate {
        cursor_id,
        arg_count,
        start_reg,
        conflict_action,
        ..
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let (_, cursor_type) = program.cursor_ref.get(*cursor_id).unwrap();
    let CursorType::VirtualTable(virtual_table) = cursor_type else {
        panic!("VUpdate on non-virtual table cursor");
    };
    if *arg_count < 2 {
        return Err(LimboError::InternalError(
            "VUpdate: arg_count must be at least 2 (rowid and insert_rowid)".to_string(),
        ));
    }
    let mut argv = Vec::with_capacity(*arg_count);
    for i in 0..*arg_count {
        if let Some(value) = state.registers.get(*start_reg + i) {
            argv.push(value.get_owned_value().clone());
        } else {
            return Err(LimboError::InternalError(format!(
                "VUpdate: register out of bounds at {}",
                *start_reg + i
            )));
        }
    }
    let result = virtual_table.update(&argv);
    match result {
        Ok(Some(new_rowid)) => {
            if *conflict_action == 5 {
                program.connection.update_last_rowid(new_rowid);
            }
            state.pc += 1;
        }
        Ok(None) => {
            state.pc += 1;
        }
        Err(e) => {
            return Err(LimboError::ExtensionError(format!(
                "Virtual table update failed: {}",
                e
            )));
        }
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vnext(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VNext {
        cursor_id,
        pc_if_next,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let has_more = {
        let mut cursor = state.get_cursor(*cursor_id);
        let cursor = cursor.as_virtual_mut();
        cursor.next()?
    };
    if has_more {
        state.pc = pc_if_next.to_offset_int();
    } else {
        state.pc += 1;
    }
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_vdestroy(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::VDestroy { db, table_name } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let conn = program.connection.clone();
    {
        let Some(vtab) = conn.syms.borrow_mut().vtabs.remove(table_name) else {
            return Err(crate::LimboError::InternalError(
                "Could not find Virtual Table to Destroy".to_string(),
            ));
        };
        vtab.destroy()?;
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_function(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Function {
        constant_mask,
        func,
        start_reg,
        dest,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let arg_count = func.arg_count;
    match &func.func {
        #[cfg(feature = "json")]
        crate::function::Func::Json(json_func) => match json_func {
            JsonFunc::Json => {
                let json_value = &state.registers[*start_reg];
                let json_str = get_json(json_value.get_owned_value(), None);
                match json_str {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::Jsonb => {
                let json_value = &state.registers[*start_reg];
                let json_blob = jsonb(json_value.get_owned_value(), &state.json_cache);
                match json_blob {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonArray
            | JsonFunc::JsonObject
            | JsonFunc::JsonbArray
            | JsonFunc::JsonbObject => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                let json_func = match json_func {
                    JsonFunc::JsonArray => json_array,
                    JsonFunc::JsonObject => json_object,
                    JsonFunc::JsonbArray => jsonb_array,
                    JsonFunc::JsonbObject => jsonb_object,
                    _ => unreachable!(),
                };
                let json_result = json_func(reg_values);
                match json_result {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonExtract => {
                let result = match arg_count {
                    0 => Ok(Value::Null),
                    _ => {
                        let val = &state.registers[*start_reg];
                        let reg_values = &state.registers[*start_reg + 1..*start_reg + arg_count];
                        json_extract(val.get_owned_value(), reg_values, &state.json_cache)
                    }
                };
                match result {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonbExtract => {
                let result = match arg_count {
                    0 => Ok(Value::Null),
                    _ => {
                        let val = &state.registers[*start_reg];
                        let reg_values = &state.registers[*start_reg + 1..*start_reg + arg_count];
                        jsonb_extract(val.get_owned_value(), reg_values, &state.json_cache)
                    }
                };
                match result {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonArrowExtract | JsonFunc::JsonArrowShiftExtract => {
                assert_eq!(arg_count, 2);
                let json = &state.registers[*start_reg];
                let path = &state.registers[*start_reg + 1];
                let json_func = match json_func {
                    JsonFunc::JsonArrowExtract => json_arrow_extract,
                    JsonFunc::JsonArrowShiftExtract => json_arrow_shift_extract,
                    _ => unreachable!(),
                };
                let json_str = json_func(
                    json.get_owned_value(),
                    path.get_owned_value(),
                    &state.json_cache,
                );
                match json_str {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonArrayLength | JsonFunc::JsonType => {
                let json_value = &state.registers[*start_reg];
                let path_value = if arg_count > 1 {
                    Some(&state.registers[*start_reg + 1])
                } else {
                    None
                };
                let func_result = match json_func {
                    JsonFunc::JsonArrayLength => json_array_length(
                        json_value.get_owned_value(),
                        path_value.map(|x| x.get_owned_value()),
                        &state.json_cache,
                    ),
                    JsonFunc::JsonType => json_type(
                        json_value.get_owned_value(),
                        path_value.map(|x| x.get_owned_value()),
                    ),
                    _ => unreachable!(),
                };
                match func_result {
                    Ok(result) => state.registers[*dest] = Register::Value(result),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonErrorPosition => {
                let json_value = &state.registers[*start_reg];
                match json_error_position(json_value.get_owned_value()) {
                    Ok(pos) => state.registers[*dest] = Register::Value(pos),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonValid => {
                let json_value = &state.registers[*start_reg];
                state.registers[*dest] =
                    Register::Value(is_json_valid(json_value.get_owned_value()));
            }
            JsonFunc::JsonPatch => {
                assert_eq!(arg_count, 2);
                assert!(*start_reg + 1 < state.registers.len());
                let target = &state.registers[*start_reg];
                let patch = &state.registers[*start_reg + 1];
                state.registers[*dest] = Register::Value(json_patch(
                    target.get_owned_value(),
                    patch.get_owned_value(),
                    &state.json_cache,
                )?);
            }
            JsonFunc::JsonbPatch => {
                assert_eq!(arg_count, 2);
                assert!(*start_reg + 1 < state.registers.len());
                let target = &state.registers[*start_reg];
                let patch = &state.registers[*start_reg + 1];
                state.registers[*dest] = Register::Value(jsonb_patch(
                    target.get_owned_value(),
                    patch.get_owned_value(),
                    &state.json_cache,
                )?);
            }
            JsonFunc::JsonRemove => {
                if let Ok(json) = json_remove(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonbRemove => {
                if let Ok(json) = jsonb_remove(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonReplace => {
                if let Ok(json) = json_replace(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonbReplace => {
                if let Ok(json) = jsonb_replace(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonInsert => {
                if let Ok(json) = json_insert(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonbInsert => {
                if let Ok(json) = jsonb_insert(
                    &state.registers[*start_reg..*start_reg + arg_count],
                    &state.json_cache,
                ) {
                    state.registers[*dest] = Register::Value(json);
                } else {
                    state.registers[*dest] = Register::Value(Value::Null);
                }
            }
            JsonFunc::JsonPretty => {
                let json_value = &state.registers[*start_reg];
                let indent = if arg_count > 1 {
                    Some(&state.registers[*start_reg + 1])
                } else {
                    None
                };
                let indent = match indent {
                    Some(value) => match value.get_owned_value() {
                        Value::Text(text) => text.as_str(),
                        Value::Integer(val) => &val.to_string(),
                        Value::Float(val) => &val.to_string(),
                        Value::Blob(val) => &String::from_utf8_lossy(val),
                        _ => "    ",
                    },
                    None => "    ",
                };
                let json_str = get_json(json_value.get_owned_value(), Some(indent))?;
                state.registers[*dest] = Register::Value(json_str);
            }
            JsonFunc::JsonSet => {
                if arg_count % 2 == 0 {
                    bail_constraint_error!("json_set() needs an odd number of arguments")
                }
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                let json_result = json_set(reg_values, &state.json_cache);
                match json_result {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonbSet => {
                if arg_count % 2 == 0 {
                    bail_constraint_error!("json_set() needs an odd number of arguments")
                }
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                let json_result = jsonb_set(reg_values, &state.json_cache);
                match json_result {
                    Ok(json) => state.registers[*dest] = Register::Value(json),
                    Err(e) => return Err(e),
                }
            }
            JsonFunc::JsonQuote => {
                let json_value = &state.registers[*start_reg];
                match json_quote(json_value.get_owned_value()) {
                    Ok(result) => state.registers[*dest] = Register::Value(result),
                    Err(e) => return Err(e),
                }
            }
        },
        crate::function::Func::Scalar(scalar_func) => match scalar_func {
            ScalarFunc::Cast => {
                assert_eq!(arg_count, 2);
                assert!(*start_reg + 1 < state.registers.len());
                let reg_value_argument = state.registers[*start_reg].clone();
                let Value::Text(reg_value_type) =
                    state.registers[*start_reg + 1].get_owned_value().clone()
                else {
                    unreachable!("Cast with non-text type");
                };
                let result = reg_value_argument
                    .get_owned_value()
                    .exec_cast(reg_value_type.as_str());
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Changes => {
                let res = &program.connection.last_change;
                let changes = res.get();
                state.registers[*dest] = Register::Value(Value::Integer(changes));
            }
            ScalarFunc::Char => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                state.registers[*dest] = Register::Value(exec_char(reg_values));
            }
            ScalarFunc::Coalesce => {}
            ScalarFunc::Concat => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                let result = exec_concat_strings(reg_values);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::ConcatWs => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                let result = exec_concat_ws(reg_values);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Glob => {
                let pattern = &state.registers[*start_reg];
                let text = &state.registers[*start_reg + 1];
                let result = match (pattern.get_owned_value(), text.get_owned_value()) {
                    (Value::Text(pattern), Value::Text(text)) => {
                        let cache = if *constant_mask > 0 {
                            Some(&mut state.regex_cache.glob)
                        } else {
                            None
                        };
                        Value::Integer(exec_glob(cache, pattern.as_str(), text.as_str()) as i64)
                    }
                    _ => {
                        unreachable!("Like on non-text registers");
                    }
                };
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::IfNull => {}
            ScalarFunc::Iif => {}
            ScalarFunc::Instr => {
                let reg_value = &state.registers[*start_reg];
                let pattern_value = &state.registers[*start_reg + 1];
                let result = reg_value
                    .get_owned_value()
                    .exec_instr(pattern_value.get_owned_value());
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::LastInsertRowid => {
                state.registers[*dest] =
                    Register::Value(Value::Integer(program.connection.last_insert_rowid() as i64));
            }
            ScalarFunc::Like => {
                let pattern = &state.registers[*start_reg];
                let match_expression = &state.registers[*start_reg + 1];
                let pattern = match pattern.get_owned_value() {
                    Value::Text(_) => pattern.get_owned_value(),
                    _ => &pattern.get_owned_value().exec_cast("TEXT"),
                };
                let match_expression = match match_expression.get_owned_value() {
                    Value::Text(_) => match_expression.get_owned_value(),
                    _ => &match_expression.get_owned_value().exec_cast("TEXT"),
                };
                let result = match (pattern, match_expression) {
                    (Value::Text(pattern), Value::Text(match_expression)) if arg_count == 3 => {
                        let escape = match construct_like_escape_arg(
                            state.registers[*start_reg + 2].get_owned_value(),
                        ) {
                            Ok(x) => x,
                            Err(e) => return Err(e),
                        };
                        Value::Integer(exec_like_with_escape(
                            pattern.as_str(),
                            match_expression.as_str(),
                            escape,
                        ) as i64)
                    }
                    (Value::Text(pattern), Value::Text(match_expression)) => {
                        let cache = if *constant_mask > 0 {
                            Some(&mut state.regex_cache.like)
                        } else {
                            None
                        };
                        Value::Integer(Value::exec_like(
                            cache,
                            pattern.as_str(),
                            match_expression.as_str(),
                        ) as i64)
                    }
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    _ => {
                        unreachable!("Like failed");
                    }
                };
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Abs
            | ScalarFunc::Lower
            | ScalarFunc::Upper
            | ScalarFunc::Length
            | ScalarFunc::OctetLength
            | ScalarFunc::Typeof
            | ScalarFunc::Unicode
            | ScalarFunc::Quote
            | ScalarFunc::RandomBlob
            | ScalarFunc::Sign
            | ScalarFunc::Soundex
            | ScalarFunc::ZeroBlob => {
                let reg_value = state.registers[*start_reg].borrow_mut().get_owned_value();
                let result = match scalar_func {
                    ScalarFunc::Sign => reg_value.exec_sign(),
                    ScalarFunc::Abs => Some(reg_value.exec_abs()?),
                    ScalarFunc::Lower => reg_value.exec_lower(),
                    ScalarFunc::Upper => reg_value.exec_upper(),
                    ScalarFunc::Length => Some(reg_value.exec_length()),
                    ScalarFunc::OctetLength => Some(reg_value.exec_octet_length()),
                    ScalarFunc::Typeof => Some(reg_value.exec_typeof()),
                    ScalarFunc::Unicode => Some(reg_value.exec_unicode()),
                    ScalarFunc::Quote => Some(reg_value.exec_quote()),
                    ScalarFunc::RandomBlob => Some(reg_value.exec_randomblob()),
                    ScalarFunc::ZeroBlob => Some(reg_value.exec_zeroblob()),
                    ScalarFunc::Soundex => Some(reg_value.exec_soundex()),
                    _ => unreachable!(),
                };
                state.registers[*dest] = Register::Value(result.unwrap_or(Value::Null));
            }
            ScalarFunc::Hex => {
                let reg_value = state.registers[*start_reg].borrow_mut();
                let result = reg_value.get_owned_value().exec_hex();
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Unhex => {
                let reg_value = &state.registers[*start_reg];
                let ignored_chars = state.registers.get(*start_reg + 1);
                let result = reg_value
                    .get_owned_value()
                    .exec_unhex(ignored_chars.map(|x| x.get_owned_value()));
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Random => {
                state.registers[*dest] = Register::Value(Value::exec_random());
            }
            ScalarFunc::Trim => {
                let reg_value = &state.registers[*start_reg];
                let pattern_value = if func.arg_count == 2 {
                    state.registers.get(*start_reg + 1)
                } else {
                    None
                };
                let result = reg_value
                    .get_owned_value()
                    .exec_trim(pattern_value.map(|x| x.get_owned_value()));
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::LTrim => {
                let reg_value = &state.registers[*start_reg];
                let pattern_value = if func.arg_count == 2 {
                    state.registers.get(*start_reg + 1)
                } else {
                    None
                };
                let result = reg_value
                    .get_owned_value()
                    .exec_ltrim(pattern_value.map(|x| x.get_owned_value()));
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::RTrim => {
                let reg_value = &state.registers[*start_reg];
                let pattern_value = if func.arg_count == 2 {
                    state.registers.get(*start_reg + 1)
                } else {
                    None
                };
                let result = reg_value
                    .get_owned_value()
                    .exec_rtrim(pattern_value.map(|x| x.get_owned_value()));
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Round => {
                let reg_value = &state.registers[*start_reg];
                assert!(arg_count == 1 || arg_count == 2);
                let precision_value = if arg_count > 1 {
                    state.registers.get(*start_reg + 1)
                } else {
                    None
                };
                let result = reg_value
                    .get_owned_value()
                    .exec_round(precision_value.map(|x| x.get_owned_value()));
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Min => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                state.registers[*dest] = Register::Value(Value::exec_min(
                    reg_values.iter().map(|v| v.get_owned_value()),
                ));
            }
            ScalarFunc::Max => {
                let reg_values = &state.registers[*start_reg..*start_reg + arg_count];
                state.registers[*dest] = Register::Value(Value::exec_max(
                    reg_values.iter().map(|v| v.get_owned_value()),
                ));
            }
            ScalarFunc::Nullif => {
                let first_value = &state.registers[*start_reg];
                let second_value = &state.registers[*start_reg + 1];
                state.registers[*dest] = Register::Value(Value::exec_nullif(
                    first_value.get_owned_value(),
                    second_value.get_owned_value(),
                ));
            }
            ScalarFunc::Substr | ScalarFunc::Substring => {
                let str_value = &state.registers[*start_reg];
                let start_value = &state.registers[*start_reg + 1];
                let length_value = if func.arg_count == 3 {
                    Some(&state.registers[*start_reg + 2])
                } else {
                    None
                };
                let result = Value::exec_substring(
                    str_value.get_owned_value(),
                    start_value.get_owned_value(),
                    length_value.map(|x| x.get_owned_value()),
                );
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Date => {
                let result = exec_date(&state.registers[*start_reg..*start_reg + arg_count]);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Time => {
                let values = &state.registers[*start_reg..*start_reg + arg_count];
                let result = exec_time(values);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::TimeDiff => {
                if arg_count != 2 {
                    state.registers[*dest] = Register::Value(Value::Null);
                } else {
                    let start = state.registers[*start_reg].get_owned_value().clone();
                    let end = state.registers[*start_reg + 1].get_owned_value().clone();
                    let result = crate::functions::datetime::exec_timediff(&[
                        Register::Value(start),
                        Register::Value(end),
                    ]);
                    state.registers[*dest] = Register::Value(result);
                }
            }
            ScalarFunc::TotalChanges => {
                let res = &program.connection.total_changes;
                let total_changes = res.get();
                state.registers[*dest] = Register::Value(Value::Integer(total_changes));
            }
            ScalarFunc::DateTime => {
                let result =
                    exec_datetime_full(&state.registers[*start_reg..*start_reg + arg_count]);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::JulianDay => {
                let result = exec_julianday(&state.registers[*start_reg..*start_reg + arg_count]);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::UnixEpoch => {
                if *start_reg == 0 {
                    let unixepoch: String = exec_unixepoch(&Value::build_text("now"))?;
                    state.registers[*dest] = Register::Value(Value::build_text(&unixepoch));
                } else {
                    let datetime_value = &state.registers[*start_reg];
                    let unixepoch = exec_unixepoch(datetime_value.get_owned_value());
                    match unixepoch {
                        Ok(time) => {
                            state.registers[*dest] = Register::Value(Value::build_text(&time));
                        }
                        Err(e) => {
                            return Err(LimboError::ParseError(format!(
                                "Error encountered while parsing datetime value: {}",
                                e
                            )));
                        }
                    }
                }
            }
            ScalarFunc::SqliteVersion => {
                let version_integer: i64 = DATABASE_VERSION.get().unwrap().parse()?;
                let version = execute_sqlite_version(version_integer);
                state.registers[*dest] = Register::Value(Value::build_text(&version));
            }
            ScalarFunc::SqliteSourceId => {
                let src_id = format!(
                    "{} {}",
                    info::build::BUILT_TIME_SQLITE,
                    info::build::GIT_COMMIT_HASH.unwrap_or("unknown")
                );
                state.registers[*dest] = Register::Value(Value::build_text(&src_id));
            }
            ScalarFunc::Replace => {
                assert_eq!(arg_count, 3);
                let source = &state.registers[*start_reg];
                let pattern = &state.registers[*start_reg + 1];
                let replacement = &state.registers[*start_reg + 2];
                state.registers[*dest] = Register::Value(Value::exec_replace(
                    source.get_owned_value(),
                    pattern.get_owned_value(),
                    replacement.get_owned_value(),
                ));
            }
            #[cfg(feature = "fs")]
            ScalarFunc::LoadExtension => {
                #[cfg(feature = "load-extension")]
                {
                    let extension = &state.registers[*start_reg];
                    let ext = crate::resolve_ext_path(&extension.get_owned_value().to_string())?;
                    program.connection.load_extension(ext)?;
                }
                #[cfg(not(feature = "load-extension"))]
                {
                    return Err(crate::LimboError::ExtensionError(
                        "load_extension() requires the `load-extension` feature to be enabled"
                            .to_string(),
                    ));
                }
            }
            ScalarFunc::StrfTime => {
                let result = exec_strftime(&state.registers[*start_reg..*start_reg + arg_count]);
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Printf => {
                let result = exec_printf(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Likely => {
                let value = &state.registers[*start_reg].borrow_mut();
                let result = value.get_owned_value().exec_likely();
                state.registers[*dest] = Register::Value(result);
            }
            ScalarFunc::Likelihood => {
                assert_eq!(arg_count, 2);
                let value = &state.registers[*start_reg];
                let probability = &state.registers[*start_reg + 1];
                let result = value
                    .get_owned_value()
                    .exec_likelihood(probability.get_owned_value());
                state.registers[*dest] = Register::Value(result);
            }
        },
        crate::function::Func::Vector(vector_func) => match vector_func {
            VectorFunc::Vector => {
                let result = vector32(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
            VectorFunc::Vector32 => {
                let result = vector32(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
            VectorFunc::Vector64 => {
                let result = vector64(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
            VectorFunc::VectorExtract => {
                let result = vector_extract(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
            VectorFunc::VectorDistanceCos => {
                let result =
                    vector_distance_cos(&state.registers[*start_reg..*start_reg + arg_count])?;
                state.registers[*dest] = Register::Value(result);
            }
        },
        crate::function::Func::External(f) => match f.func {
            ExtFunc::Scalar(f) => {
                if arg_count == 0 {
                    let result_c_value: ExtValue = unsafe { (f)(0, std::ptr::null()) };
                    match Value::from_ffi(result_c_value) {
                        Ok(result_ov) => {
                            state.registers[*dest] = Register::Value(result_ov);
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                } else {
                    let register_slice = &state.registers[*start_reg..*start_reg + arg_count];
                    let mut ext_values: Vec<ExtValue> = Vec::with_capacity(arg_count);
                    for ov in register_slice.iter() {
                        let val = ov.get_owned_value().to_ffi();
                        ext_values.push(val);
                    }
                    let argv_ptr = ext_values.as_ptr();
                    let result_c_value: ExtValue = unsafe { (f)(arg_count as i32, argv_ptr) };
                    match Value::from_ffi(result_c_value) {
                        Ok(result_ov) => {
                            state.registers[*dest] = Register::Value(result_ov);
                        }
                        Err(e) => {
                            return Err(e);
                        }
                    }
                }
            }
            _ => unreachable!("aggregate called in scalar context"),
        },
        crate::function::Func::Math(math_func) => match math_func.arity() {
            MathFuncArity::Nullary => match math_func {
                MathFunc::Pi => {
                    state.registers[*dest] = Register::Value(Value::Float(std::f64::consts::PI));
                }
                _ => {
                    unreachable!("Unexpected mathematical Nullary function {:?}", math_func);
                }
            },
            MathFuncArity::Unary => {
                let reg_value = &state.registers[*start_reg];
                let result = reg_value.get_owned_value().exec_math_unary(math_func);
                state.registers[*dest] = Register::Value(result);
            }
            MathFuncArity::Binary => {
                let lhs = &state.registers[*start_reg];
                let rhs = &state.registers[*start_reg + 1];
                let result = lhs
                    .get_owned_value()
                    .exec_math_binary(rhs.get_owned_value(), math_func);
                state.registers[*dest] = Register::Value(result);
            }
            MathFuncArity::UnaryOrBinary => match math_func {
                MathFunc::Log => {
                    let result = match arg_count {
                        1 => {
                            let arg = &state.registers[*start_reg];
                            arg.get_owned_value().exec_math_log(None)
                        }
                        2 => {
                            let base = &state.registers[*start_reg];
                            let arg = &state.registers[*start_reg + 1];
                            arg.get_owned_value()
                                .exec_math_log(Some(base.get_owned_value()))
                        }
                        _ => {
                            unreachable!(
                                "{:?} function with unexpected number of arguments",
                                math_func
                            )
                        }
                    };
                    state.registers[*dest] = Register::Value(result);
                }
                _ => {
                    unreachable!(
                        "Unexpected mathematical UnaryOrBinary function {:?}",
                        math_func
                    )
                }
            },
        },
        crate::function::Func::AlterTable(alter_func) => {
            let r#type = &state.registers[*start_reg + 0].get_owned_value().clone();
            let Value::Text(name) = &state.registers[*start_reg + 1].get_owned_value() else {
                panic!("sqlite_schema.name should be TEXT")
            };
            let name = name.to_string();
            let Value::Text(tbl_name) = &state.registers[*start_reg + 2].get_owned_value() else {
                panic!("sqlite_schema.tbl_name should be TEXT")
            };
            let tbl_name = tbl_name.to_string();
            let Value::Integer(root_page) =
                &state.registers[*start_reg + 3].get_owned_value().clone()
            else {
                panic!("sqlite_schema.root_page should be INTEGER")
            };
            let sql = &state.registers[*start_reg + 4].get_owned_value().clone();
            let (new_name, new_tbl_name, new_sql) = match alter_func {
                AlterTableFunc::RenameTable => {
                    let rename_from = {
                        match &state.registers[*start_reg + 5].get_owned_value() {
                            Value::Text(rename_from) => normalize_ident(rename_from.as_str()),
                            _ => panic!("rename_from parameter should be TEXT"),
                        }
                    };
                    let rename_to = {
                        match &state.registers[*start_reg + 6].get_owned_value() {
                            Value::Text(rename_to) => normalize_ident(rename_to.as_str()),
                            _ => panic!("rename_to parameter should be TEXT"),
                        }
                    };
                    let new_name = if let Some(column) =
                        &name.strip_prefix(&format!("sqlite_autoindex_{rename_from}_"))
                    {
                        format!("sqlite_autoindex_{rename_to}_{column}")
                    } else if name == rename_from {
                        rename_to.clone()
                    } else {
                        name
                    };
                    let new_tbl_name = if tbl_name == rename_from {
                        rename_to.clone()
                    } else {
                        tbl_name
                    };
                    let new_sql = 'sql: {
                        let Value::Text(sql) = sql else {
                            break 'sql None;
                        };
                        let mut parser = Parser::new(sql.as_str().as_bytes());
                        let ast::Cmd::Stmt(stmt) = parser.next().unwrap().unwrap() else {
                            todo!()
                        };
                        match stmt {
                            ast::Stmt::CreateIndex {
                                unique,
                                if_not_exists,
                                idx_name,
                                tbl_name,
                                columns,
                                where_clause,
                            } => {
                                let table_name = normalize_ident(&tbl_name.0);
                                if rename_from != table_name {
                                    break 'sql None;
                                }
                                Some(
                                    ast::Stmt::CreateIndex {
                                        unique,
                                        if_not_exists,
                                        idx_name,
                                        tbl_name: ast::Name(rename_to),
                                        columns,
                                        where_clause,
                                    }
                                    .format()
                                    .unwrap(),
                                )
                            }
                            ast::Stmt::CreateTable {
                                temporary,
                                if_not_exists,
                                tbl_name,
                                body,
                            } => {
                                let table_name = normalize_ident(&tbl_name.name.0);
                                if rename_from != table_name {
                                    break 'sql None;
                                }
                                Some(
                                    ast::Stmt::CreateTable {
                                        temporary,
                                        if_not_exists,
                                        tbl_name: ast::QualifiedName {
                                            db_name: None,
                                            name: ast::Name(rename_to),
                                            alias: None,
                                        },
                                        body,
                                    }
                                    .format()
                                    .unwrap(),
                                )
                            }
                            _ => todo!(),
                        }
                    };
                    (new_name, new_tbl_name, new_sql)
                }
                AlterTableFunc::RenameColumn => {
                    let table = {
                        match &state.registers[*start_reg + 5].get_owned_value() {
                            Value::Text(rename_to) => normalize_ident(rename_to.as_str()),
                            _ => panic!("table parameter should be TEXT"),
                        }
                    };
                    let rename_from = {
                        match &state.registers[*start_reg + 6].get_owned_value() {
                            Value::Text(rename_from) => normalize_ident(rename_from.as_str()),
                            _ => panic!("rename_from parameter should be TEXT"),
                        }
                    };
                    let rename_to = {
                        match &state.registers[*start_reg + 7].get_owned_value() {
                            Value::Text(rename_to) => normalize_ident(rename_to.as_str()),
                            _ => panic!("rename_to parameter should be TEXT"),
                        }
                    };
                    let new_sql = 'sql: {
                        if table != tbl_name {
                            break 'sql None;
                        }
                        let Value::Text(sql) = sql else {
                            break 'sql None;
                        };
                        let mut parser = Parser::new(sql.as_str().as_bytes());
                        let ast::Cmd::Stmt(stmt) = parser.next().unwrap().unwrap() else {
                            todo!()
                        };
                        match stmt {
                            ast::Stmt::CreateIndex {
                                unique,
                                if_not_exists,
                                idx_name,
                                tbl_name,
                                mut columns,
                                where_clause,
                            } => {
                                if table != normalize_ident(&tbl_name.0) {
                                    break 'sql None;
                                }
                                for column in &mut columns {
                                    match &mut column.expr {
                                        ast::Expr::Id(ast::Id(id))
                                            if normalize_ident(&id) == rename_from =>
                                        {
                                            *id = rename_to.clone();
                                        }
                                        _ => {}
                                    }
                                }
                                Some(
                                    ast::Stmt::CreateIndex {
                                        unique,
                                        if_not_exists,
                                        idx_name,
                                        tbl_name,
                                        columns,
                                        where_clause,
                                    }
                                    .format()
                                    .unwrap(),
                                )
                            }
                            ast::Stmt::CreateTable {
                                temporary,
                                if_not_exists,
                                tbl_name,
                                body,
                            } => {
                                if table != normalize_ident(&tbl_name.name.0) {
                                    break 'sql None;
                                }
                                let ast::CreateTableBody::ColumnsAndConstraints {
                                    mut columns,
                                    constraints,
                                    options,
                                } = *body
                                else {
                                    todo!()
                                };
                                let column_index = columns
                                    .get_index_of(&ast::Name(rename_from))
                                    .expect("column being renamed should be present");
                                let mut column_definition =
                                    columns.get_index(column_index).unwrap().1.clone();
                                column_definition.col_name = ast::Name(rename_to.clone());
                                assert!(columns
                                    .insert(ast::Name(rename_to), column_definition.clone())
                                    .is_none());
                                columns.swap_remove_index(column_index).unwrap();
                                Some(
                                    ast::Stmt::CreateTable {
                                        temporary,
                                        if_not_exists,
                                        tbl_name,
                                        body: Box::new(
                                            ast::CreateTableBody::ColumnsAndConstraints {
                                                columns,
                                                constraints,
                                                options,
                                            },
                                        ),
                                    }
                                    .format()
                                    .unwrap(),
                                )
                            }
                            _ => todo!(),
                        }
                    };
                    (name, tbl_name, new_sql)
                }
            };
            state.registers[*dest + 0] = Register::Value(r#type.clone());
            state.registers[*dest + 1] = Register::Value(Value::Text(Text::from(new_name)));
            state.registers[*dest + 2] = Register::Value(Value::Text(Text::from(new_tbl_name)));
            state.registers[*dest + 3] = Register::Value(Value::Integer(*root_page));
            if let Some(new_sql) = new_sql {
                state.registers[*dest + 4] = Register::Value(Value::Text(Text::from(new_sql)));
            } else {
                state.registers[*dest + 4] = Register::Value(sql.clone());
            }
        }
        crate::function::Func::Agg(_) => {
            unreachable!("Aggregate functions should not be handled here")
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
