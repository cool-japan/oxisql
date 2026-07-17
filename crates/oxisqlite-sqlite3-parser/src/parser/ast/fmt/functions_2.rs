//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::ast::*;
use crate::dialect::TokenType::*;
use std::fmt::{self, Display, Formatter};

use super::functions::{ToTokens, TokenStream};
use super::functions_4::{comma, double_quote};

impl ToTokens for Expr {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Between {
                lhs,
                not,
                start,
                end,
            } => {
                lhs.to_tokens(s)?;
                if *not {
                    s.append(TK_NOT, None)?;
                }
                s.append(TK_BETWEEN, None)?;
                start.to_tokens(s)?;
                s.append(TK_AND, None)?;
                end.to_tokens(s)
            }
            Self::Binary(lhs, op, rhs) => {
                lhs.to_tokens(s)?;
                op.to_tokens(s)?;
                rhs.to_tokens(s)
            }
            Self::Case {
                base,
                when_then_pairs,
                else_expr,
            } => {
                s.append(TK_CASE, None)?;
                if let Some(ref base) = base {
                    base.to_tokens(s)?;
                }
                for (when, then) in when_then_pairs {
                    s.append(TK_WHEN, None)?;
                    when.to_tokens(s)?;
                    s.append(TK_THEN, None)?;
                    then.to_tokens(s)?;
                }
                if let Some(ref else_expr) = else_expr {
                    s.append(TK_ELSE, None)?;
                    else_expr.to_tokens(s)?;
                }
                s.append(TK_END, None)
            }
            Self::Cast { expr, type_name } => {
                s.append(TK_CAST, None)?;
                s.append(TK_LP, None)?;
                expr.to_tokens(s)?;
                s.append(TK_AS, None)?;
                if let Some(ref type_name) = type_name {
                    type_name.to_tokens(s)?;
                }
                s.append(TK_RP, None)
            }
            Self::Collate(expr, collation) => {
                expr.to_tokens(s)?;
                s.append(TK_COLLATE, None)?;
                double_quote(collation, s)
            }
            Self::DoublyQualified(db_name, tbl_name, col_name) => {
                db_name.to_tokens(s)?;
                s.append(TK_DOT, None)?;
                tbl_name.to_tokens(s)?;
                s.append(TK_DOT, None)?;
                col_name.to_tokens(s)
            }
            Self::Exists(subquery) => {
                s.append(TK_EXISTS, None)?;
                s.append(TK_LP, None)?;
                subquery.to_tokens(s)?;
                s.append(TK_RP, None)
            }
            Self::FunctionCall {
                name,
                distinctness,
                args,
                order_by,
                filter_over,
            } => {
                name.to_tokens(s)?;
                s.append(TK_LP, None)?;
                if let Some(distinctness) = distinctness {
                    distinctness.to_tokens(s)?;
                }
                if let Some(args) = args {
                    comma(args, s)?;
                }
                if let Some(order_by) = order_by {
                    s.append(TK_ORDER, None)?;
                    s.append(TK_BY, None)?;
                    comma(order_by, s)?;
                }
                s.append(TK_RP, None)?;
                if let Some(filter_over) = filter_over {
                    filter_over.to_tokens(s)?;
                }
                Ok(())
            }
            Self::FunctionCallStar { name, filter_over } => {
                name.to_tokens(s)?;
                s.append(TK_LP, None)?;
                s.append(TK_STAR, None)?;
                s.append(TK_RP, None)?;
                if let Some(filter_over) = filter_over {
                    filter_over.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Id(id) => id.to_tokens(s),
            Self::Column { .. } => Ok(()),
            Self::InList { lhs, not, rhs } => {
                lhs.to_tokens(s)?;
                if *not {
                    s.append(TK_NOT, None)?;
                }
                s.append(TK_IN, None)?;
                s.append(TK_LP, None)?;
                if let Some(rhs) = rhs {
                    comma(rhs, s)?;
                }
                s.append(TK_RP, None)
            }
            Self::InSelect { lhs, not, rhs } => {
                lhs.to_tokens(s)?;
                if *not {
                    s.append(TK_NOT, None)?;
                }
                s.append(TK_IN, None)?;
                s.append(TK_LP, None)?;
                rhs.to_tokens(s)?;
                s.append(TK_RP, None)
            }
            Self::InTable {
                lhs,
                not,
                rhs,
                args,
            } => {
                lhs.to_tokens(s)?;
                if *not {
                    s.append(TK_NOT, None)?;
                }
                s.append(TK_IN, None)?;
                rhs.to_tokens(s)?;
                if let Some(args) = args {
                    s.append(TK_LP, None)?;
                    comma(args, s)?;
                    s.append(TK_RP, None)?;
                }
                Ok(())
            }
            Self::IsNull(sub_expr) => {
                sub_expr.to_tokens(s)?;
                s.append(TK_ISNULL, None)
            }
            Self::Like {
                lhs,
                not,
                op,
                rhs,
                escape,
            } => {
                lhs.to_tokens(s)?;
                if *not {
                    s.append(TK_NOT, None)?;
                }
                op.to_tokens(s)?;
                rhs.to_tokens(s)?;
                if let Some(escape) = escape {
                    s.append(TK_ESCAPE, None)?;
                    escape.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Literal(lit) => lit.to_tokens(s),
            Self::Name(name) => name.to_tokens(s),
            Self::NotNull(sub_expr) => {
                sub_expr.to_tokens(s)?;
                s.append(TK_NOTNULL, None)
            }
            Self::Parenthesized(exprs) => {
                s.append(TK_LP, None)?;
                comma(exprs, s)?;
                s.append(TK_RP, None)
            }
            Self::Qualified(qualifier, qualified) => {
                qualifier.to_tokens(s)?;
                s.append(TK_DOT, None)?;
                qualified.to_tokens(s)
            }
            Self::Raise(rt, err) => {
                s.append(TK_RAISE, None)?;
                s.append(TK_LP, None)?;
                rt.to_tokens(s)?;
                if let Some(err) = err {
                    s.append(TK_COMMA, None)?;
                    err.to_tokens(s)?;
                }
                s.append(TK_RP, None)
            }
            Self::RowId { .. } => Ok(()),
            Self::Subquery(query) => {
                s.append(TK_LP, None)?;
                query.to_tokens(s)?;
                s.append(TK_RP, None)
            }
            Self::Unary(op, sub_expr) => {
                op.to_tokens(s)?;
                sub_expr.to_tokens(s)
            }
            Self::Variable(var) => match var.chars().next() {
                Some(c) if c == '$' || c == '@' || c == '#' || c == ':' => {
                    s.append(TK_VARIABLE, Some(var))
                }
                Some(_) => s.append(TK_VARIABLE, Some(&("?".to_owned() + var))),
                None => s.append(TK_VARIABLE, Some("?")),
            },
        }
    }
}

impl Display for Expr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.to_fmt(f)
    }
}

