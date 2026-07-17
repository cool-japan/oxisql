//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

#[cfg(feature = "json")]
use crate::function::JsonFunc;
use crate::function::{Func, FuncCtx, ScalarFunc};
use crate::schema::{Affinity, Type};
use crate::translate::emitter::Resolver;
use crate::translate::plan::TableReferences;
use crate::vdbe::{
    builder::ProgramBuilder,
    insn::{CmpInsFlags, Insn},
    BranchOffset,
};
use crate::Result;
use limbo_sqlite3_parser::ast::{self, Expr};

use super::value::translate_expr;
use super::walk::{comparison_affinity, walk_expr};

/// Evaluates a single `lhs_reg <op> rhs_value_expr` comparison for [ast::Expr::InList] when it
/// is used as a value rather than a jump condition (see the `InList` arm of [translate_expr]),
/// writing a genuine three-valued (0/1/NULL) result into `dest`. Uses `Eq` for `IN` and `Ne` for
/// `NOT IN` -- the same per-element comparison translate_condition_expr's jump-based `InList`
/// handling performs -- wrapped with [wrap_eval_jump_expr_zero_or_null] the same way
/// [emit_binary_insn] does for `=`/`<>`.
pub(super) fn translate_in_list_comparison(
    program: &mut ProgramBuilder,
    referenced_tables: Option<&TableReferences>,
    resolver: &Resolver,
    lhs_reg: usize,
    rhs_value_expr: &ast::Expr,
    not: bool,
    dest: usize,
) -> Result<()> {
    let rhs_reg = program.alloc_register();
    translate_expr(
        program,
        referenced_tables,
        rhs_value_expr,
        rhs_reg,
        resolver,
    )?;
    let if_true_label = program.allocate_label();
    let cmp_insn = if not {
        Insn::Ne {
            lhs: lhs_reg,
            rhs: rhs_reg,
            target_pc: if_true_label,
            flags: CmpInsFlags::default(),
            collation: program.curr_collation(),
        }
    } else {
        Insn::Eq {
            lhs: lhs_reg,
            rhs: rhs_reg,
            target_pc: if_true_label,
            flags: CmpInsFlags::default(),
            collation: program.curr_collation(),
        }
    };
    wrap_eval_jump_expr_zero_or_null(program, cmp_insn, dest, if_true_label, lhs_reg, rhs_reg);
    Ok(())
}
pub(super) fn emit_binary_insn(
    program: &mut ProgramBuilder,
    op: &ast::Operator,
    lhs: usize,
    rhs: usize,
    target_register: usize,
    lhs_expr: &Expr,
    rhs_expr: &Expr,
    referenced_tables: Option<&TableReferences>,
) -> Result<()> {
    let mut affinity = Affinity::Blob;
    if op.is_comparison() {
        affinity = comparison_affinity(lhs_expr, rhs_expr, referenced_tables);
    }
    match op {
        ast::Operator::NotEquals => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Ne {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::Equals => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Eq {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::Less => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Lt {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::LessEquals => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Le {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::Greater => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Gt {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::GreaterEquals => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr_zero_or_null(
                program,
                Insn::Ge {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
                lhs,
                rhs,
            );
        }
        ast::Operator::Add => {
            program.emit_insn(Insn::Add {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Subtract => {
            program.emit_insn(Insn::Subtract {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Multiply => {
            program.emit_insn(Insn::Multiply {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Divide => {
            program.emit_insn(Insn::Divide {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Modulus => {
            program.emit_insn(Insn::Remainder {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::And => {
            program.emit_insn(Insn::And {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Or => {
            program.emit_insn(Insn::Or {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::BitwiseAnd => {
            program.emit_insn(Insn::BitAnd {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::BitwiseOr => {
            program.emit_insn(Insn::BitOr {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::RightShift => {
            program.emit_insn(Insn::ShiftRight {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::LeftShift => {
            program.emit_insn(Insn::ShiftLeft {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        ast::Operator::Is => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr(
                program,
                Insn::Eq {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().null_eq().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
            );
        }
        ast::Operator::IsNot => {
            let if_true_label = program.allocate_label();
            wrap_eval_jump_expr(
                program,
                Insn::Ne {
                    lhs,
                    rhs,
                    target_pc: if_true_label,
                    flags: CmpInsFlags::default().null_eq().with_affinity(affinity),
                    collation: program.curr_collation(),
                },
                target_register,
                if_true_label,
            );
        }
        #[cfg(feature = "json")]
        op @ (ast::Operator::ArrowRight | ast::Operator::ArrowRightShift) => {
            let json_func = match op {
                ast::Operator::ArrowRight => JsonFunc::JsonArrowExtract,
                ast::Operator::ArrowRightShift => JsonFunc::JsonArrowShiftExtract,
                _ => unreachable!(),
            };
            program.emit_insn(Insn::Function {
                constant_mask: 0,
                start_reg: lhs,
                dest: target_register,
                func: FuncCtx {
                    func: Func::Json(json_func),
                    arg_count: 2,
                },
            })
        }
        ast::Operator::Concat => {
            program.emit_insn(Insn::Concat {
                lhs,
                rhs,
                dest: target_register,
            });
        }
        // The parser only ever constructs `~` as `UnaryOperator::BitwiseNot` (see
        // `expr(A) ::= BITNOT(B) expr(X)` in parse.y); no grammar rule builds a `Binary`
        // node with `Operator::BitwiseNot` (the binary bitwise rule is
        // `expr(X) BITAND|BITOR|LSHIFT|RSHIFT expr(Y)`, which excludes BITNOT), so this
        // is truly unreachable.
        ast::Operator::BitwiseNot => {
            unreachable!("~ is only ever constructed as a UnaryOperator, never a Binary Operator")
        }
        // The `->`/`->>` grammar rule (`expr(B) PTR(C) expr(D)`) is unconditional in the
        // parser crate, so `Operator::ArrowRight`/`ArrowRightShift` can still be parsed
        // even when this crate's own "json" feature is disabled (only the arm above that
        // implements them is feature-gated) -- give a clean error instead of falling
        // through to a panic in that configuration.
        #[cfg(not(feature = "json"))]
        ast::Operator::ArrowRight | ast::Operator::ArrowRightShift => {
            crate::bail_parse_error!(
                "the -> and ->> operators require the \"json\" feature to be enabled"
            )
        }
    }
    Ok(())
}
/// The base logic for translating LIKE and GLOB expressions.
/// The logic for handling "NOT LIKE" is different depending on whether the expression
/// is a conditional jump or not. This is why the caller handles the "NOT LIKE" behavior;
/// see [translate_condition_expr] and [translate_expr] for implementations.
pub(super) fn translate_like_base(
    program: &mut ProgramBuilder,
    referenced_tables: Option<&TableReferences>,
    expr: &ast::Expr,
    target_register: usize,
    resolver: &Resolver,
) -> Result<usize> {
    let ast::Expr::Like {
        lhs,
        op,
        rhs,
        escape,
        ..
    } = expr
    else {
        crate::bail_parse_error!("expected Like expression");
    };
    match op {
        ast::LikeOperator::Like | ast::LikeOperator::Glob => {
            let arg_count = if matches!(escape, Some(_)) { 3 } else { 2 };
            let start_reg = program.alloc_registers(arg_count);
            let mut constant_mask = 0;
            translate_expr(program, referenced_tables, lhs, start_reg + 1, resolver)?;
            let _ = translate_expr(program, referenced_tables, rhs, start_reg, resolver)?;
            if arg_count == 3 {
                if let Some(escape) = escape {
                    translate_expr(program, referenced_tables, escape, start_reg + 2, resolver)?;
                }
            }
            if matches!(rhs.as_ref(), ast::Expr::Literal(_)) {
                program.mark_last_insn_constant();
                constant_mask = 1;
            }
            let func = match op {
                ast::LikeOperator::Like => ScalarFunc::Like,
                ast::LikeOperator::Glob => ScalarFunc::Glob,
                _ => unreachable!(),
            };
            program.emit_insn(Insn::Function {
                constant_mask,
                start_reg,
                dest: target_register,
                func: FuncCtx {
                    func: Func::Scalar(func),
                    arg_count,
                },
            });
        }
        ast::LikeOperator::Match => {
            // Real SQLite's MATCH is a placeholder operator with no default implementation;
            // it only becomes usable when a virtual table module overrides `xFindFunction` to
            // supply one (e.g. FTS3/4/5). No bundled virtual table in this engine does that
            // today, so this is the correct (real-SQLite-matching) behavior, not a stopgap.
            crate::bail_parse_error!("unable to use function MATCH in the requested context")
        }
        ast::LikeOperator::Regexp => {
            // `X REGEXP Y` is defined by SQLite as `regexp(Y, X)`: the pattern (rhs)
            // is the first argument, the subject (lhs) is the second. REGEXP has no
            // ESCAPE clause in the grammar, so `escape` is always None here.
            let start_reg = program.alloc_registers(2);
            translate_expr(program, referenced_tables, lhs, start_reg + 1, resolver)?;
            let _ = translate_expr(program, referenced_tables, rhs, start_reg, resolver)?;
            let mut constant_mask = 0;
            if matches!(rhs.as_ref(), ast::Expr::Literal(_)) {
                program.mark_last_insn_constant();
                constant_mask = 1;
            }
            program.emit_insn(Insn::Function {
                constant_mask,
                start_reg,
                dest: target_register,
                func: FuncCtx {
                    func: Func::Scalar(ScalarFunc::Regexp),
                    arg_count: 2,
                },
            });
        }
    }
    Ok(target_register)
}
/// Emits a whole insn for a function call.
/// Assumes the number of parameters is valid for the given function.
/// Returns the target register for the function.
pub(super) fn translate_function(
    program: &mut ProgramBuilder,
    args: &[ast::Expr],
    referenced_tables: Option<&TableReferences>,
    resolver: &Resolver,
    target_register: usize,
    func_ctx: FuncCtx,
) -> Result<usize> {
    let start_reg = program.alloc_registers(args.len());
    let mut current_reg = start_reg;
    for arg in args.iter() {
        translate_expr(program, referenced_tables, arg, current_reg, resolver)?;
        current_reg += 1;
    }
    program.emit_insn(Insn::Function {
        constant_mask: 0,
        start_reg,
        dest: target_register,
        func: func_ctx,
    });
    Ok(target_register)
}
fn wrap_eval_jump_expr(
    program: &mut ProgramBuilder,
    insn: Insn,
    target_register: usize,
    if_true_label: BranchOffset,
) {
    program.emit_insn(Insn::Integer {
        value: 1, // emit True by default
        dest: target_register,
    });
    program.emit_insn(insn);
    program.emit_insn(Insn::Integer {
        value: 0, // emit False if we reach this point (no jump)
        dest: target_register,
    });
    program.preassign_label_to_next_insn(if_true_label);
}

fn wrap_eval_jump_expr_zero_or_null(
    program: &mut ProgramBuilder,
    insn: Insn,
    target_register: usize,
    if_true_label: BranchOffset,
    e1_reg: usize,
    e2_reg: usize,
) {
    program.emit_insn(Insn::Integer {
        value: 1, // emit True by default
        dest: target_register,
    });
    program.emit_insn(insn);
    program.emit_insn(Insn::ZeroOrNull {
        rg1: e1_reg,
        rg2: e2_reg,
        dest: target_register,
    });
    program.preassign_label_to_next_insn(if_true_label);
}

pub fn maybe_apply_affinity(col_type: Type, target_register: usize, program: &mut ProgramBuilder) {
    if col_type == Type::Real {
        program.emit_insn(Insn::RealAffinity {
            register: target_register,
        })
    }
}

/// Sanitaizes a string literal by removing single quote at front and back
/// and escaping double single quotes
pub fn sanitize_string(input: &str) -> String {
    input[1..input.len() - 1].replace("''", "'").to_string()
}

/// Returns the components of a binary expression
/// e.g. t.x = 5 -> Some((t.x, =, 5))
pub fn as_binary_components(
    expr: &ast::Expr,
) -> Result<Option<(&ast::Expr, ast::Operator, &ast::Expr)>> {
    match unwrap_parens(expr)? {
        ast::Expr::Binary(lhs, operator, rhs)
            if matches!(
                operator,
                ast::Operator::Equals
                    | ast::Operator::Greater
                    | ast::Operator::Less
                    | ast::Operator::GreaterEquals
                    | ast::Operator::LessEquals
            ) =>
        {
            Ok(Some((lhs.as_ref(), *operator, rhs.as_ref())))
        }
        _ => Ok(None),
    }
}

/// Recursively unwrap parentheses from an expression
/// e.g. (((t.x > 5))) -> t.x > 5
fn unwrap_parens(expr: &ast::Expr) -> Result<&ast::Expr> {
    match expr {
        ast::Expr::Column { .. } => Ok(expr),
        ast::Expr::Parenthesized(exprs) => match exprs.len() {
            1 => unwrap_parens(exprs.first().unwrap()),
            _ => crate::bail_parse_error!("expected single expression in parentheses"),
        },
        _ => Ok(expr),
    }
}

/// Recursively unwrap parentheses from an owned Expr.
/// Returns how many pairs of parentheses were removed.
pub fn unwrap_parens_owned(expr: ast::Expr) -> Result<(ast::Expr, usize)> {
    let mut paren_count = 0;
    match expr {
        ast::Expr::Parenthesized(mut exprs) => match exprs.len() {
            1 => {
                paren_count += 1;
                let (expr, count) = unwrap_parens_owned(exprs.pop().unwrap())?;
                paren_count += count;
                Ok((expr, paren_count))
            }
            _ => crate::bail_parse_error!("expected single expression in parentheses"),
        },
        _ => Ok((expr, paren_count)),
    }
}

/// Returns true if `expr` contains a subquery in expression position anywhere in its tree
/// (a scalar `(SELECT ...)`, `EXISTS (...)`, or `... IN (SELECT ...)`).
///
/// This is used by the emitter to decide WHERE-term evaluation placement: a term containing a
/// subquery must not be hoisted before the main loop, because a correlated subquery requires the
/// outer row's cursors to be positioned first. (`walk_expr` visits subquery nodes but does not
/// descend into their inner `Select`, which is exactly what we want here: detect presence without
/// recursing into the as-yet-unplanned inner query.)
pub fn expr_contains_subquery(expr: &ast::Expr) -> bool {
    let mut found = false;
    let _ = walk_expr(expr, &mut |e: &ast::Expr| -> Result<()> {
        if matches!(
            e,
            ast::Expr::Subquery(_) | ast::Expr::Exists(_) | ast::Expr::InSelect { .. }
        ) {
            found = true;
        }
        Ok(())
    });
    found
}
