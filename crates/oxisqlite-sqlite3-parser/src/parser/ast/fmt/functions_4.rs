//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::ast::*;
use crate::dialect::is_identifier;
use crate::dialect::TokenType::*;
use std::ops::Deref;

use super::functions::{ToTokens, TokenStream};

impl ToTokens for PragmaBody {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Equals(value) => {
                s.append(TK_EQ, None)?;
                value.to_tokens(s)
            }
            Self::Call(value) => {
                s.append(TK_LP, None)?;
                value.to_tokens(s)?;
                s.append(TK_RP, None)
            }
        }
    }
}

impl ToTokens for TriggerTime {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Before => s.append(TK_BEFORE, None),
            Self::After => s.append(TK_AFTER, None),
            Self::InsteadOf => {
                s.append(TK_INSTEAD, None)?;
                s.append(TK_OF, None)
            }
        }
    }
}

impl ToTokens for TriggerEvent {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Delete => s.append(TK_DELETE, None),
            Self::Insert => s.append(TK_INSERT, None),
            Self::Update => s.append(TK_UPDATE, None),
            Self::UpdateOf(ref col_names) => {
                s.append(TK_UPDATE, None)?;
                s.append(TK_OF, None)?;
                comma(col_names.deref(), s)
            }
        }
    }
}

impl ToTokens for TriggerCmd {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Update(update) => {
                let TriggerCmdUpdate {
                    or_conflict,
                    tbl_name,
                    sets,
                    from,
                    where_clause,
                } = &**update;
                s.append(TK_UPDATE, None)?;
                if let Some(or_conflict) = or_conflict {
                    s.append(TK_OR, None)?;
                    or_conflict.to_tokens(s)?;
                }
                tbl_name.to_tokens(s)?;
                s.append(TK_SET, None)?;
                comma(sets, s)?;
                if let Some(from) = from {
                    s.append(TK_FROM, None)?;
                    from.to_tokens(s)?;
                }
                if let Some(where_clause) = where_clause {
                    s.append(TK_WHERE, None)?;
                    where_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Insert(insert) => {
                let TriggerCmdInsert {
                    or_conflict,
                    tbl_name,
                    col_names,
                    select,
                    upsert,
                    returning,
                } = &**insert;
                if let Some(ResolveType::Replace) = or_conflict {
                    s.append(TK_REPLACE, None)?;
                } else {
                    s.append(TK_INSERT, None)?;
                    if let Some(or_conflict) = or_conflict {
                        s.append(TK_OR, None)?;
                        or_conflict.to_tokens(s)?;
                    }
                }
                s.append(TK_INTO, None)?;
                tbl_name.to_tokens(s)?;
                if let Some(col_names) = col_names {
                    s.append(TK_LP, None)?;
                    comma(col_names.deref(), s)?;
                    s.append(TK_RP, None)?;
                }
                select.to_tokens(s)?;
                if let Some(upsert) = upsert {
                    upsert.to_tokens(s)?;
                }
                if let Some(returning) = returning {
                    s.append(TK_RETURNING, None)?;
                    comma(returning, s)?;
                }
                Ok(())
            }
            Self::Delete(delete) => {
                s.append(TK_DELETE, None)?;
                s.append(TK_FROM, None)?;
                delete.tbl_name.to_tokens(s)?;
                if let Some(where_clause) = &delete.where_clause {
                    s.append(TK_WHERE, None)?;
                    where_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Select(select) => select.to_tokens(s),
        }
    }
}

impl ToTokens for ResolveType {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::Rollback => TK_ROLLBACK,
                Self::Abort => TK_ABORT,
                Self::Fail => TK_FAIL,
                Self::Ignore => TK_IGNORE,
                Self::Replace => TK_REPLACE,
            },
            None,
        )
    }
}

impl ToTokens for With {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_WITH, None)?;
        if self.recursive {
            s.append(TK_RECURSIVE, None)?;
        }
        comma(&self.ctes, s)
    }
}

impl ToTokens for CommonTableExpr {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.tbl_name.to_tokens(s)?;
        if let Some(ref columns) = self.columns {
            s.append(TK_LP, None)?;
            comma(columns, s)?;
            s.append(TK_RP, None)?;
        }
        s.append(TK_AS, None)?;
        match self.materialized {
            Materialized::Any => {}
            Materialized::Yes => {
                s.append(TK_MATERIALIZED, None)?;
            }
            Materialized::No => {
                s.append(TK_NOT, None)?;
                s.append(TK_MATERIALIZED, None)?;
            }
        };
        s.append(TK_LP, None)?;
        self.select.to_tokens(s)?;
        s.append(TK_RP, None)
    }
}

impl ToTokens for Type {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self.size {
            None => s.append(TK_ID, Some(&self.name)),
            Some(ref size) => {
                s.append(TK_ID, Some(&self.name))?; // TODO check there is no forbidden chars
                s.append(TK_LP, None)?;
                size.to_tokens(s)?;
                s.append(TK_RP, None)
            }
        }
    }
}

impl ToTokens for TypeSize {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::MaxSize(size) => size.to_tokens(s),
            Self::TypeSize(size1, size2) => {
                size1.to_tokens(s)?;
                s.append(TK_COMMA, None)?;
                size2.to_tokens(s)
            }
        }
    }
}