impl ToTokens for Literal {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Numeric(ref num) => s.append(TK_FLOAT, Some(num)), // TODO Validate TK_FLOAT
            Self::String(ref str) => s.append(TK_STRING, Some(str)),
            Self::Blob(ref blob) => s.append(TK_BLOB, Some(blob)),
            Self::Keyword(ref str) => s.append(TK_ID, Some(str)), // TODO Validate TK_ID
            Self::Null => s.append(TK_NULL, None),
            Self::CurrentDate => s.append(TK_CTIME_KW, Some("CURRENT_DATE")),
            Self::CurrentTime => s.append(TK_CTIME_KW, Some("CURRENT_TIME")),
            Self::CurrentTimestamp => s.append(TK_CTIME_KW, Some("CURRENT_TIMESTAMP")),
        }
    }
}

impl ToTokens for LikeOperator {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            TK_LIKE_KW,
            Some(match self {
                Self::Glob => "GLOB",
                Self::Like => "LIKE",
                Self::Match => "MATCH",
                Self::Regexp => "REGEXP",
            }),
        )
    }
}

impl ToTokens for Operator {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Add => s.append(TK_PLUS, None),
            Self::And => s.append(TK_AND, None),
            Self::ArrowRight => s.append(TK_PTR, Some("->")),
            Self::ArrowRightShift => s.append(TK_PTR, Some("->>")),
            Self::BitwiseAnd => s.append(TK_BITAND, None),
            Self::BitwiseOr => s.append(TK_BITOR, None),
            Self::BitwiseNot => s.append(TK_BITNOT, None),
            Self::Concat => s.append(TK_CONCAT, None),
            Self::Equals => s.append(TK_EQ, None),
            Self::Divide => s.append(TK_SLASH, None),
            Self::Greater => s.append(TK_GT, None),
            Self::GreaterEquals => s.append(TK_GE, None),
            Self::Is => s.append(TK_IS, None),
            Self::IsNot => {
                s.append(TK_IS, None)?;
                s.append(TK_NOT, None)
            }
            Self::LeftShift => s.append(TK_LSHIFT, None),
            Self::Less => s.append(TK_LT, None),
            Self::LessEquals => s.append(TK_LE, None),
            Self::Modulus => s.append(TK_REM, None),
            Self::Multiply => s.append(TK_STAR, None),
            Self::NotEquals => s.append(TK_NE, None),
            Self::Or => s.append(TK_OR, None),
            Self::RightShift => s.append(TK_RSHIFT, None),
            Self::Subtract => s.append(TK_MINUS, None),
        }
    }
}

impl ToTokens for UnaryOperator {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::BitwiseNot => TK_BITNOT,
                Self::Negative => TK_MINUS,
                Self::Not => TK_NOT,
                Self::Positive => TK_PLUS,
            },
            None,
        )
    }
}

