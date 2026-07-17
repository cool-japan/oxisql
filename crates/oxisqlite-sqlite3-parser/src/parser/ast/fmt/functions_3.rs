//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::ast::*;
use crate::dialect::TokenType::*;
use std::fmt::{self, Display, Formatter};
use std::ops::Deref;

use super::functions::{ToTokens, TokenStream};
use super::functions_4::{comma, double_quote};

impl ToTokens for JoinOperator {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Comma => s.append(TK_COMMA, None),
            Self::TypedJoin(join_type) => {
                if let Some(ref join_type) = join_type {
                    join_type.to_tokens(s)?;
                }
                s.append(TK_JOIN, None)
            }
        }
    }
}

impl ToTokens for JoinType {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if self.contains(Self::NATURAL) {
            s.append(TK_JOIN_KW, Some("NATURAL"))?;
        }
        if self.contains(Self::INNER) {
            if self.contains(Self::CROSS) {
                s.append(TK_JOIN_KW, Some("CROSS"))?;
            }
            s.append(TK_JOIN_KW, Some("INNER"))?;
        } else {
            if self.contains(Self::LEFT) {
                if self.contains(Self::RIGHT) {
                    s.append(TK_JOIN_KW, Some("FULL"))?;
                } else {
                    s.append(TK_JOIN_KW, Some("LEFT"))?;
                }
            } else if self.contains(Self::RIGHT) {
                s.append(TK_JOIN_KW, Some("RIGHT"))?;
            }
            if self.contains(Self::OUTER) {
                s.append(TK_JOIN_KW, Some("OUTER"))?;
            }
        }
        Ok(())
    }
}

impl ToTokens for JoinConstraint {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::On(expr) => {
                s.append(TK_ON, None)?;
                expr.to_tokens(s)
            }
            Self::Using(col_names) => {
                s.append(TK_USING, None)?;
                s.append(TK_LP, None)?;
                comma(col_names.deref(), s)?;
                s.append(TK_RP, None)
            }
        }
    }
}

impl ToTokens for GroupBy {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_GROUP, None)?;
        s.append(TK_BY, None)?;
        comma(&self.exprs, s)?;
        if let Some(ref having) = self.having {
            s.append(TK_HAVING, None)?;
            having.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for Id {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        double_quote(&self.0, s)
    }
}

impl ToTokens for Name {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        double_quote(self.0.as_str(), s)
    }
}

impl Display for Name {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.to_fmt(f)
    }
}

impl ToTokens for QualifiedName {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if let Some(ref db_name) = self.db_name {
            db_name.to_tokens(s)?;
            s.append(TK_DOT, None)?;
        }
        self.name.to_tokens(s)?;
        if let Some(ref alias) = self.alias {
            s.append(TK_AS, None)?;
            alias.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for AlterTableBody {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::RenameTo(name) => {
                s.append(TK_RENAME, None)?;
                s.append(TK_TO, None)?;
                name.to_tokens(s)
            }
            Self::AddColumn(def) => {
                s.append(TK_ADD, None)?;
                s.append(TK_COLUMNKW, None)?;
                def.to_tokens(s)
            }
            Self::RenameColumn { old, new } => {
                s.append(TK_RENAME, None)?;
                old.to_tokens(s)?;
                s.append(TK_TO, None)?;
                new.to_tokens(s)
            }
            Self::DropColumn(name) => {
                s.append(TK_DROP, None)?;
                s.append(TK_COLUMNKW, None)?;
                name.to_tokens(s)
            }
        }
    }
}

impl ToTokens for CreateTableBody {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::ColumnsAndConstraints {
                columns,
                constraints,
                options,
            } => {
                s.append(TK_LP, None)?;
                comma(columns.values(), s)?;
                if let Some(constraints) = constraints {
                    s.append(TK_COMMA, None)?;
                    comma(constraints, s)?;
                }
                s.append(TK_RP, None)?;
                if options.contains(TableOptions::WITHOUT_ROWID) {
                    s.append(TK_WITHOUT, None)?;
                    s.append(TK_ID, Some("ROWID"))?;
                }
                if options.contains(TableOptions::STRICT) {
                    s.append(TK_ID, Some("STRICT"))?;
                }
                Ok(())
            }
            Self::AsSelect(select) => {
                s.append(TK_AS, None)?;
                select.to_tokens(s)
            }
        }
    }
}

