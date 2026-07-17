//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::custom_err;
use crate::dialect::TokenType::*;
use crate::dialect::{from_token, Token};
use crate::parser::{parse::YYCODETYPE, ParserError};
use std::str::{self, Bytes};

use super::macros::JoinType;
use super::types::{
    DistinctNames, Limit, Name, RefArg, ResultColumn, TriggerCmdDelete, TriggerCmdInsert,
    TriggerTime, Upsert, With,
};
use super::types_11::{
    FrameClause, InitDeferredPred, OneSelect, QualifiedName, SortedColumn, TriggerCmdUpdate,
};
use super::types_9::{CompoundOperator, Expr, Stmt};

/// SQL literal
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Literal {
    /// Number
    Numeric(String),
    /// String
    // TODO Check that string is already quoted and correctly escaped
    String(String),
    /// BLOB
    // TODO Check that string is valid (only hexa)
    Blob(String),
    /// Keyword
    Keyword(String),
    /// `NULL`
    Null,
    /// `CURRENT_DATE`
    CurrentDate,
    /// `CURRENT_TIME`
    CurrentTime,
    /// `CURRENT_TIMESTAMP`
    CurrentTimestamp,
}
impl Literal {
    /// Constructor
    pub fn from_ctime_kw(token: Token) -> Self {
        if token.1.eq_ignore_ascii_case("CURRENT_DATE") {
            Self::CurrentDate
        } else if token.1.eq_ignore_ascii_case("CURRENT_TIME") {
            Self::CurrentTime
        } else if token.1.eq_ignore_ascii_case("CURRENT_TIMESTAMP") {
            Self::CurrentTimestamp
        } else {
            unreachable!()
        }
    }
}
/// `INSERT` body
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_insert.html
// https://sqlite.org/syntax/insert-stmt.html
pub enum InsertBody {
    /// `SELECT` or `VALUES`
    Select(Box<Select>, Option<Upsert>),
    /// `DEFAULT VALUES`
    DefaultValues,
}
/// identifier or one of several keywords or `INDEXED`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Id(pub String);
impl Id {
    /// Constructor
    pub fn from_token(ty: YYCODETYPE, token: Token) -> Self {
        Self(from_token(ty, token))
    }
}
/// Sort orders
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum SortOrder {
    /// `ASC`
    Asc,
    /// `DESC`
    Desc,
}
/// Statement or Explain statement
#[derive(Clone, Debug, PartialEq, Eq)]
// https://sqlite.org/syntax/sql-stmt.html
pub enum Cmd {
    /// `EXPLAIN` statement
    Explain(Stmt),
    /// `EXPLAIN QUERY PLAN` statement
    ExplainQueryPlan(Stmt),
    /// statement
    Stmt(Stmt),
}
/// Textual comparison operator in an expression
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum LikeOperator {
    /// `GLOB`
    Glob,
    /// `LIKE`
    Like,
    /// `MATCH`
    Match,
    /// `REGEXP`
    Regexp,
}
impl LikeOperator {
    /// Constructor
    pub fn from_token(token_type: YYCODETYPE, token: Token) -> Self {
        if token_type == TK_MATCH as YYCODETYPE {
            return Self::Match;
        } else if token_type == TK_LIKE_KW as YYCODETYPE {
            let token = token.1;
            if token.eq_ignore_ascii_case("LIKE") {
                return Self::Like;
            } else if token.eq_ignore_ascii_case("GLOB") {
                return Self::Glob;
            } else if token.eq_ignore_ascii_case("REGEXP") {
                return Self::Regexp;
            }
        }
        unreachable!()
    }
}
/// `INSERT`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Insert {
    /// CTE
    pub with: Option<With>,
    /// `OR`
    pub or_conflict: Option<ResolveType>, // TODO distinction between REPLACE and INSERT OR REPLACE
    /// table name
    pub tbl_name: QualifiedName,
    /// `COLUMNS`
    pub columns: Option<DistinctNames>,
    /// `VALUES` or `SELECT`
    pub body: InsertBody,
    /// `RETURNING`
    pub returning: Option<Vec<ResultColumn>>,
}
/// foreign-key defer clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeferSubclause {
    /// `DEFERRABLE`
    pub deferrable: bool,
    /// `INITIALLY` `DEFERRED` / `IMMEDIATE`
    pub init_deferred: Option<InitDeferredPred>,
}
/// Unary operators
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UnaryOperator {
    /// bitwise negation (`~`)
    BitwiseNot,
    /// negative-sign
    Negative,
    /// `NOT`
    Not,
    /// positive-sign
    Positive,
}
/// Compound select
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CompoundSelect {
    /// operator
    pub operator: CompoundOperator,
    /// select
    pub select: Box<OneSelect>,
}
/// Window definition
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/window-defn.html
pub struct Window {
    /// base window name
    pub base: Option<Name>,
    /// `PARTITION BY`
    pub partition_by: Option<Vec<Expr>>,
    /// `ORDER BY`
    pub order_by: Option<Vec<SortedColumn>>,
    /// frame spec
    pub frame_clause: Option<FrameClause>,
}
/// Frame modes
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FrameMode {
    /// `GROUPS`
    Groups,
    /// `RANGE`
    Range,
    /// `ROWS`
    Rows,
}
/// Frame exclusions
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FrameExclude {
    /// `NO OTHERS`
    NoOthers,
    /// `CURRENT ROW`
    CurrentRow,
    /// `GROUP`
    Group,
    /// `TIES`
    Ties,
}
/// `CREATE TRIGGER` event
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TriggerEvent {
    /// `DELETE`
    Delete,
    /// `INSERT`
    Insert,
    /// `UPDATE`
    Update,
    /// `UPDATE OF`: col names
    UpdateOf(DistinctNames),
}
/// foreign-key reference actions
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RefAct {
    /// `SET NULL`
    SetNull,
    /// `SET DEFAULT`
    SetDefault,
    /// `CASCADE`
    Cascade,
    /// `RESTRICT`
    Restrict,
    /// `NO ACTION`
    NoAction,
}
/// Function call `OVER` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/over-clause.html
pub enum Over {
    /// Window definition
    Window(Window),
    /// Window name
    Name(Name),
}
/// `JOIN` constraint
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum JoinConstraint {
    /// `ON`
    On(Expr),
    /// `USING`: col names
    Using(DistinctNames),
}
/// Indexed column
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/indexed-column.html
pub struct IndexedColumn {
    /// column name
    pub col_name: Name,
    /// `COLLATE`
    pub collation_name: Option<Name>, // FIXME Ids
    /// `ORDER BY`
    pub order: Option<SortOrder>,
}
/// `UPDATE ... SET`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Set {
    /// column name(s)
    pub col_names: DistinctNames,
    /// expression
    pub expr: Expr,
}
/// `INDEXED BY` / `NOT INDEXED`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Indexed {
    /// `INDEXED BY`: idx name
    IndexedBy(Name),
    /// `NOT INDEXED`
    NotIndexed,
}
/// Column type
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/type-name.html
pub struct Type {
    /// type name
    pub name: String, // TODO Validate: Ids+
    /// type size
    pub size: Option<TypeSize>,
}
/// `SELECT` statement
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_select.html
// https://sqlite.org/syntax/factored-select-stmt.html
pub struct Select {
    /// CTE
    pub with: Option<With>,
    /// body
    pub body: SelectBody,
    /// `ORDER BY`
    pub order_by: Option<Vec<SortedColumn>>, // ORDER BY term does not match any column in the result set
    /// `LIMIT`
    pub limit: Option<Box<Limit>>,
}
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
/// Internal ID of a table reference.
///
/// Used by [Expr::Column] and [Expr::RowId] to refer to a table.
/// E.g. in 'SELECT * FROM t UNION ALL SELECT * FROM t', there are two table references,
/// so there are two TableReferenceIds.
pub struct TableReferenceId(pub(super) usize);
/// Conflict resolution types
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ResolveType {
    /// `ROLLBACK`
    Rollback,
    /// `ABORT`
    Abort, // default
    /// `FAIL`
    Fail,
    /// `IGNORE`
    Ignore,
    /// `REPLACE`
    Replace,
}
impl ResolveType {
    /// Get the OE_XXX bit value
    pub fn bit_value(&self) -> usize {
        match self {
            ResolveType::Rollback => 1,
            ResolveType::Abort => 2,
            ResolveType::Fail => 3,
            ResolveType::Ignore => 4,
            ResolveType::Replace => 5,
        }
    }
}
/// Column type size limit(s)
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/type-name.html
pub enum TypeSize {
    /// maximum size
    MaxSize(Box<Expr>),
    /// precision
    TypeSize(Box<Expr>, Box<Expr>),
}
/// `CREATE TRIGGER` command
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_createtrigger.html
// https://sqlite.org/syntax/create-trigger-stmt.html
pub enum TriggerCmd {
    /// `UPDATE`
    Update(Box<TriggerCmdUpdate>),
    /// `INSERT`
    Insert(Box<TriggerCmdInsert>),
    /// `DELETE`
    Delete(Box<TriggerCmdDelete>),
    /// `SELECT`
    Select(Box<Select>),
}
/// `SELECT` body
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SelectBody {
    /// first select
    pub select: Box<OneSelect>,
    /// compounds
    pub compounds: Option<Vec<CompoundSelect>>,
}
impl SelectBody {
    pub(crate) fn push(&mut self, cs: CompoundSelect) -> Result<(), ParserError> {
        use crate::ast::check::ColumnCount;
        if let ColumnCount::Fixed(n) = self.select.column_count() {
            if let ColumnCount::Fixed(m) = cs.select.column_count() {
                if n != m {
                    return Err(
                        custom_err!(
                            "SELECTs to the left and right of {} do not have the same number of result columns",
                            cs.operator
                        ),
                    );
                }
            }
        }
        if let Some(ref mut v) = self.compounds {
            v.push(cs);
        } else {
            self.compounds = Some(vec![cs]);
        }
        Ok(())
    }
}
/// `CREATE TRIGGER
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct CreateTrigger {
    /// `TEMPORARY`
    pub temporary: bool,
    /// `IF NOT EXISTS`
    pub if_not_exists: bool,
    /// trigger name
    pub trigger_name: QualifiedName,
    /// `BEFORE`/`AFTER`/`INSTEAD OF`
    pub time: Option<TriggerTime>,
    /// `DELETE`/`INSERT`/`UPDATE`
    pub event: TriggerEvent,
    /// table name
    pub tbl_name: QualifiedName,
    /// `FOR EACH ROW`
    pub for_each_row: bool,
    /// `WHEN`
    pub when_clause: Option<Expr>,
    /// statements
    pub commands: Vec<TriggerCmd>,
}
/// Table constraint
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/table-constraint.html
pub enum TableConstraint {
    /// `PRIMARY KEY`
    PrimaryKey {
        /// columns
        columns: Vec<SortedColumn>,
        /// `AUTOINCREMENT`
        auto_increment: bool,
        /// `ON CONFLICT` clause
        conflict_clause: Option<ResolveType>,
    },
    /// `UNIQUE`
    Unique {
        /// columns
        columns: Vec<SortedColumn>,
        /// `ON CONFLICT` clause
        conflict_clause: Option<ResolveType>,
    },
    /// `CHECK`
    Check(Expr),
    /// `FOREIGN KEY`
    ForeignKey {
        /// columns
        columns: Vec<IndexedColumn>,
        /// `REFERENCES`
        clause: ForeignKeyClause,
        /// `DEFERRABLE`
        deref_clause: Option<DeferSubclause>,
    },
}
/// Join operators
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/join-operator.html
pub enum JoinOperator {
    /// `,`
    Comma,
    /// `JOIN`
    TypedJoin(Option<JoinType>),
}
impl JoinOperator {
    pub(crate) fn from(
        token: Token,
        n1: Option<Name>,
        n2: Option<Name>,
    ) -> Result<Self, ParserError> {
        Ok({
            let mut jt = JoinType::try_from(token.1.as_bytes())?;
            for n in [&n1, &n2].into_iter().flatten() {
                jt |= JoinType::try_from(n.0.as_ref())?;
            }
            if (jt & (JoinType::INNER | JoinType::OUTER)) == (JoinType::INNER | JoinType::OUTER)
                || (jt & (JoinType::OUTER | JoinType::LEFT | JoinType::RIGHT)) == JoinType::OUTER
            {
                return Err(custom_err!(
                    "unsupported JOIN type: {:?} {:?} {:?}",
                    str::from_utf8(token.1.as_bytes()),
                    n1,
                    n2
                ));
            }
            Self::TypedJoin(Some(jt))
        })
    }
    pub(super) fn is_natural(&self) -> bool {
        match self {
            Self::TypedJoin(Some(jt)) => jt.contains(JoinType::NATURAL),
            _ => false,
        }
    }
}
/// `REFERENCES` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/foreign-key-clause.html
pub struct ForeignKeyClause {
    /// foreign table name
    pub tbl_name: Name,
    /// foreign table columns
    pub columns: Option<Vec<IndexedColumn>>,
    /// referential action(s) / deferrable option(s)
    pub args: Vec<RefArg>,
}
pub(super) struct QuotedIterator<'s>(pub(super) Bytes<'s>, pub(super) u8);