impl ToTokens for TransactionType {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::Deferred => TK_DEFERRED,
                Self::Immediate => TK_IMMEDIATE,
                Self::Exclusive => TK_EXCLUSIVE,
            },
            None,
        )
    }
}

impl ToTokens for Upsert {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_ON, None)?;
        s.append(TK_CONFLICT, None)?;
        if let Some(ref index) = self.index {
            index.to_tokens(s)?;
        }
        self.do_clause.to_tokens(s)?;
        if let Some(ref next) = self.next {
            next.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for UpsertIndex {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_LP, None)?;
        comma(&self.targets, s)?;
        s.append(TK_RP, None)?;
        if let Some(ref where_clause) = self.where_clause {
            s.append(TK_WHERE, None)?;
            where_clause.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for UpsertDo {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Set { sets, where_clause } => {
                s.append(TK_DO, None)?;
                s.append(TK_UPDATE, None)?;
                s.append(TK_SET, None)?;
                comma(sets, s)?;
                if let Some(where_clause) = where_clause {
                    s.append(TK_WHERE, None)?;
                    where_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Nothing => {
                s.append(TK_DO, None)?;
                s.append(TK_NOTHING, None)
            }
        }
    }
}

impl ToTokens for FunctionTail {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if let Some(ref filter_clause) = self.filter_clause {
            s.append(TK_FILTER, None)?;
            s.append(TK_LP, None)?;
            s.append(TK_WHERE, None)?;
            filter_clause.to_tokens(s)?;
            s.append(TK_RP, None)?;
        }
        if let Some(ref over_clause) = self.over_clause {
            s.append(TK_OVER, None)?;
            over_clause.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for Over {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Window(ref window) => window.to_tokens(s),
            Self::Name(ref name) => name.to_tokens(s),
        }
    }
}

impl ToTokens for WindowDef {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.name.to_tokens(s)?;
        s.append(TK_AS, None)?;
        self.window.to_tokens(s)
    }
}

impl ToTokens for Window {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_LP, None)?;
        if let Some(ref base) = self.base {
            base.to_tokens(s)?;
        }
        if let Some(ref partition_by) = self.partition_by {
            s.append(TK_PARTITION, None)?;
            s.append(TK_BY, None)?;
            comma(partition_by, s)?;
        }
        if let Some(ref order_by) = self.order_by {
            s.append(TK_ORDER, None)?;
            s.append(TK_BY, None)?;
            comma(order_by, s)?;
        }
        if let Some(ref frame_clause) = self.frame_clause {
            frame_clause.to_tokens(s)?;
        }
        s.append(TK_RP, None)
    }
}

impl ToTokens for FrameClause {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.mode.to_tokens(s)?;
        if let Some(ref end) = self.end {
            s.append(TK_BETWEEN, None)?;
            self.start.to_tokens(s)?;
            s.append(TK_AND, None)?;
            end.to_tokens(s)?;
        } else {
            self.start.to_tokens(s)?;
        }
        if let Some(ref exclude) = self.exclude {
            s.append(TK_EXCLUDE, None)?;
            exclude.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for FrameMode {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::Groups => TK_GROUPS,
                Self::Range => TK_RANGE,
                Self::Rows => TK_ROWS,
            },
            None,
        )
    }
}

impl ToTokens for FrameBound {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::CurrentRow => {
                s.append(TK_CURRENT, None)?;
                s.append(TK_ROW, None)
            }
            Self::Following(value) => {
                value.to_tokens(s)?;
                s.append(TK_FOLLOWING, None)
            }
            Self::Preceding(value) => {
                value.to_tokens(s)?;
                s.append(TK_PRECEDING, None)
            }
            Self::UnboundedFollowing => {
                s.append(TK_UNBOUNDED, None)?;
                s.append(TK_FOLLOWING, None)
            }
            Self::UnboundedPreceding => {
                s.append(TK_UNBOUNDED, None)?;
                s.append(TK_PRECEDING, None)
            }
        }
    }
}

impl ToTokens for FrameExclude {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::NoOthers => {
                s.append(TK_NO, None)?;
                s.append(TK_OTHERS, None)
            }
            Self::CurrentRow => {
                s.append(TK_CURRENT, None)?;
                s.append(TK_ROW, None)
            }
            Self::Group => s.append(TK_GROUP, None),
            Self::Ties => s.append(TK_TIES, None),
        }
    }
}

pub(super) fn comma<I, S: TokenStream>(items: I, s: &mut S) -> Result<(), S::Error>
where
    I: IntoIterator,
    I::Item: ToTokens,
{
    let iter = items.into_iter();
    for (i, item) in iter.enumerate() {
        if i != 0 {
            s.append(TK_COMMA, None)?;
        }
        item.to_tokens(s)?;
    }
    Ok(())
}

pub(super) fn double_quote<S: TokenStream>(name: &str, s: &mut S) -> Result<(), S::Error> {
    if name.is_empty() {
        return s.append(TK_ID, Some("\"\""));
    }
    if is_identifier(name) {
        // identifier must be quoted when they match a keyword...
        /*if is_keyword(name) {
            f.write_char('`')?;
            f.write_str(name)?;
            return f.write_char('`');
        }*/
        return s.append(TK_ID, Some(name));
    }
    /*f.write_char('"')?;
    for c in name.chars() {
        if c == '"' {
            f.write_char(c)?;
        }
        f.write_char(c)?;
    }
    f.write_char('"')*/
    s.append(TK_ID, Some(name))
}