impl ToTokens for ColumnDefinition {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.col_name.to_tokens(s)?;
        if let Some(ref col_type) = self.col_type {
            col_type.to_tokens(s)?;
        }
        for constraint in &self.constraints {
            constraint.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for NamedColumnConstraint {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if let Some(ref name) = self.name {
            s.append(TK_CONSTRAINT, None)?;
            name.to_tokens(s)?;
        }
        self.constraint.to_tokens(s)
    }
}

impl ToTokens for ColumnConstraint {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::PrimaryKey {
                order,
                conflict_clause,
                auto_increment,
            } => {
                s.append(TK_PRIMARY, None)?;
                s.append(TK_KEY, None)?;
                if let Some(order) = order {
                    order.to_tokens(s)?;
                }
                if let Some(conflict_clause) = conflict_clause {
                    s.append(TK_ON, None)?;
                    s.append(TK_CONFLICT, None)?;
                    conflict_clause.to_tokens(s)?;
                }
                if *auto_increment {
                    s.append(TK_AUTOINCR, None)?;
                }
                Ok(())
            }
            Self::NotNull {
                nullable,
                conflict_clause,
            } => {
                if !nullable {
                    s.append(TK_NOT, None)?;
                }
                s.append(TK_NULL, None)?;
                if let Some(conflict_clause) = conflict_clause {
                    s.append(TK_ON, None)?;
                    s.append(TK_CONFLICT, None)?;
                    conflict_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Unique(conflict_clause) => {
                s.append(TK_UNIQUE, None)?;
                if let Some(conflict_clause) = conflict_clause {
                    s.append(TK_ON, None)?;
                    s.append(TK_CONFLICT, None)?;
                    conflict_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Check(expr) => {
                s.append(TK_CHECK, None)?;
                s.append(TK_LP, None)?;
                expr.to_tokens(s)?;
                s.append(TK_RP, None)
            }
            Self::Default(expr) => {
                s.append(TK_DEFAULT, None)?;
                expr.to_tokens(s)
            }
            Self::Defer(deref_clause) => deref_clause.to_tokens(s),
            Self::Collate { collation_name } => {
                s.append(TK_COLLATE, None)?;
                collation_name.to_tokens(s)
            }
            Self::ForeignKey {
                clause,
                deref_clause,
            } => {
                s.append(TK_REFERENCES, None)?;
                clause.to_tokens(s)?;
                if let Some(deref_clause) = deref_clause {
                    deref_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Generated { expr, typ } => {
                s.append(TK_AS, None)?;
                s.append(TK_LP, None)?;
                expr.to_tokens(s)?;
                s.append(TK_RP, None)?;
                if let Some(typ) = typ {
                    typ.to_tokens(s)?;
                }
                Ok(())
            }
        }
    }
}

impl ToTokens for NamedTableConstraint {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if let Some(ref name) = self.name {
            s.append(TK_CONSTRAINT, None)?;
            name.to_tokens(s)?;
        }
        self.constraint.to_tokens(s)
    }
}

impl ToTokens for TableConstraint {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::PrimaryKey {
                columns,
                auto_increment,
                conflict_clause,
            } => {
                s.append(TK_PRIMARY, None)?;
                s.append(TK_KEY, None)?;
                s.append(TK_LP, None)?;
                comma(columns, s)?;
                if *auto_increment {
                    s.append(TK_AUTOINCR, None)?;
                }
                s.append(TK_RP, None)?;
                if let Some(conflict_clause) = conflict_clause {
                    s.append(TK_ON, None)?;
                    s.append(TK_CONFLICT, None)?;
                    conflict_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Unique {
                columns,
                conflict_clause,
            } => {
                s.append(TK_UNIQUE, None)?;
                s.append(TK_LP, None)?;
                comma(columns, s)?;
                s.append(TK_RP, None)?;
                if let Some(conflict_clause) = conflict_clause {
                    s.append(TK_ON, None)?;
                    s.append(TK_CONFLICT, None)?;
                    conflict_clause.to_tokens(s)?;
                }
                Ok(())
            }
            Self::Check(expr) => {
                s.append(TK_CHECK, None)?;
                s.append(TK_LP, None)?;
                expr.to_tokens(s)?;
                s.append(TK_RP, None)
            }
            Self::ForeignKey {
                columns,
                clause,
                deref_clause,
            } => {
                s.append(TK_FOREIGN, None)?;
                s.append(TK_KEY, None)?;
                s.append(TK_LP, None)?;
                comma(columns, s)?;
                s.append(TK_RP, None)?;
                s.append(TK_REFERENCES, None)?;
                clause.to_tokens(s)?;
                if let Some(deref_clause) = deref_clause {
                    deref_clause.to_tokens(s)?;
                }
                Ok(())
            }
        }
    }
}

impl ToTokens for SortOrder {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(
            match self {
                Self::Asc => TK_ASC,
                Self::Desc => TK_DESC,
            },
            None,
        )
    }
}

impl ToTokens for NullsOrder {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_NULLS, None)?;
        s.append(
            match self {
                Self::First => TK_FIRST,
                Self::Last => TK_LAST,
            },
            None,
        )
    }
}

impl ToTokens for ForeignKeyClause {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.tbl_name.to_tokens(s)?;
        if let Some(ref columns) = self.columns {
            s.append(TK_LP, None)?;
            comma(columns, s)?;
            s.append(TK_RP, None)?;
        }
        for arg in &self.args {
            arg.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for RefArg {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::OnDelete(ref action) => {
                s.append(TK_ON, None)?;
                s.append(TK_DELETE, None)?;
                action.to_tokens(s)
            }
            Self::OnInsert(ref action) => {
                s.append(TK_ON, None)?;
                s.append(TK_INSERT, None)?;
                action.to_tokens(s)
            }
            Self::OnUpdate(ref action) => {
                s.append(TK_ON, None)?;
                s.append(TK_UPDATE, None)?;
                action.to_tokens(s)
            }
            Self::Match(ref name) => {
                s.append(TK_MATCH, None)?;
                name.to_tokens(s)
            }
        }
    }
}

impl ToTokens for RefAct {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::SetNull => {
                s.append(TK_SET, None)?;
                s.append(TK_NULL, None)
            }
            Self::SetDefault => {
                s.append(TK_SET, None)?;
                s.append(TK_DEFAULT, None)
            }
            Self::Cascade => s.append(TK_CASCADE, None),
            Self::Restrict => s.append(TK_RESTRICT, None),
            Self::NoAction => {
                s.append(TK_NO, None)?;
                s.append(TK_ACTION, None)
            }
        }
    }
}

