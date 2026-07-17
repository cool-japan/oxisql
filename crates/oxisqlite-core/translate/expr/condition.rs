//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::translate::emitter::Resolver;
use crate::translate::plan::TableReferences;
use crate::vdbe::{
    builder::ProgramBuilder,
    insn::{CmpInsFlags, Insn},
};
use crate::Result;
use limbo_sqlite3_parser::ast;
use tracing::{instrument, Level};

use super::binary_emit::translate_like_base;
use super::types::{ConditionMetadata, NoConstantOptReason};
use super::value::translate_expr;

#[instrument(skip_all, level = Level::TRACE)]
fn emit_cond_jump(program: &mut ProgramBuilder, cond_meta: ConditionMetadata, reg: usize) {
    if cond_meta.jump_if_condition_is_true {
        program.emit_insn(Insn::If {
            reg,
            target_pc: cond_meta.jump_target_when_true,
            jump_if_null: false,
        });
    } else {
        program.emit_insn(Insn::IfNot {
            reg,
            target_pc: cond_meta.jump_target_when_false,
            jump_if_null: true,
        });
    }
}

#[instrument(skip(program, referenced_tables, expr, resolver), level = Level::TRACE)]
pub fn translate_condition_expr(
    program: &mut ProgramBuilder,
    referenced_tables: &TableReferences,
    expr: &ast::Expr,
    condition_metadata: ConditionMetadata,
    resolver: &Resolver,
) -> Result<()> {
    match expr {
        ast::Expr::Between { .. } => {
            unreachable!("expression should have been rewritten in optmizer")
        }
        ast::Expr::Binary(lhs, ast::Operator::And, rhs) => {
            // In a binary AND, never jump to the parent 'jump_target_when_true' label on the first condition, because
            // the second condition MUST also be true. Instead we instruct the child expression to jump to a local
            // true label.
            let jump_target_when_true = program.allocate_label();
            translate_condition_expr(
                program,
                referenced_tables,
                lhs,
                ConditionMetadata {
                    jump_if_condition_is_true: false,
                    jump_target_when_true,
                    ..condition_metadata
                },
                resolver,
            )?;
            program.preassign_label_to_next_insn(jump_target_when_true);
            translate_condition_expr(
                program,
                referenced_tables,
                rhs,
                condition_metadata,
                resolver,
            )?;
        }
        ast::Expr::Binary(lhs, ast::Operator::Or, rhs) => {
            // In a binary OR, never jump to the parent 'jump_target_when_false' label on the first condition, because
            // the second condition CAN also be true. Instead we instruct the child expression to jump to a local
            // false label.
            let jump_target_when_false = program.allocate_label();
            translate_condition_expr(
                program,
                referenced_tables,
                lhs,
                ConditionMetadata {
                    jump_if_condition_is_true: true,
                    jump_target_when_false,
                    ..condition_metadata
                },
                resolver,
            )?;
            program.preassign_label_to_next_insn(jump_target_when_false);
            translate_condition_expr(
                program,
                referenced_tables,
                rhs,
                condition_metadata,
                resolver,
            )?;
        }
        ast::Expr::Binary(_, _, _) => {
            let result_reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, result_reg, resolver)?;
            emit_cond_jump(program, condition_metadata, result_reg);
        }
        ast::Expr::Literal(_)
        | ast::Expr::Cast { .. }
        | ast::Expr::FunctionCall { .. }
        | ast::Expr::Column { .. }
        | ast::Expr::RowId { .. }
        | ast::Expr::Case { .. } => {
            let reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, reg, resolver)?;
            emit_cond_jump(program, condition_metadata, reg);
        }
        ast::Expr::InList { lhs, not, rhs } => {
            // lhs is e.g. a column reference
            // rhs is an Option<Vec<Expr>>
            // If rhs is None, it means the IN expression is always false, i.e. tbl.id IN ().
            // If rhs is Some, it means the IN expression has a list of values to compare against, e.g. tbl.id IN (1, 2, 3).
            //
            // The IN expression is equivalent to a series of OR expressions.
            // For example, `a IN (1, 2, 3)` is equivalent to `a = 1 OR a = 2 OR a = 3`.
            // The NOT IN expression is equivalent to a series of AND expressions.
            // For example, `a NOT IN (1, 2, 3)` is equivalent to `a != 1 AND a != 2 AND a != 3`.
            //
            // SQLite typically optimizes IN expressions to use a binary search on an ephemeral index if there are many values.
            // For now we don't have the plumbing to do that, so we'll just emit a series of comparisons,
            // which is what SQLite also does for small lists of values.
            // TODO: Let's refactor this later to use a more efficient implementation conditionally based on the number of values.

            if rhs.is_none() {
                // If rhs is None, IN expressions are always false and NOT IN expressions are always true.
                if *not {
                    // On a trivially true NOT IN () expression we can only jump to the 'jump_target_when_true' label if 'jump_if_condition_is_true'; otherwise me must fall through.
                    // This is because in a more complex condition we might need to evaluate the rest of the condition.
                    // Note that we are already breaking up our WHERE clauses into a series of terms at "AND" boundaries, so right now we won't be running into cases where jumping on true would be incorrect,
                    // but once we have e.g. parenthesization and more complex conditions, not having this 'if' here would introduce a bug.
                    if condition_metadata.jump_if_condition_is_true {
                        program.emit_insn(Insn::Goto {
                            target_pc: condition_metadata.jump_target_when_true,
                        });
                    }
                } else {
                    program.emit_insn(Insn::Goto {
                        target_pc: condition_metadata.jump_target_when_false,
                    });
                }
                return Ok(());
            }

            // The left hand side only needs to be evaluated once we have a list of values to compare against.
            let lhs_reg = program.alloc_register();
            let _ = translate_expr(program, Some(referenced_tables), lhs, lhs_reg, resolver)?;

            let rhs = rhs.as_ref().unwrap();

            // The difference between a local jump and an "upper level" jump is that for example in this case:
            // WHERE foo IN (1,2,3) OR bar = 5,
            // we can immediately jump to the 'jump_target_when_true' label of the ENTIRE CONDITION if foo = 1, foo = 2, or foo = 3 without evaluating the bar = 5 condition.
            // This is why in Binary-OR expressions we set jump_if_condition_is_true to true for the first condition.
            // However, in this example:
            // WHERE foo IN (1,2,3) AND bar = 5,
            // we can't jump to the 'jump_target_when_true' label of the entire condition foo = 1, foo = 2, or foo = 3, because we still need to evaluate the bar = 5 condition later.
            // This is why in that case we just jump over the rest of the IN conditions in this "local" branch which evaluates the IN condition.
            let jump_target_when_true = if condition_metadata.jump_if_condition_is_true {
                condition_metadata.jump_target_when_true
            } else {
                program.allocate_label()
            };

            if !*not {
                // If it's an IN expression, we need to jump to the 'jump_target_when_true' label if any of the conditions are true.
                for (i, expr) in rhs.iter().enumerate() {
                    let rhs_reg = program.alloc_register();
                    let last_condition = i == rhs.len() - 1;
                    let _ =
                        translate_expr(program, Some(referenced_tables), expr, rhs_reg, resolver)?;
                    // If this is not the last condition, we need to jump to the 'jump_target_when_true' label if the condition is true.
                    if !last_condition {
                        program.emit_insn(Insn::Eq {
                            lhs: lhs_reg,
                            rhs: rhs_reg,
                            target_pc: jump_target_when_true,
                            flags: CmpInsFlags::default(),
                            collation: program.curr_collation(),
                        });
                    } else {
                        // If this is the last condition, we need to jump to the 'jump_target_when_false' label if there is no match.
                        program.emit_insn(Insn::Ne {
                            lhs: lhs_reg,
                            rhs: rhs_reg,
                            target_pc: condition_metadata.jump_target_when_false,
                            flags: CmpInsFlags::default().jump_if_null(),
                            collation: program.curr_collation(),
                        });
                    }
                }
                // If we got here, then the last condition was a match, so we jump to the 'jump_target_when_true' label if 'jump_if_condition_is_true'.
                // If not, we can just fall through without emitting an unnecessary instruction.
                if condition_metadata.jump_if_condition_is_true {
                    program.emit_insn(Insn::Goto {
                        target_pc: condition_metadata.jump_target_when_true,
                    });
                }
            } else {
                // If it's a NOT IN expression, we need to jump to the 'jump_target_when_false' label if any of the conditions are true.
                for expr in rhs.iter() {
                    let rhs_reg = program.alloc_register();
                    let _ =
                        translate_expr(program, Some(referenced_tables), expr, rhs_reg, resolver)?;
                    program.emit_insn(Insn::Eq {
                        lhs: lhs_reg,
                        rhs: rhs_reg,
                        target_pc: condition_metadata.jump_target_when_false,
                        flags: CmpInsFlags::default().jump_if_null(),
                        collation: program.curr_collation(),
                    });
                }
                // If we got here, then none of the conditions were a match, so we jump to the 'jump_target_when_true' label if 'jump_if_condition_is_true'.
                // If not, we can just fall through without emitting an unnecessary instruction.
                if condition_metadata.jump_if_condition_is_true {
                    program.emit_insn(Insn::Goto {
                        target_pc: condition_metadata.jump_target_when_true,
                    });
                }
            }

            if !condition_metadata.jump_if_condition_is_true {
                program.preassign_label_to_next_insn(jump_target_when_true);
            }
        }
        ast::Expr::Like { not, .. } => {
            let cur_reg = program.alloc_register();
            translate_like_base(program, Some(referenced_tables), expr, cur_reg, resolver)?;
            if !*not {
                emit_cond_jump(program, condition_metadata, cur_reg);
            } else if condition_metadata.jump_if_condition_is_true {
                program.emit_insn(Insn::IfNot {
                    reg: cur_reg,
                    target_pc: condition_metadata.jump_target_when_true,
                    jump_if_null: false,
                });
            } else {
                program.emit_insn(Insn::If {
                    reg: cur_reg,
                    target_pc: condition_metadata.jump_target_when_false,
                    jump_if_null: true,
                });
            }
        }
        ast::Expr::Parenthesized(exprs) => {
            if exprs.len() == 1 {
                let _ = translate_condition_expr(
                    program,
                    referenced_tables,
                    &exprs[0],
                    condition_metadata,
                    resolver,
                );
            } else {
                crate::bail_parse_error!(
                    "parenthesized conditional should have exactly one expression"
                );
            }
        }
        ast::Expr::NotNull(expr) => {
            let cur_reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, cur_reg, resolver)?;
            program.emit_insn(Insn::IsNull {
                reg: cur_reg,
                target_pc: condition_metadata.jump_target_when_false,
            });
        }
        ast::Expr::IsNull(expr) => {
            let cur_reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, cur_reg, resolver)?;
            program.emit_insn(Insn::NotNull {
                reg: cur_reg,
                target_pc: condition_metadata.jump_target_when_false,
            });
        }
        ast::Expr::Unary(_, _) => {
            // This is an inefficient implementation for op::NOT, because translate_expr() will emit an Insn::Not,
            // and then we immediately emit an Insn::If/Insn::IfNot for the conditional jump. In reality we would not
            // like to emit the negation instruction Insn::Not at all, since we could just emit the "opposite" jump instruction
            // directly. However, using translate_expr() directly simplifies our conditional jump code for unary expressions,
            // and we'd rather be correct than maximally efficient, for now.
            let expr_reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, expr_reg, resolver)?;
            emit_cond_jump(program, condition_metadata, expr_reg);
        }
        // Subquery-valued conditions (EXISTS / IN (SELECT ...) / scalar subquery used as a
        // boolean). We evaluate the subquery into a register via translate_expr() (which contains
        // the full correlated/uncorrelated emission logic) and then perform the conditional jump.
        ast::Expr::Exists(_) | ast::Expr::InSelect { .. } | ast::Expr::Subquery(_) => {
            let reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), expr, reg, resolver)?;
            emit_cond_jump(program, condition_metadata, reg);
        }
        other => {
            // Every `ast::Expr` variant not given bespoke short-circuiting treatment above
            // (e.g. `Collate`, `Id`, `Variable`, and the pre-resolved-away
            // `Qualified`/`DoublyQualified`/`Name`) is still a perfectly good *value*-producing
            // expression: evaluate it via translate_expr and branch on its truthiness, exactly
            // like the `Literal | Cast | FunctionCall | Column | RowId | Case` arm above does.
            // Variants that genuinely aren't implemented yet (`InTable`, `Raise`) simply surface
            // translate_expr's own todo!()/unreachable!() for that variant when reached through
            // here, instead of duplicating a separate one in this function.
            let reg = program.alloc_register();
            translate_expr(program, Some(referenced_tables), other, reg, resolver)?;
            emit_cond_jump(program, condition_metadata, reg);
        }
    }
    Ok(())
}

/// Translate an expression into bytecode via [translate_expr()], and forbid any constant values from being hoisted
/// into the beginning of the program. This is a good idea in most cases where
/// a register will end up being reused e.g. in a coroutine.
pub fn translate_expr_no_constant_opt(
    program: &mut ProgramBuilder,
    referenced_tables: Option<&TableReferences>,
    expr: &ast::Expr,
    target_register: usize,
    resolver: &Resolver,
    deopt_reason: NoConstantOptReason,
) -> Result<usize> {
    tracing::debug!(
        "translate_expr_no_constant_opt: expr={:?}, deopt_reason={:?}",
        expr,
        deopt_reason
    );
    let next_span_idx = program.constant_spans_next_idx();
    let translated = translate_expr(program, referenced_tables, expr, target_register, resolver)?;
    program.constant_spans_invalidate_after(next_span_idx);
    Ok(translated)
}
