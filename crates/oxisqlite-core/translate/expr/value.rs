//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

#[cfg(feature = "json")]
use crate::function::JsonFunc;
use crate::function::{Func, FuncCtx, MathFuncArity, ScalarFunc, VectorFunc};
use crate::functions::datetime;
use crate::schema::Table;
use crate::translate::collate::CollationSeq;
use crate::translate::emitter::Resolver;
use crate::translate::optimizer::Optimizable;
use crate::translate::plan::TableReferences;
use crate::translate::subquery::{
    emit_in_subquery, emit_scalar_or_exists_subquery, plan_expr_subquery, ExprSubqueryKind,
};
use crate::util::{exprs_are_equivalent, normalize_ident, parse_numeric_literal};
use crate::vdbe::builder::CursorKey;
use crate::vdbe::{
    builder::ProgramBuilder,
    insn::{CmpInsFlags, Insn},
};
use crate::{Result, Value};
use limbo_sqlite3_parser::ast::{self, UnaryOperator};

use super::binary_emit::{
    emit_binary_insn, maybe_apply_affinity, sanitize_string, translate_function,
    translate_in_list_comparison, translate_like_base,
};
use super::condition::translate_expr_no_constant_opt;
use super::types::NoConstantOptReason;

/// Translate an expression into bytecode.
pub fn translate_expr(
    program: &mut ProgramBuilder,
    referenced_tables: Option<&TableReferences>,
    expr: &ast::Expr,
    target_register: usize,
    resolver: &Resolver,
) -> Result<usize> {
    let constant_span = if expr.is_constant(resolver) {
        if !program.constant_span_is_open() {
            Some(program.constant_span_start())
        } else {
            None
        }
    } else {
        program.constant_span_end_all();
        None
    };

    if let Some(reg) = resolver.resolve_cached_expr_reg(expr) {
        program.emit_insn(Insn::Copy {
            src_reg: reg,
            dst_reg: target_register,
            amount: 0,
        });
        if let Some(span) = constant_span {
            program.constant_span_end(span);
        }
        return Ok(target_register);
    }

    match expr {
        ast::Expr::Between { .. } => {
            unreachable!("expression should have been rewritten in optmizer")
        }
        ast::Expr::Binary(e1, op, e2) => {
            // Check if both sides of the expression are equivalent and reuse the same register if so
            if exprs_are_equivalent(e1, e2) {
                let shared_reg = program.alloc_register();
                translate_expr(program, referenced_tables, e1, shared_reg, resolver)?;

                emit_binary_insn(
                    program,
                    op,
                    shared_reg,
                    shared_reg,
                    target_register,
                    e1,
                    e2,
                    referenced_tables,
                )?;
                program.reset_collation();
                Ok(target_register)
            } else {
                let e1_reg = program.alloc_registers(2);
                let e2_reg = e1_reg + 1;

                translate_expr(program, referenced_tables, e1, e1_reg, resolver)?;
                let left_collation_ctx = program.curr_collation_ctx();
                program.reset_collation();

                translate_expr(program, referenced_tables, e2, e2_reg, resolver)?;
                let right_collation_ctx = program.curr_collation_ctx();
                program.reset_collation();

                /*
                 * The rules for determining which collating function to use for a binary comparison
                 * operator (=, <, >, <=, >=, !=, IS, and IS NOT) are as follows:
                 *
                 * 1. If either operand has an explicit collating function assignment using the postfix COLLATE operator,
                 * then the explicit collating function is used for comparison,
                 * with precedence to the collating function of the left operand.
                 *
                 * 2. If either operand is a column, then the collating function of that column is used
                 * with precedence to the left operand. For the purposes of the previous sentence,
                 * a column name preceded by one or more unary "+" operators and/or CAST operators is still considered a column name.
                 *
                 * 3. Otherwise, the BINARY collating function is used for comparison.
                 */
                let collation_ctx = {
                    match (left_collation_ctx, right_collation_ctx) {
                        (Some((c_left, true)), _) => Some((c_left, true)),
                        (_, Some((c_right, true))) => Some((c_right, true)),
                        (Some((c_left, from_collate_left)), None) => {
                            Some((c_left, from_collate_left))
                        }
                        (None, Some((c_right, from_collate_right))) => {
                            Some((c_right, from_collate_right))
                        }
                        (Some((c_left, from_collate_left)), Some((_, false))) => {
                            Some((c_left, from_collate_left))
                        }
                        _ => None,
                    }
                };
                program.set_collation(collation_ctx);

                emit_binary_insn(
                    program,
                    op,
                    e1_reg,
                    e2_reg,
                    target_register,
                    e1,
                    e2,
                    referenced_tables,
                )?;
                program.reset_collation();
                Ok(target_register)
            }
        }
        ast::Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            // There's two forms of CASE, one which checks a base expression for equality
            // against the WHEN values, and returns the corresponding THEN value if it matches:
            //   CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'many' END
            // And one which evaluates a series of boolean predicates:
            //   CASE WHEN is_good THEN 'good' WHEN is_bad THEN 'bad' ELSE 'okay' END
            // This just changes which sort of branching instruction to issue, after we
            // generate the expression if needed.
            let return_label = program.allocate_label();
            let mut next_case_label = program.allocate_label();
            // Only allocate a reg to hold the base expression if one was provided.
            // And base_reg then becomes the flag we check to see which sort of
            // case statement we're processing.
            let base_reg = base.as_ref().map(|_| program.alloc_register());
            let expr_reg = program.alloc_register();
            if let Some(base_expr) = base {
                translate_expr(
                    program,
                    referenced_tables,
                    base_expr,
                    base_reg.unwrap(),
                    resolver,
                )?;
            };
            for (when_expr, then_expr) in when_then_pairs {
                translate_expr_no_constant_opt(
                    program,
                    referenced_tables,
                    when_expr,
                    expr_reg,
                    resolver,
                    NoConstantOptReason::RegisterReuse,
                )?;
                match base_reg {
                    // CASE 1 WHEN 0 THEN 0 ELSE 1 becomes 1==0, Ne branch to next clause
                    Some(base_reg) => program.emit_insn(Insn::Ne {
                        lhs: base_reg,
                        rhs: expr_reg,
                        target_pc: next_case_label,
                        // A NULL result is considered untrue when evaluating WHEN terms.
                        flags: CmpInsFlags::default().jump_if_null(),
                        collation: program.curr_collation(),
                    }),
                    // CASE WHEN 0 THEN 0 ELSE 1 becomes ifnot 0 branch to next clause
                    None => program.emit_insn(Insn::IfNot {
                        reg: expr_reg,
                        target_pc: next_case_label,
                        jump_if_null: true,
                    }),
                };
                // THEN...
                translate_expr_no_constant_opt(
                    program,
                    referenced_tables,
                    then_expr,
                    target_register,
                    resolver,
                    NoConstantOptReason::RegisterReuse,
                )?;
                program.emit_insn(Insn::Goto {
                    target_pc: return_label,
                });
                // This becomes either the next WHEN, or in the last WHEN/THEN, we're
                // assured to have at least one instruction corresponding to the ELSE immediately follow.
                program.preassign_label_to_next_insn(next_case_label);
                next_case_label = program.allocate_label();
            }
            match else_expr {
                Some(expr) => {
                    translate_expr_no_constant_opt(
                        program,
                        referenced_tables,
                        expr,
                        target_register,
                        resolver,
                        NoConstantOptReason::RegisterReuse,
                    )?;
                }
                // If ELSE isn't specified, it means ELSE null.
                None => {
                    program.emit_insn(Insn::Null {
                        dest: target_register,
                        dest_end: None,
                    });
                }
            };
            program.preassign_label_to_next_insn(return_label);
            Ok(target_register)
        }
        ast::Expr::Cast { expr, type_name } => {
            let type_name = type_name.as_ref().unwrap(); // TODO: why is this optional?
            let reg_expr = program.alloc_registers(2);
            translate_expr(program, referenced_tables, expr, reg_expr, resolver)?;
            program.emit_insn(Insn::String8 {
                // we make a comparison against uppercase static strs in the affinity() function,
                // so we need to make sure we're comparing against the uppercase version,
                // and it's better to do this once instead of every time we check affinity
                value: type_name.name.to_uppercase(),
                dest: reg_expr + 1,
            });
            program.mark_last_insn_constant();
            program.emit_insn(Insn::Function {
                constant_mask: 0,
                start_reg: reg_expr,
                dest: target_register,
                func: FuncCtx {
                    func: Func::Scalar(ScalarFunc::Cast),
                    arg_count: 2,
                },
            });
            Ok(target_register)
        }
        ast::Expr::Collate(expr, collation) => {
            // First translate inner expr, then set the curr collation. If we set curr collation before,
            // it may be overwritten later by inner translate.
            translate_expr(program, referenced_tables, expr, target_register, resolver)?;
            let collation = CollationSeq::new(collation)?;
            program.set_collation(Some((collation, true)));
            Ok(target_register)
        }
        ast::Expr::DoublyQualified(..) => {
            unreachable!("DoublyQualified should be resolved to a Column before translation")
        }
        ast::Expr::Exists(select) => {
            let outer = referenced_tables.ok_or_else(|| {
                crate::error::LimboError::ParseError(
                    "EXISTS subquery requires an enclosing query with table references".to_string(),
                )
            })?;
            let mut planned = plan_expr_subquery(program, resolver, outer, select)?;
            emit_scalar_or_exists_subquery(
                program,
                resolver,
                &mut planned,
                ExprSubqueryKind::Exists,
                target_register,
            )?;
            Ok(target_register)
        }
        ast::Expr::FunctionCall {
            name,
            distinctness: _,
            args,
            filter_over: _,
            order_by: _,
        } => {
            let args_count = if let Some(args) = args { args.len() } else { 0 };
            let func_name = normalize_ident(name.0.as_str());
            let func_type = resolver.resolve_function(&func_name, args_count);

            if func_type.is_none() {
                crate::bail_parse_error!("unknown function {}", name.0);
            }

            let func_ctx = FuncCtx {
                func: func_type.unwrap(),
                arg_count: args_count,
            };

            match &func_ctx.func {
                Func::Agg(_) => {
                    crate::bail_parse_error!("aggregation function in non-aggregation context")
                }
                Func::External(_) => {
                    let regs = program.alloc_registers(args_count);
                    if let Some(args) = args {
                        for (i, arg_expr) in args.iter().enumerate() {
                            translate_expr(
                                program,
                                referenced_tables,
                                arg_expr,
                                regs + i,
                                resolver,
                            )?;
                        }
                    }
                    program.emit_insn(Insn::Function {
                        constant_mask: 0,
                        start_reg: regs,
                        dest: target_register,
                        func: func_ctx,
                    });

                    Ok(target_register)
                }
                #[cfg(feature = "json")]
                Func::Json(j) => match j {
                    JsonFunc::Json | JsonFunc::Jsonb => {
                        let args = expect_arguments_exact!(args, 1, j);

                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                    JsonFunc::JsonArray
                    | JsonFunc::JsonbArray
                    | JsonFunc::JsonExtract
                    | JsonFunc::JsonSet
                    | JsonFunc::JsonbSet
                    | JsonFunc::JsonbExtract
                    | JsonFunc::JsonReplace
                    | JsonFunc::JsonbReplace
                    | JsonFunc::JsonbRemove
                    | JsonFunc::JsonInsert
                    | JsonFunc::JsonbInsert => translate_function(
                        program,
                        args.as_deref().unwrap_or_default(),
                        referenced_tables,
                        resolver,
                        target_register,
                        func_ctx,
                    ),
                    JsonFunc::JsonArrowExtract | JsonFunc::JsonArrowShiftExtract => {
                        unreachable!(
                            "These two functions are only reachable via the -> and ->> operators"
                        )
                    }
                    JsonFunc::JsonArrayLength | JsonFunc::JsonType => {
                        let args = expect_arguments_max!(args, 2, j);

                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                    JsonFunc::JsonErrorPosition => {
                        let args = if let Some(args) = args {
                            if args.len() != 1 {
                                crate::bail_parse_error!(
                                    "{} function with not exactly 1 argument",
                                    j.to_string()
                                );
                            }
                            args
                        } else {
                            crate::bail_parse_error!(
                                "{} function with no arguments",
                                j.to_string()
                            );
                        };
                        let json_reg = program.alloc_register();
                        translate_expr(program, referenced_tables, &args[0], json_reg, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg: json_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                    JsonFunc::JsonObject | JsonFunc::JsonbObject => {
                        let args = expect_arguments_even!(args, j);

                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                    JsonFunc::JsonValid => translate_function(
                        program,
                        args.as_deref().unwrap_or_default(),
                        referenced_tables,
                        resolver,
                        target_register,
                        func_ctx,
                    ),
                    JsonFunc::JsonPatch | JsonFunc::JsonbPatch => {
                        let args = expect_arguments_exact!(args, 2, j);
                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                    JsonFunc::JsonRemove => {
                        let start_reg =
                            program.alloc_registers(args.as_ref().map(|x| x.len()).unwrap_or(1));
                        if let Some(args) = args {
                            for (i, arg) in args.iter().enumerate() {
                                // register containing result of each argument expression
                                translate_expr(
                                    program,
                                    referenced_tables,
                                    arg,
                                    start_reg + i,
                                    resolver,
                                )?;
                            }
                        }
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                    JsonFunc::JsonQuote => {
                        let args = expect_arguments_exact!(args, 1, j);
                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                    JsonFunc::JsonPretty => {
                        let args = expect_arguments_max!(args, 2, j);

                        translate_function(
                            program,
                            args,
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        )
                    }
                },
                Func::Vector(vector_func) => match vector_func {
                    VectorFunc::Vector | VectorFunc::Vector32 => {
                        let args = expect_arguments_exact!(args, 1, vector_func);
                        let start_reg = program.alloc_register();
                        translate_expr(program, referenced_tables, &args[0], start_reg, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                    VectorFunc::Vector64 => {
                        let args = expect_arguments_exact!(args, 1, vector_func);
                        let start_reg = program.alloc_register();
                        translate_expr(program, referenced_tables, &args[0], start_reg, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                    VectorFunc::VectorExtract => {
                        let args = expect_arguments_exact!(args, 1, vector_func);
                        let start_reg = program.alloc_register();
                        translate_expr(program, referenced_tables, &args[0], start_reg, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                    VectorFunc::VectorDistanceCos => {
                        let args = expect_arguments_exact!(args, 2, vector_func);
                        let regs = program.alloc_registers(2);
                        translate_expr(program, referenced_tables, &args[0], regs, resolver)?;
                        translate_expr(program, referenced_tables, &args[1], regs + 1, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg: regs,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                },
                Func::Scalar(srf) => {
                    match srf {
                        ScalarFunc::Cast => {
                            unreachable!("this is always ast::Expr::Cast")
                        }
                        ScalarFunc::Changes => {
                            if args.is_some() {
                                crate::bail_parse_error!(
                                    "{} function with more than 0 arguments",
                                    srf
                                );
                            }
                            let start_reg = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Char => translate_function(
                            program,
                            args.as_deref().unwrap_or_default(),
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        ),
                        ScalarFunc::Coalesce => {
                            let args = expect_arguments_min!(args, 2, srf);

                            // coalesce function is implemented as a series of not null checks
                            // whenever a not null check succeeds, we jump to the end of the series
                            let label_coalesce_end = program.allocate_label();
                            for (index, arg) in args.iter().enumerate() {
                                let reg = translate_expr_no_constant_opt(
                                    program,
                                    referenced_tables,
                                    arg,
                                    target_register,
                                    resolver,
                                    NoConstantOptReason::RegisterReuse,
                                )?;
                                if index < args.len() - 1 {
                                    program.emit_insn(Insn::NotNull {
                                        reg,
                                        target_pc: label_coalesce_end,
                                    });
                                }
                            }
                            program.preassign_label_to_next_insn(label_coalesce_end);

                            Ok(target_register)
                        }
                        ScalarFunc::LastInsertRowid => {
                            let regs = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: regs,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Concat => {
                            let args = if let Some(args) = args {
                                args
                            } else {
                                crate::bail_parse_error!(
                                    "{} function with no arguments",
                                    srf.to_string()
                                );
                            };
                            let mut start_reg = None;
                            for arg in args.iter() {
                                let reg = program.alloc_register();
                                start_reg = Some(start_reg.unwrap_or(reg));
                                translate_expr(program, referenced_tables, arg, reg, resolver)?;
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: start_reg.unwrap(),
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::ConcatWs => {
                            let args = expect_arguments_min!(args, 2, srf);

                            let temp_register = program.alloc_register();
                            for arg in args.iter() {
                                let reg = program.alloc_register();
                                translate_expr(program, referenced_tables, arg, reg, resolver)?;
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: temp_register + 1,
                                dest: temp_register,
                                func: func_ctx,
                            });

                            program.emit_insn(Insn::Copy {
                                src_reg: temp_register,
                                dst_reg: target_register,
                                amount: 1,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::IfNull => {
                            let args = match args {
                                Some(args) if args.len() == 2 => args,
                                Some(_) => crate::bail_parse_error!(
                                    "{} function requires exactly 2 arguments",
                                    srf.to_string()
                                ),
                                None => crate::bail_parse_error!(
                                    "{} function requires arguments",
                                    srf.to_string()
                                ),
                            };

                            let temp_reg = program.alloc_register();
                            translate_expr_no_constant_opt(
                                program,
                                referenced_tables,
                                &args[0],
                                temp_reg,
                                resolver,
                                NoConstantOptReason::RegisterReuse,
                            )?;
                            let before_copy_label = program.allocate_label();
                            program.emit_insn(Insn::NotNull {
                                reg: temp_reg,
                                target_pc: before_copy_label,
                            });

                            translate_expr_no_constant_opt(
                                program,
                                referenced_tables,
                                &args[1],
                                temp_reg,
                                resolver,
                                NoConstantOptReason::RegisterReuse,
                            )?;
                            program.resolve_label(before_copy_label, program.offset());
                            program.emit_insn(Insn::Copy {
                                src_reg: temp_reg,
                                dst_reg: target_register,
                                amount: 0,
                            });

                            Ok(target_register)
                        }
                        ScalarFunc::Iif => {
                            let args = match args {
                                Some(args) if args.len() == 3 => args,
                                _ => crate::bail_parse_error!(
                                    "{} requires exactly 3 arguments",
                                    srf.to_string()
                                ),
                            };
                            let temp_reg = program.alloc_register();
                            translate_expr_no_constant_opt(
                                program,
                                referenced_tables,
                                &args[0],
                                temp_reg,
                                resolver,
                                NoConstantOptReason::RegisterReuse,
                            )?;
                            let jump_target_when_false = program.allocate_label();
                            program.emit_insn(Insn::IfNot {
                                reg: temp_reg,
                                target_pc: jump_target_when_false,
                                jump_if_null: true,
                            });
                            translate_expr_no_constant_opt(
                                program,
                                referenced_tables,
                                &args[1],
                                target_register,
                                resolver,
                                NoConstantOptReason::RegisterReuse,
                            )?;
                            let jump_target_result = program.allocate_label();
                            program.emit_insn(Insn::Goto {
                                target_pc: jump_target_result,
                            });
                            program.preassign_label_to_next_insn(jump_target_when_false);
                            translate_expr_no_constant_opt(
                                program,
                                referenced_tables,
                                &args[2],
                                target_register,
                                resolver,
                                NoConstantOptReason::RegisterReuse,
                            )?;
                            program.preassign_label_to_next_insn(jump_target_result);
                            Ok(target_register)
                        }
                        ScalarFunc::Glob | ScalarFunc::Like | ScalarFunc::Regexp => {
                            let args = if let Some(args) = args {
                                if args.len() < 2 {
                                    crate::bail_parse_error!(
                                        "{} function with less than 2 arguments",
                                        srf.to_string()
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!(
                                    "{} function with no arguments",
                                    srf.to_string()
                                );
                            };
                            let func_registers = program.alloc_registers(args.len());
                            for (i, arg) in args.iter().enumerate() {
                                let _ = translate_expr(
                                    program,
                                    referenced_tables,
                                    arg,
                                    func_registers + i,
                                    resolver,
                                )?;
                            }
                            program.emit_insn(Insn::Function {
                                // Only constant patterns for LIKE are supported currently, so this
                                // is always 1
                                constant_mask: 1,
                                start_reg: func_registers,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
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
                            let args = expect_arguments_exact!(args, 1, srf);
                            let start_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        #[cfg(feature = "fs")]
                        ScalarFunc::LoadExtension => {
                            let args = expect_arguments_exact!(args, 1, srf);
                            let start_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Random => {
                            if args.is_some() {
                                crate::bail_parse_error!(
                                    "{} function with arguments",
                                    srf.to_string()
                                );
                            }
                            let regs = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: regs,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Date | ScalarFunc::DateTime | ScalarFunc::JulianDay => {
                            let start_reg = program
                                .alloc_registers(args.as_ref().map(|x| x.len()).unwrap_or(1));
                            if let Some(args) = args {
                                for (i, arg) in args.iter().enumerate() {
                                    // register containing result of each argument expression
                                    translate_expr(
                                        program,
                                        referenced_tables,
                                        arg,
                                        start_reg + i,
                                        resolver,
                                    )?;
                                }
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Substr | ScalarFunc::Substring => {
                            let args = if let Some(args) = args {
                                if !(args.len() == 2 || args.len() == 3) {
                                    crate::bail_parse_error!(
                                        "{} function with wrong number of arguments",
                                        srf.to_string()
                                    )
                                }
                                args
                            } else {
                                crate::bail_parse_error!(
                                    "{} function with no arguments",
                                    srf.to_string()
                                );
                            };

                            let str_reg = program.alloc_register();
                            let start_reg = program.alloc_register();
                            let length_reg = program.alloc_register();
                            let str_reg = translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                str_reg,
                                resolver,
                            )?;
                            let _ = translate_expr(
                                program,
                                referenced_tables,
                                &args[1],
                                start_reg,
                                resolver,
                            )?;
                            if args.len() == 3 {
                                translate_expr(
                                    program,
                                    referenced_tables,
                                    &args[2],
                                    length_reg,
                                    resolver,
                                )?;
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: str_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Hex => {
                            let args = if let Some(args) = args {
                                if args.len() != 1 {
                                    crate::bail_parse_error!(
                                        "hex function must have exactly 1 argument",
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!("hex function with no arguments",);
                            };
                            let start_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::UnixEpoch => {
                            let mut start_reg = 0;
                            match args {
                                Some(args) if args.len() > 1 => {
                                    crate::bail_parse_error!("epoch function with > 1 arguments. Modifiers are not yet supported.");
                                }
                                Some(args) if args.len() == 1 => {
                                    let arg_reg = program.alloc_register();
                                    let _ = translate_expr(
                                        program,
                                        referenced_tables,
                                        &args[0],
                                        arg_reg,
                                        resolver,
                                    )?;
                                    start_reg = arg_reg;
                                }
                                _ => {}
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Time => {
                            let start_reg = program
                                .alloc_registers(args.as_ref().map(|x| x.len()).unwrap_or(1));
                            if let Some(args) = args {
                                for (i, arg) in args.iter().enumerate() {
                                    // register containing result of each argument expression
                                    translate_expr(
                                        program,
                                        referenced_tables,
                                        arg,
                                        start_reg + i,
                                        resolver,
                                    )?;
                                }
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::TimeDiff => {
                            let args = expect_arguments_exact!(args, 2, srf);

                            let start_reg = program.alloc_registers(2);
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[1],
                                start_reg + 1,
                                resolver,
                            )?;

                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::TotalChanges => {
                            if args.is_some() {
                                crate::bail_parse_error!(
                                    "{} function with more than 0 arguments",
                                    srf.to_string()
                                );
                            }
                            let start_reg = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Trim
                        | ScalarFunc::LTrim
                        | ScalarFunc::RTrim
                        | ScalarFunc::Round
                        | ScalarFunc::Unhex => {
                            let args = expect_arguments_max!(args, 2, srf);

                            let start_reg = program.alloc_registers(args.len());
                            for (i, arg) in args.iter().enumerate() {
                                translate_expr(
                                    program,
                                    referenced_tables,
                                    arg,
                                    start_reg + i,
                                    resolver,
                                )?;
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Min => {
                            let args = if let Some(args) = args {
                                if args.is_empty() {
                                    crate::bail_parse_error!(
                                        "min function with less than one argument"
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!("min function with no arguments");
                            };
                            let start_reg = program.alloc_registers(args.len());
                            for (i, arg) in args.iter().enumerate() {
                                translate_expr(
                                    program,
                                    referenced_tables,
                                    arg,
                                    start_reg + i,
                                    resolver,
                                )?;
                            }

                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Max => {
                            let args = if let Some(args) = args {
                                if args.is_empty() {
                                    crate::bail_parse_error!(
                                        "max function with less than one argument"
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!("max function with no arguments");
                            };
                            let start_reg = program.alloc_registers(args.len());
                            for (i, arg) in args.iter().enumerate() {
                                translate_expr(
                                    program,
                                    referenced_tables,
                                    arg,
                                    start_reg + i,
                                    resolver,
                                )?;
                            }

                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Nullif | ScalarFunc::Instr => {
                            let args = if let Some(args) = args {
                                if args.len() != 2 {
                                    crate::bail_parse_error!(
                                        "{} function must have two argument",
                                        srf.to_string()
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!(
                                    "{} function with no arguments",
                                    srf.to_string()
                                );
                            };

                            let first_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                first_reg,
                                resolver,
                            )?;
                            let second_reg = program.alloc_register();
                            let _ = translate_expr(
                                program,
                                referenced_tables,
                                &args[1],
                                second_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: first_reg,
                                dest: target_register,
                                func: func_ctx,
                            });

                            Ok(target_register)
                        }
                        ScalarFunc::SqliteVersion => {
                            if args.is_some() {
                                crate::bail_parse_error!("sqlite_version function with arguments");
                            }

                            let output_register = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: output_register,
                                dest: output_register,
                                func: func_ctx,
                            });

                            program.emit_insn(Insn::Copy {
                                src_reg: output_register,
                                dst_reg: target_register,
                                amount: 0,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::SqliteSourceId => {
                            if args.is_some() {
                                crate::bail_parse_error!(
                                    "sqlite_source_id function with arguments"
                                );
                            }

                            let output_register = program.alloc_register();
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: output_register,
                                dest: output_register,
                                func: func_ctx,
                            });

                            program.emit_insn(Insn::Copy {
                                src_reg: output_register,
                                dst_reg: target_register,
                                amount: 0,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Replace => {
                            let args = if let Some(args) = args {
                                if !args.len() == 3 {
                                    crate::bail_parse_error!(
                                        "function {}() requires exactly 3 arguments",
                                        srf.to_string()
                                    )
                                }
                                args
                            } else {
                                crate::bail_parse_error!(
                                    "function {}() requires exactly 3 arguments",
                                    srf.to_string()
                                );
                            };
                            let str_reg = program.alloc_register();
                            let pattern_reg = program.alloc_register();
                            let replacement_reg = program.alloc_register();
                            let _ = translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                str_reg,
                                resolver,
                            )?;
                            let _ = translate_expr(
                                program,
                                referenced_tables,
                                &args[1],
                                pattern_reg,
                                resolver,
                            )?;
                            let _ = translate_expr(
                                program,
                                referenced_tables,
                                &args[2],
                                replacement_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg: str_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::StrfTime => {
                            let start_reg = program
                                .alloc_registers(args.as_ref().map(|x| x.len()).unwrap_or(1));
                            if let Some(args) = args {
                                for (i, arg) in args.iter().enumerate() {
                                    // register containing result of each argument expression
                                    translate_expr(
                                        program,
                                        referenced_tables,
                                        arg,
                                        start_reg + i,
                                        resolver,
                                    )?;
                                }
                            }
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Printf => translate_function(
                            program,
                            args.as_deref().unwrap_or(&[]),
                            referenced_tables,
                            resolver,
                            target_register,
                            func_ctx,
                        ),
                        ScalarFunc::Likely => {
                            let args = if let Some(args) = args {
                                if args.len() != 1 {
                                    crate::bail_parse_error!(
                                        "likely function must have exactly 1 argument",
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!("likely function with no arguments",);
                            };
                            let start_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;
                            program.emit_insn(Insn::Function {
                                constant_mask: 0,
                                start_reg,
                                dest: target_register,
                                func: func_ctx,
                            });
                            Ok(target_register)
                        }
                        ScalarFunc::Likelihood => {
                            let args = if let Some(args) = args {
                                if args.len() != 2 {
                                    crate::bail_parse_error!(
                                        "likelihood() function must have exactly 2 arguments",
                                    );
                                }
                                args
                            } else {
                                crate::bail_parse_error!("likelihood() function with no arguments",);
                            };

                            if let ast::Expr::Literal(ast::Literal::Numeric(ref value)) = args[1] {
                                if let Ok(probability) = value.parse::<f64>() {
                                    if !(0.0..=1.0).contains(&probability) {
                                        crate::bail_parse_error!(
                                            "second argument of likelihood() must be between 0.0 and 1.0",
                                        );
                                    }
                                    if !value.contains('.') {
                                        crate::bail_parse_error!(
                                            "second argument of likelihood() must be a floating point number with decimal point",
                                        );
                                    }
                                } else {
                                    crate::bail_parse_error!(
                                        "second argument of likelihood() must be a floating point constant",
                                    );
                                }
                            } else {
                                crate::bail_parse_error!(
                                    "second argument of likelihood() must be a numeric literal",
                                );
                            }

                            let start_reg = program.alloc_register();
                            translate_expr(
                                program,
                                referenced_tables,
                                &args[0],
                                start_reg,
                                resolver,
                            )?;

                            program.emit_insn(Insn::Copy {
                                src_reg: start_reg,
                                dst_reg: target_register,
                                amount: 0,
                            });

                            Ok(target_register)
                        }
                    }
                }
                Func::Math(math_func) => match math_func.arity() {
                    MathFuncArity::Nullary => {
                        if args.is_some() {
                            crate::bail_parse_error!("{} function with arguments", math_func);
                        }

                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg: 0,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }

                    MathFuncArity::Unary => {
                        let args = expect_arguments_exact!(args, 1, math_func);
                        let start_reg = program.alloc_register();
                        translate_expr(program, referenced_tables, &args[0], start_reg, resolver)?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }

                    MathFuncArity::Binary => {
                        let args = expect_arguments_exact!(args, 2, math_func);
                        let start_reg = program.alloc_registers(2);
                        let _ = translate_expr(
                            program,
                            referenced_tables,
                            &args[0],
                            start_reg,
                            resolver,
                        )?;
                        let _ = translate_expr(
                            program,
                            referenced_tables,
                            &args[1],
                            start_reg + 1,
                            resolver,
                        )?;
                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }

                    MathFuncArity::UnaryOrBinary => {
                        let args = expect_arguments_max!(args, 2, math_func);

                        let regs = program.alloc_registers(args.len());
                        for (i, arg) in args.iter().enumerate() {
                            translate_expr(program, referenced_tables, arg, regs + i, resolver)?;
                        }

                        program.emit_insn(Insn::Function {
                            constant_mask: 0,
                            start_reg: regs,
                            dest: target_register,
                            func: func_ctx,
                        });
                        Ok(target_register)
                    }
                },
                Func::AlterTable(_) => unreachable!(),
            }
        }
        ast::Expr::FunctionCallStar { name, .. } => {
            // A genuine aggregate written as `agg(*)` (e.g. `COUNT(*)`) is collected by
            // `resolve_aggregates` and its result is served by the
            // `resolver.resolve_cached_expr_reg(expr)` cache-hit check at the very top of
            // this function, so it never reaches this arm. The only way to get here is a
            // non-aggregate function name used with `*`, e.g. `SELECT abs(*) FROM t`,
            // which the grammar accepts syntactically but is not semantically valid.
            crate::bail_parse_error!(
                "{}(*) is invalid: '*' may only be used with an aggregate function",
                name.0
            )
        }
        ast::Expr::Id(id) => crate::bail_parse_error!(
            "no such column: {} - should this be a string literal in single-quotes?",
            id.0
        ),
        ast::Expr::Column {
            database: _,
            table: table_ref_id,
            column,
            is_rowid_alias,
        } => {
            let (index, use_covering_index) = {
                if let Some(table_reference) = referenced_tables
                    .unwrap()
                    .find_joined_table_by_internal_id(*table_ref_id)
                {
                    (
                        table_reference.op.index(),
                        table_reference.utilizes_covering_index(),
                    )
                } else {
                    (None, false)
                }
            };

            let table = referenced_tables
                .unwrap()
                .find_table_by_internal_id(*table_ref_id)
                .expect("table reference should be found");

            let Some(table_column) = table.get_column_at(*column) else {
                crate::bail_parse_error!("column index out of bounds");
            };
            // Counter intuitive but a column always needs to have a collation
            program.set_collation(Some((table_column.collation.unwrap_or_default(), false)));

            // If we are reading a column from a table, we find the cursor that corresponds to
            // the table and read the column from the cursor.
            // If we have a covering index, we don't have an open table cursor so we read from the index cursor.
            match &table {
                Table::BTree(_) => {
                    let table_cursor_id = if use_covering_index {
                        None
                    } else {
                        Some(program.resolve_cursor_id(&CursorKey::table(*table_ref_id)))
                    };
                    let index_cursor_id = index.map(|index| {
                        program.resolve_cursor_id(&CursorKey::index(*table_ref_id, index.clone()))
                    });
                    if *is_rowid_alias {
                        if let Some(index_cursor_id) = index_cursor_id {
                            program.emit_insn(Insn::IdxRowId {
                                cursor_id: index_cursor_id,
                                dest: target_register,
                            });
                        } else if let Some(table_cursor_id) = table_cursor_id {
                            program.emit_insn(Insn::RowId {
                                cursor_id: table_cursor_id,
                                dest: target_register,
                            });
                        } else {
                            unreachable!("Either index or table cursor must be opened");
                        }
                    } else {
                        let read_cursor = if use_covering_index {
                            index_cursor_id.expect(
                                "index cursor should be opened when use_covering_index=true",
                            )
                        } else {
                            table_cursor_id.expect(
                                "table cursor should be opened when use_covering_index=false",
                            )
                        };
                        let column = if use_covering_index {
                            let index = index.expect(
                                "index cursor should be opened when use_covering_index=true",
                            );
                            index.column_table_pos_to_index_pos(*column).unwrap_or_else(|| {
                                        panic!("covering index {} does not contain column number {} of table {}", index.name, column, table_ref_id)
                                    })
                        } else {
                            *column
                        };

                        program.emit_column(read_cursor, column, target_register);
                    }
                    let Some(column) = table.get_column_at(*column) else {
                        crate::bail_parse_error!("column index out of bounds");
                    };
                    maybe_apply_affinity(column.ty, target_register, program);
                    Ok(target_register)
                }
                Table::FromClauseSubquery(from_clause_subquery) => {
                    // If we are reading a column from a subquery, we instead copy the column from the
                    // subquery's result registers.
                    program.emit_insn(Insn::Copy {
                        src_reg: from_clause_subquery
                            .result_columns_start_reg
                            .expect("Subquery result_columns_start_reg must be set")
                            + *column,
                        dst_reg: target_register,
                        amount: 0,
                    });
                    Ok(target_register)
                }
                Table::Virtual(_) => {
                    let cursor_id = program.resolve_cursor_id(&CursorKey::table(*table_ref_id));
                    program.emit_insn(Insn::VColumn {
                        cursor_id,
                        column: *column,
                        dest: target_register,
                    });
                    Ok(target_register)
                }
                Table::Pseudo(_) => panic!("Column access on pseudo table"),
                Table::View(_) => crate::bail_parse_error!(
                    "view must be expanded into a subquery before column access"
                ),
            }
        }
        ast::Expr::RowId {
            database: _,
            table: table_ref_id,
        } => {
            let (index, use_covering_index) = {
                if let Some(table_reference) = referenced_tables
                    .unwrap()
                    .find_joined_table_by_internal_id(*table_ref_id)
                {
                    (
                        table_reference.op.index(),
                        table_reference.utilizes_covering_index(),
                    )
                } else {
                    (None, false)
                }
            };

            if use_covering_index {
                let index =
                    index.expect("index cursor should be opened when use_covering_index=true");
                let cursor_id =
                    program.resolve_cursor_id(&CursorKey::index(*table_ref_id, index.clone()));
                program.emit_insn(Insn::IdxRowId {
                    cursor_id,
                    dest: target_register,
                });
            } else {
                let cursor_id = program.resolve_cursor_id(&CursorKey::table(*table_ref_id));
                program.emit_insn(Insn::RowId {
                    cursor_id,
                    dest: target_register,
                });
            }
            Ok(target_register)
        }
        ast::Expr::InList { lhs, not, rhs } => {
            // `x IN ()` / `x NOT IN ()`: the grammar always represents an empty list as
            // `rhs: None` (see `exprlist(A) ::= .  {A = None;}` vs. `nexprlist`, which
            // always has at least one element, in parse.y), matching the always-false /
            // always-true short-circuit in translate_condition_expr's InList arm above.
            // `lhs` is intentionally not evaluated in that case, mirroring that same
            // jump-based implementation. We still guard defensively against a
            // hypothetical `Some(&[])` instead of assuming the grammar invariant holds.
            let Some((first_value_expr, remaining_value_exprs)) =
                rhs.as_ref().and_then(|values| values.split_first())
            else {
                program.emit_insn(Insn::Integer {
                    value: if *not { 1 } else { 0 },
                    dest: target_register,
                });
                return Ok(target_register);
            };

            let lhs_reg = program.alloc_register();
            translate_expr(program, referenced_tables, lhs, lhs_reg, resolver)?;

            // `x IN (a, b, c)`     is `x = a OR  x = b OR  x = c`
            // `x NOT IN (a, b, c)` is `x != a AND x != b AND x != c`
            // (De Morgan's law holds in SQL's three-valued/Kleene logic, matching the
            // per-element comparisons translate_condition_expr's jump-based InList
            // handling performs above.) Each comparison below produces a genuine
            // 0/1/NULL value via the same Eq/Ne + ZeroOrNull pattern emit_binary_insn
            // uses for `=`/`<>`, and the results are folded with Or/And, which already
            // implement SQL's three-valued logic (see Value::exec_or / Value::exec_and).
            translate_in_list_comparison(
                program,
                referenced_tables,
                resolver,
                lhs_reg,
                first_value_expr,
                *not,
                target_register,
            )?;
            for value_expr in remaining_value_exprs {
                let cmp_reg = program.alloc_register();
                translate_in_list_comparison(
                    program,
                    referenced_tables,
                    resolver,
                    lhs_reg,
                    value_expr,
                    *not,
                    cmp_reg,
                )?;
                if *not {
                    program.emit_insn(Insn::And {
                        lhs: target_register,
                        rhs: cmp_reg,
                        dest: target_register,
                    });
                } else {
                    program.emit_insn(Insn::Or {
                        lhs: target_register,
                        rhs: cmp_reg,
                        dest: target_register,
                    });
                }
            }

            Ok(target_register)
        }
        ast::Expr::InSelect { lhs, not, rhs } => {
            let outer = referenced_tables.ok_or_else(|| {
                crate::error::LimboError::ParseError(
                    "IN (SELECT ...) requires an enclosing query with table references".to_string(),
                )
            })?;
            let lhs_reg = program.alloc_register();
            translate_expr(program, referenced_tables, lhs, lhs_reg, resolver)?;
            let mut planned = plan_expr_subquery(program, resolver, outer, rhs)?;
            emit_in_subquery(
                program,
                resolver,
                &mut planned,
                lhs_reg,
                target_register,
                *not,
            )?;
            Ok(target_register)
        }
        ast::Expr::InTable { .. } => todo!(),
        ast::Expr::IsNull(expr) => {
            let reg = program.alloc_register();
            translate_expr(program, referenced_tables, expr, reg, resolver)?;
            program.emit_insn(Insn::Integer {
                value: 1,
                dest: target_register,
            });
            let label = program.allocate_label();
            program.emit_insn(Insn::IsNull {
                reg,
                target_pc: label,
            });
            program.emit_insn(Insn::Integer {
                value: 0,
                dest: target_register,
            });
            program.preassign_label_to_next_insn(label);
            Ok(target_register)
        }
        ast::Expr::Like { not, .. } => {
            let like_reg = if *not {
                program.alloc_register()
            } else {
                target_register
            };
            translate_like_base(program, referenced_tables, expr, like_reg, resolver)?;
            if *not {
                program.emit_insn(Insn::Not {
                    reg: like_reg,
                    dest: target_register,
                });
            }
            Ok(target_register)
        }
        ast::Expr::Literal(lit) => match lit {
            ast::Literal::Numeric(val) => {
                match parse_numeric_literal(val)? {
                    Value::Integer(int_value) => {
                        program.emit_insn(Insn::Integer {
                            value: int_value,
                            dest: target_register,
                        });
                    }
                    Value::Float(real_value) => {
                        program.emit_insn(Insn::Real {
                            value: real_value,
                            dest: target_register,
                        });
                    }
                    _ => unreachable!(),
                }
                Ok(target_register)
            }
            ast::Literal::String(s) => {
                program.emit_insn(Insn::String8 {
                    value: sanitize_string(s),
                    dest: target_register,
                });
                Ok(target_register)
            }
            ast::Literal::Blob(s) => {
                let bytes = s
                    .as_bytes()
                    .chunks_exact(2)
                    .map(|pair| {
                        // We assume that sqlite3-parser has already validated that
                        // the input is valid hex string, thus unwrap is safe.
                        let hex_byte = std::str::from_utf8(pair).unwrap();
                        u8::from_str_radix(hex_byte, 16).unwrap()
                    })
                    .collect();
                program.emit_insn(Insn::Blob {
                    value: bytes,
                    dest: target_register,
                });
                Ok(target_register)
            }
            ast::Literal::Keyword(_) => unreachable!(
                "Literal::Keyword is only constructed by the PRAGMA-only `nmnum` grammar rule \
                 (see parse.y) and is consumed directly by translate/pragma.rs, never via translate_expr"
            ),
            ast::Literal::Null => {
                program.emit_insn(Insn::Null {
                    dest: target_register,
                    dest_end: None,
                });
                Ok(target_register)
            }
            ast::Literal::CurrentDate => {
                program.emit_insn(Insn::String8 {
                    value: datetime::exec_date(&[]).to_string(),
                    dest: target_register,
                });
                Ok(target_register)
            }
            ast::Literal::CurrentTime => {
                program.emit_insn(Insn::String8 {
                    value: datetime::exec_time(&[]).to_string(),
                    dest: target_register,
                });
                Ok(target_register)
            }
            ast::Literal::CurrentTimestamp => {
                program.emit_insn(Insn::String8 {
                    value: datetime::exec_datetime_full(&[]).to_string(),
                    dest: target_register,
                });
                Ok(target_register)
            }
        },
        ast::Expr::Name(_) => unreachable!(
            "Expr::Name is only constructed by the PRAGMA-only `nmnum` grammar rule (see \
             parse.y) and is consumed directly by translate/pragma.rs, never via translate_expr"
        ),
        ast::Expr::NotNull(expr) => {
            let reg = program.alloc_register();
            translate_expr(program, referenced_tables, expr, reg, resolver)?;
            program.emit_insn(Insn::Integer {
                value: 1,
                dest: target_register,
            });
            let label = program.allocate_label();
            program.emit_insn(Insn::NotNull {
                reg,
                target_pc: label,
            });
            program.emit_insn(Insn::Integer {
                value: 0,
                dest: target_register,
            });
            program.preassign_label_to_next_insn(label);
            Ok(target_register)
        }
        ast::Expr::Parenthesized(exprs) => {
            if exprs.is_empty() {
                crate::bail_parse_error!("parenthesized expression with no arguments");
            }
            if exprs.len() == 1 {
                translate_expr(
                    program,
                    referenced_tables,
                    &exprs[0],
                    target_register,
                    resolver,
                )?;
            } else {
                // Parenthesized expressions with multiple arguments are reserved for special cases
                // like `(a, b) IN ((1, 2), (3, 4))` (row-value comparisons), which are not yet
                // supported. Mirrors the identical guard in translate_condition_expr's sibling
                // Parenthesized arm above, which bails cleanly for this same shape instead of
                // panicking; full row-value desugaring is a separate, deferred feature.
                crate::bail_parse_error!("parenthesized expression should have exactly one expression");
            }
            Ok(target_register)
        }
        ast::Expr::Qualified(_, _) => {
            unreachable!("Qualified should be resolved to a Column before translation")
        }
        ast::Expr::Raise(_, _) => todo!(),
        ast::Expr::Subquery(select) => {
            let outer = referenced_tables.ok_or_else(|| {
                crate::error::LimboError::ParseError(
                    "scalar subquery requires an enclosing query with table references".to_string(),
                )
            })?;
            let mut planned = plan_expr_subquery(program, resolver, outer, select)?;
            emit_scalar_or_exists_subquery(
                program,
                resolver,
                &mut planned,
                ExprSubqueryKind::Scalar,
                target_register,
            )?;
            Ok(target_register)
        }
        ast::Expr::Unary(op, expr) => match (op, expr.as_ref()) {
            (UnaryOperator::Positive, expr) => {
                translate_expr(program, referenced_tables, expr, target_register, resolver)
            }
            (UnaryOperator::Negative, ast::Expr::Literal(ast::Literal::Numeric(numeric_value))) => {
                let numeric_value = "-".to_owned() + numeric_value;
                match parse_numeric_literal(&numeric_value)? {
                    Value::Integer(int_value) => {
                        program.emit_insn(Insn::Integer {
                            value: int_value,
                            dest: target_register,
                        });
                    }
                    Value::Float(real_value) => {
                        program.emit_insn(Insn::Real {
                            value: real_value,
                            dest: target_register,
                        });
                    }
                    _ => unreachable!(),
                }
                Ok(target_register)
            }
            (UnaryOperator::Negative, _) => {
                let value = 0;

                let reg = program.alloc_register();
                translate_expr(program, referenced_tables, expr, reg, resolver)?;
                let zero_reg = program.alloc_register();
                program.emit_insn(Insn::Integer {
                    value,
                    dest: zero_reg,
                });
                program.mark_last_insn_constant();
                program.emit_insn(Insn::Subtract {
                    lhs: zero_reg,
                    rhs: reg,
                    dest: target_register,
                });
                Ok(target_register)
            }
            (UnaryOperator::BitwiseNot, ast::Expr::Literal(ast::Literal::Numeric(num_val))) => {
                match parse_numeric_literal(num_val)? {
                    Value::Integer(int_value) => {
                        program.emit_insn(Insn::Integer {
                            value: !int_value,
                            dest: target_register,
                        });
                    }
                    Value::Float(real_value) => {
                        program.emit_insn(Insn::Integer {
                            value: !(real_value as i64),
                            dest: target_register,
                        });
                    }
                    _ => unreachable!(),
                }
                Ok(target_register)
            }
            (UnaryOperator::BitwiseNot, ast::Expr::Literal(ast::Literal::Null)) => {
                program.emit_insn(Insn::Null {
                    dest: target_register,
                    dest_end: None,
                });
                Ok(target_register)
            }
            (UnaryOperator::BitwiseNot, _) => {
                let reg = program.alloc_register();
                translate_expr(program, referenced_tables, expr, reg, resolver)?;
                program.emit_insn(Insn::BitNot {
                    reg,
                    dest: target_register,
                });
                Ok(target_register)
            }
            (UnaryOperator::Not, _) => {
                let reg = program.alloc_register();
                translate_expr(program, referenced_tables, expr, reg, resolver)?;
                program.emit_insn(Insn::Not {
                    reg,
                    dest: target_register,
                });
                Ok(target_register)
            }
        },
        ast::Expr::Variable(name) => {
            let index = program.parameters.push(name)?;
            program.emit_insn(Insn::Variable {
                index,
                dest: target_register,
            });
            Ok(target_register)
        }
    }?;

    if let Some(span) = constant_span {
        program.constant_span_end(span);
    }

    Ok(target_register)
}