impl ToTokens for DeferSubclause {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if !self.deferrable {
            s.append(TK_NOT, None)?;
        }
        s.append(TK_DEFERRABLE, None)?;
        if let Some(init_deferred) = self.init_deferred {
            init_deferred.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for InitDeferredPred {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_INITIALLY, None)?;
        s.append(
            match self {
                Self::InitiallyDeferred => TK_DEFERRED,
                Self::InitiallyImmediate => TK_IMMEDIATE,
            },
            None,
        )
    }
}

impl ToTokens for IndexedColumn {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.col_name.to_tokens(s)?;
        if let Some(ref collation_name) = self.collation_name {
            s.append(TK_COLLATE, None)?;
            collation_name.to_tokens(s)?;
        }
        if let Some(order) = self.order {
            order.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for Indexed {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::IndexedBy(ref name) => {
                s.append(TK_INDEXED, None)?;
                s.append(TK_BY, None)?;
                name.to_tokens(s)
            }
            Self::NotIndexed => {
                s.append(TK_NOT, None)?;
                s.append(TK_INDEXED, None)
            }
        }
    }
}

impl ToTokens for SortedColumn {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        self.expr.to_tokens(s)?;
        if let Some(ref order) = self.order {
            order.to_tokens(s)?;
        }
        if let Some(ref nulls) = self.nulls {
            nulls.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for Limit {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        s.append(TK_LIMIT, None)?;
        self.expr.to_tokens(s)?;
        if let Some(ref offset) = self.offset {
            s.append(TK_OFFSET, None)?;
            offset.to_tokens(s)?;
        }
        Ok(())
    }
}

impl ToTokens for InsertBody {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        match self {
            Self::Select(select, upsert) => {
                select.to_tokens(s)?;
                if let Some(upsert) = upsert {
                    upsert.to_tokens(s)?;
                }
                Ok(())
            }
            Self::DefaultValues => {
                s.append(TK_DEFAULT, None)?;
                s.append(TK_VALUES, None)
            }
        }
    }
}

impl ToTokens for Set {
    fn to_tokens<S: TokenStream>(&self, s: &mut S) -> Result<(), S::Error> {
        if self.col_names.len() == 1 {
            comma(self.col_names.deref(), s)?;
        } else {
            s.append(TK_LP, None)?;
            comma(self.col_names.deref(), s)?;
            s.append(TK_RP, None)?;
        }
        s.append(TK_EQ, None)?;
        self.expr.to_tokens(s)
    }
}