impl ToTokens for Select {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if let Some(ref with) = self.with {
            with.to_tokens(s)?;
        }
        self.body.to_tokens(s)?;
        if let Some(ref order_by) = self.order_by {
            s.append(TK_ORDER, None)?;
            s.append(TK_BY, None)?;
            comma(order_by, s)?;
        }
        if let Some(ref limit) = self.limit {
            limit.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for SelectBody {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.select.to_tokens(s)?;
        if let Some(ref compounds) = self.compounds {
            for compound in compounds {
                compound.to_tokens(s)?;
            }
        }
        Ok(())
    }
}

impl ToTokens for CompoundSelect {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.operator.to_tokens(s)?;
        self.select.to_tokens(s)
    }
}

impl ToTokens for CompoundOperator {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Union => s.append(TK_UNION, None),
            Self::UnionAll => {
                s.append(TK_UNION, None)?;
                s.append(TK_ALL, None)
            }
            Self::Except => s.append(TK_EXCEPT, None),
            Self::Intersect => s.append(TK_INTERSECT, None),
        }
    }
}

impl Display for CompoundOperator {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.to_fmt(f)
    }
}

impl ToTokens for OneSelect {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Select(select) => {
                let SelectInner {
                    distinctness,
                    columns,
                    from,
                    where_clause,
                    group_by,
                    window_clause,
                } = &**select;
                s.append(TK_SELECT, None)?;
                if let Some(ref distinctness) = distinctness {
                    distinctness.to_tokens(s)?;
                }
                comma(columns, s)?;
                if let Some(ref from) = from {
                    s.append(TK_FROM, None)?;
                    from.to_tokens(s)?;
                }
                if let Some(ref where_clause) = where_clause {
                    s.append(TK_WHERE, None)?;
                    where_clause.to_tokens(s)?;
                }
                if let Some(ref group_by) = group_by {
                    group_by.to_tokens(s)?;
                }
                if let Some(ref window_clause) = window_clause {
                    s.append(TK_WINDOW, None)?;
                    comma(window_clause, s)?;
                }
                Ok(())
            }
            Self::Values(values) => {
                for (i, vals) in values.iter().enumerate() {
                    if i == 0 {
                        s.append(TK_VALUES, None)?;
                    } else {
                        s.append(TK_COMMA, None)?;
                    }
                    s.append(TK_LP, None)?;
                    comma(vals, s)?;
                    s.append(TK_RP, None)?;
                }
                Ok(())
            }
        }
    }
}

impl ToTokens for FromClause {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.select
            .as_ref()
            .expect("FromClause select is mandatory per AST invariant")
            .to_tokens(s)?;
        if let Some(ref joins) = self.joins {
            for join in joins {
                join.to_tokens(s)?;
            }
        }
        Ok(())
    }
}

impl ToTokens for Distinctness {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::Distinct => TK_DISTINCT,
                Self::All => TK_ALL,
            },
            None,
        )
    }
}

impl ToTokens for ResultColumn {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Expr(expr, alias) => {
                expr.to_tokens(s)?;
                if let Some(alias) = alias {
                    alias.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Star => s.append(TK_STAR, None),
            Self::TableStar(tbl_name) => {
                tbl_name.to_tokens(s)?;
                s.append(TK_DOT, None)?;
                s.append(TK_STAR, None)
            }
        }
    }
}

impl ToTokens for As {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::As(ref name) => {
                s.append(TK_AS, None)?;
                name.to_tokens(s)
            }
            Self::Elided(ref name) => name.to_tokens(s),
        }
    }
}

impl ToTokens for JoinedSelectTable {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.operator.to_tokens(s)?;
        self.table.to_tokens(s)?;
        if let Some(ref constraint) = self.constraint {
            constraint.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for SelectTable {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Table(name, alias, indexed) => {
                name.to_tokens(s)?;
                if let Some(alias) = alias {
                    alias.to_tokens(s)?;
                }
                if let Some(indexed) = indexed {
                    indexed.to_tokens(s)?;
                }
                Ok(())
            }
            Self::TableCall(name, exprs, alias) => {
                name.to_tokens(s)?;
                s.append(TK_LP, None)?;
                if let Some(exprs) = exprs {
                    comma(exprs, s)?;
                }
                s.append(TK_RP, None)?;
                if let Some(alias) = alias {
                    alias.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Select(select, alias) => {
                s.append(TK_LP, None)?;
                select.to_tokens(s)?;
                s.append(TK_RP, None)?;
                if let Some(alias) = alias {
                    alias.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Sub(from, alias) => {
                s.append(TK_LP, None)?;
                from.to_tokens(s)?;
                s.append(TK_RP, None)?;
                if let Some(alias) = alias {
                    alias.to_tokens(s)?;
                }
                Ok(())
            }
        }
    }
}
