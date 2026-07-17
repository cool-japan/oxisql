//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::custom_err;
use crate::parser::ParserError;
use indexmap::{IndexMap, IndexSet};
use std::str::{self};

use super::type_aliases::PragmaValue;
use super::types::{
    ColumnConstraint, Distinctness, FrameBound, FromClause, GroupBy, Name, NamedColumnConstraint,
    ResultColumn, WindowDef,
};
use super::types_10::{
    ForeignKeyClause, FrameExclude, FrameMode, IndexedColumn, ResolveType, Select, Set, SortOrder,
    TableConstraint, Type,
};
use super::types_9::Expr;

/// Table column definition
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/column-def.html
pub struct ColumnDefinition {
    /// column name
    pub col_name: Name,
    /// column type
    pub col_type: Option<Type>,
    /// column constraints
    pub constraints: Vec<NamedColumnConstraint>,
}
impl ColumnDefinition {
    /// Constructor
    pub fn add_column(columns: &mut IndexMap<Name, Self>, mut cd: Self) -> Result<(), ParserError> {
        let col_name = &cd.col_name;
        if columns.contains_key(col_name) {
            return Err(custom_err!(
                "duplicate column name: {}",
                col_name.unquoted()
            ));
        }
        // https://github.com/sqlite/sqlite/blob/e452bf40a14aca57fd9047b330dff282f3e4bbcc/src/build.c#L1511-L1514
        if let Some(ref mut col_type) = cd.col_type {
            let mut split = col_type.name.split_ascii_whitespace();
            let truncate = if split
                .next_back()
                .is_some_and(|s| s.eq_ignore_ascii_case("ALWAYS"))
                && split
                    .next_back()
                    .is_some_and(|s| s.eq_ignore_ascii_case("GENERATED"))
            {
                let mut generated = false;
                for constraint in &cd.constraints {
                    if let ColumnConstraint::Generated { .. } = constraint.constraint {
                        generated = true;
                        break;
                    }
                }
                generated
            } else {
                false
            };
            if truncate {
                // str_split_whitespace_remainder
                let new_type: Vec<&str> = split.collect();
                col_type.name = new_type.join(" ");
            }
        }
        for constraint in &cd.constraints {
            if let ColumnConstraint::ForeignKey {
                clause:
                    ForeignKeyClause {
                        tbl_name, columns, ..
                    },
                ..
            } = &constraint.constraint
            {
                // The child table may reference the primary key of the parent without specifying the primary key column
                if columns.as_ref().map_or(0, Vec::len) > 1 {
                    return Err(custom_err!(
                        "foreign key on {} should reference only one column of table {}",
                        col_name,
                        tbl_name
                    ));
                }
            }
        }
        columns.insert(col_name.clone(), cd);
        Ok(())
    }
}
/// Sorted column
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SortedColumn {
    /// expression
    pub expr: Expr,
    /// `ASC` / `DESC`
    pub order: Option<SortOrder>,
    /// `NULLS FIRST` / `NULLS LAST`
    pub nulls: Option<NullsOrder>,
}
/// Frame specification
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/frame-spec.html
pub struct FrameClause {
    /// unit
    pub mode: FrameMode,
    /// start bound
    pub start: FrameBound,
    /// end bound
    pub end: Option<FrameBound>,
    /// `EXCLUDE`
    pub exclude: Option<FrameExclude>,
}
/// `?` or `$` Prepared statement arg placeholder(s)
#[derive(Default)]
pub struct ParameterInfo {
    /// Number of SQL parameters in a prepared statement, like `sqlite3_bind_parameter_count`
    pub count: u32,
    /// Parameter name(s) if any
    pub names: IndexSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// `SELECT` core
pub struct SelectInner {
    /// `DISTINCT`
    pub distinctness: Option<Distinctness>,
    /// columns
    pub columns: Vec<ResultColumn>,
    /// `FROM` clause
    pub from: Option<FromClause>,
    /// `WHERE` clause
    pub where_clause: Option<Expr>,
    /// `GROUP BY`
    pub group_by: Option<GroupBy>,
    /// `WINDOW` definition
    pub window_clause: Option<Vec<WindowDef>>,
}
/// CTE materialization
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Materialized {
    /// No hint
    Any,
    /// `MATERIALIZED`
    Yes,
    /// `NOT MATERIALIZED`
    No,
}
/// `PRAGMA` body
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/pragma-stmt.html
pub enum PragmaBody {
    /// `=`
    Equals(PragmaValue),
    /// function call
    Call(PragmaValue),
}
/// `NULLS FIRST` or `NULLS LAST`
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum NullsOrder {
    /// `NULLS FIRST`
    First,
    /// `NULLS LAST`
    Last,
}
/// `CREATE VIRTUAL TABLE`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CreateVirtualTable {
    /// `IF NOT EXISTS`
    pub if_not_exists: bool,
    /// table name
    pub tbl_name: QualifiedName,
    /// module name
    pub module_name: Name,
    /// args
    pub args: Option<Vec<String>>, // TODO smol str
}
/// CTE
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/common-table-expression.html
pub struct CommonTableExpr {
    /// table name
    pub tbl_name: Name,
    /// table columns
    pub columns: Option<Vec<IndexedColumn>>, // check no duplicate
    /// `MATERIALIZED`
    pub materialized: Materialized,
    /// query
    pub select: Box<Select>,
}
impl CommonTableExpr {
    /// Constructor
    pub fn add_cte(ctes: &mut Vec<Self>, cte: Self) -> Result<(), ParserError> {
        if ctes.iter().any(|c| c.tbl_name == cte.tbl_name) {
            return Err(custom_err!("duplicate WITH table name: {}", cte.tbl_name));
        }
        ctes.push(cte);
        Ok(())
    }
}
/// `INITIALLY` `DEFERRED` / `IMMEDIATE`
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum InitDeferredPred {
    /// `INITIALLY DEFERRED`
    InitiallyDeferred,
    /// `INITIALLY IMMEDIATE`
    InitiallyImmediate, // default
}
pub(crate) enum ExplainKind {
    Explain,
    QueryPlan,
}
/// Named table constraint
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/table-constraint.html
pub struct NamedTableConstraint {
    /// constraint name
    pub name: Option<Name>,
    /// constraint
    pub constraint: TableConstraint,
}
/// `SELECT` core
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/select-core.html
pub enum OneSelect {
    /// `SELECT`
    Select(Box<SelectInner>),
    /// `VALUES`
    Values(Vec<Vec<Expr>>),
}
/// `UPDATE` trigger command
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TriggerCmdUpdate {
    /// `OR`
    pub or_conflict: Option<ResolveType>,
    /// table name
    pub tbl_name: Name,
    /// `SET` assignments
    pub sets: Vec<Set>,
    /// `FROM`
    pub from: Option<FromClause>,
    /// `WHERE` clause
    pub where_clause: Option<Expr>,
}
/// Qualified name
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct QualifiedName {
    /// schema
    pub db_name: Option<Name>,
    /// object name
    pub name: Name,
    /// alias
    pub alias: Option<Name>, // FIXME restrict alias usage (fullname vs xfullname)
}
impl QualifiedName {
    /// Constructor
    pub fn single(name: Name) -> Self {
        Self {
            db_name: None,
            name,
            alias: None,
        }
    }
    /// Constructor
    pub fn fullname(db_name: Name, name: Name) -> Self {
        Self {
            db_name: Some(db_name),
            name,
            alias: None,
        }
    }
    /// Constructor
    pub fn xfullname(db_name: Name, name: Name, alias: Name) -> Self {
        Self {
            db_name: Some(db_name),
            name,
            alias: Some(alias),
        }
    }
    /// Constructor
    pub fn alias(name: Name, alias: Name) -> Self {
        Self {
            db_name: None,
            name,
            alias: Some(alias),
        }
    }
}
