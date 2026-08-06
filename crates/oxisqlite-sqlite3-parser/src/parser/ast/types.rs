//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::custom_err;
use crate::dialect::{from_token, Token};
use crate::parser::{parse::YYCODETYPE, ParserError};
use indexmap::IndexSet;
use strum_macros::{EnumIter, EnumString};

use super::types_10::{
    DeferSubclause, ForeignKeyClause, Id, Indexed, JoinConstraint, JoinOperator, Over,
    QuotedIterator, RefAct, ResolveType, Select, Set, SortOrder, Window,
};
use super::types_11::{ColumnDefinition, CommonTableExpr, QualifiedName, SortedColumn};
use super::types_9::{Expr, JoinedSelectTable};

/// Ordered set of distinct column names
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DistinctNames(pub(super) IndexSet<Name>);
impl DistinctNames {
    /// Initialize
    pub fn new(name: Name) -> Self {
        let mut dn = Self(IndexSet::new());
        dn.0.insert(name);
        dn
    }
    /// Single column name
    pub fn single(name: Name) -> Self {
        let mut dn = Self(IndexSet::with_capacity(1));
        dn.0.insert(name);
        dn
    }
    /// Push a distinct name or fail
    pub fn insert(&mut self, name: Name) -> Result<(), ParserError> {
        if self.0.contains(&name) {
            return Err(custom_err!("column \"{}\" specified more than once", name));
        }
        self.0.insert(name);
        Ok(())
    }
}
/// `SELECT` ... `FROM` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/join-clause.html
pub struct FromClause {
    /// table
    pub select: Option<Box<SelectTable>>, // FIXME mandatory
    /// `JOIN`ed tabled
    pub joins: Option<Vec<JoinedSelectTable>>,
    pub(super) op: Option<JoinOperator>, // FIXME transient
}
impl FromClause {
    pub(crate) fn empty() -> Self {
        Self {
            select: None,
            joins: None,
            op: None,
        }
    }
    pub(crate) fn push(
        &mut self,
        table: SelectTable,
        jc: Option<JoinConstraint>,
    ) -> Result<(), ParserError> {
        let op = self.op.take();
        if let Some(op) = op {
            let jst = JoinedSelectTable {
                operator: op,
                table,
                constraint: jc,
            };
            if jst.operator.is_natural() && jst.constraint.is_some() {
                return Err(custom_err!(
                    "a NATURAL join may not have an ON or USING clause"
                ));
            }
            if let Some(ref mut joins) = self.joins {
                joins.push(jst);
            } else {
                self.joins = Some(vec![jst]);
            }
        } else {
            if jc.is_some() {
                return Err(custom_err!("a JOIN clause is required before ON"));
            }
            debug_assert!(self.select.is_none());
            debug_assert!(self.joins.is_none());
            self.select = Some(Box::new(table));
        }
        Ok(())
    }
    pub(crate) fn push_op(&mut self, op: JoinOperator) {
        self.op = Some(op);
    }
}
/// `ALTER TABLE` body
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_altertable.html
pub enum AlterTableBody {
    /// `RENAME TO`: new table name
    RenameTo(Name),
    /// `ADD COLUMN`
    AddColumn(ColumnDefinition), // TODO distinction between ADD and ADD COLUMN
    /// `RENAME COLUMN`
    RenameColumn {
        /// old name
        old: Name,
        /// new name
        new: Name,
    },
    /// `DROP COLUMN`
    DropColumn(Name), // TODO distinction between DROP and DROP COLUMN
}
/// Alias
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum As {
    /// `AS`
    As(Name),
    /// no `AS`
    Elided(Name), // FIXME Ids
}
/// `GROUP BY`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct GroupBy {
    /// expressions
    pub exprs: Vec<Expr>,
    /// `HAVING`
    pub having: Option<Box<Expr>>, // HAVING clause on a non-aggregate query
}
/// `PRAGMA` value
#[derive(Clone, Debug, PartialEq, Eq, EnumIter, EnumString, strum::Display)]
#[strum(serialize_all = "snake_case")]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/pragma.html
pub enum PragmaName {
    /// `application_id` pragma — the 32-bit signed application identifier
    /// stored at offset 68 of the database header.
    ApplicationId,
    /// set the autovacuum mode
    AutoVacuum,
    /// `cache_size` pragma
    CacheSize,
    /// returns foreign key constraint information for a table
    ForeignKeyList,
    /// returns one row for each column in a named index (`index_info(idx)`)
    IndexInfo,
    /// returns one row for each index associated with a table (`index_list(tbl)`)
    IndexList,
    /// Run integrity check on the database file
    IntegrityCheck,
    /// `journal_mode` pragma
    JournalMode,
    /// Noop as per SQLite docs
    LegacyFileFormat,
    /// Return the total number of pages in the database file.
    PageCount,
    /// Return the page size of the database in bytes.
    PageSize,
    /// Returns schema version of the database file.
    SchemaVersion,
    /// `synchronous` pragma — controls fsync durability level (OFF/NORMAL/FULL/EXTRA).
    Synchronous,
    /// returns information about the columns of a table
    TableInfo,
    /// Returns the user version of the database file.
    UserVersion,
    /// trigger a checkpoint to run on database(s) if WAL is enabled
    WalCheckpoint,
}
/// `WITH` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_with.html
// https://sqlite.org/syntax/with-clause.html
pub struct With {
    /// `RECURSIVE`
    pub recursive: bool,
    /// CTEs
    pub ctes: Vec<CommonTableExpr>,
}
/// `SELECT` or `RETURNING` result column
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/result-column.html
pub enum ResultColumn {
    /// expression
    Expr(Expr, Option<As>),
    /// `*`
    Star,
    /// table name.`*`
    TableStar(Name),
}
/// foreign-key reference args
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RefArg {
    /// `ON DELETE`
    OnDelete(RefAct),
    /// `ON INSERT`
    OnInsert(RefAct),
    /// `ON UPDATE`
    OnUpdate(RefAct),
    /// `MATCH`
    Match(Name),
}
/// `DELETE` trigger command
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TriggerCmdDelete {
    /// table name
    pub tbl_name: Name,
    /// `WHERE` clause
    pub where_clause: Option<Expr>,
}
/// Function call tail
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FunctionTail {
    /// `FILTER` clause
    pub filter_clause: Option<Box<Expr>>,
    /// `OVER` clause
    pub over_clause: Option<Box<Over>>,
}
/// `OVER` window definition
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WindowDef {
    /// window name
    pub name: Name,
    /// window definition
    pub window: Window,
}
/// Transaction types
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TransactionType {
    /// `DEFERRED`
    Deferred, // default
    /// `IMMEDIATE`
    Immediate,
    /// `EXCLUSIVE`
    Exclusive,
}
/// Frame bounds
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum FrameBound {
    /// `CURRENT ROW`
    CurrentRow,
    /// `FOLLOWING`
    Following(Box<Expr>),
    /// `PRECEDING`
    Preceding(Box<Expr>),
    /// `UNBOUNDED FOLLOWING`
    UnboundedFollowing,
    /// `UNBOUNDED PRECEDING`
    UnboundedPreceding,
}
/// Upsert `DO` action
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UpsertDo {
    /// `SET`
    Set {
        /// assignments
        sets: Vec<Set>,
        /// `WHERE` clause
        where_clause: Option<Expr>,
    },
    /// `NOTHING`
    Nothing,
}
/// `SELECT` distinctness
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Distinctness {
    /// `DISTINCT`
    Distinct,
    /// `ALL`
    All,
}
/// `INSERT` trigger command
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct TriggerCmdInsert {
    /// `OR`
    pub or_conflict: Option<ResolveType>,
    /// table name
    pub tbl_name: Name,
    /// `COLUMNS`
    pub col_names: Option<DistinctNames>,
    /// `SELECT` or `VALUES`
    pub select: Box<Select>,
    /// `ON CONFLICT` clause
    pub upsert: Option<Upsert>,
    /// `RETURNING`
    pub returning: Option<Vec<ResultColumn>>,
}
/// SQL operators
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Operator {
    /// `+`
    Add,
    /// `AND`
    And,
    /// `->`
    ArrowRight,
    /// `->>`
    ArrowRightShift,
    /// `&`
    BitwiseAnd,
    /// `|`
    BitwiseOr,
    /// `~`
    BitwiseNot,
    /// String concatenation (`||`)
    Concat,
    /// `=` or `==`
    Equals,
    /// `/`
    Divide,
    /// `>`
    Greater,
    /// `>=`
    GreaterEquals,
    /// `IS`
    Is,
    /// `IS NOT`
    IsNot,
    /// `<<`
    LeftShift,
    /// `<`
    Less,
    /// `<=`
    LessEquals,
    /// `%`
    Modulus,
    /// `*`
    Multiply,
    /// `!=` or `<>`
    NotEquals,
    /// `OR`
    Or,
    /// `>>`
    RightShift,
    /// `-`
    Subtract,
}
impl Operator {
    /// returns whether order of operations can be ignored
    pub fn is_commutative(&self) -> bool {
        matches!(
            self,
            Operator::Add
                | Operator::Multiply
                | Operator::BitwiseAnd
                | Operator::BitwiseOr
                | Operator::Equals
                | Operator::NotEquals
        )
    }
    /// Returns true if this operator is a comparison operator that may need affinity conversion
    pub fn is_comparison(&self) -> bool {
        matches!(
            self,
            Self::Equals
                | Self::NotEquals
                | Self::Less
                | Self::LessEquals
                | Self::Greater
                | Self::GreaterEquals
                | Self::Is
                | Self::IsNot
        )
    }
}
/// Column constraint
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/column-constraint.html
pub enum ColumnConstraint {
    /// `PRIMARY KEY`
    PrimaryKey {
        /// `ASC` / `DESC`
        order: Option<SortOrder>,
        /// `ON CONFLICT` clause
        conflict_clause: Option<ResolveType>,
        /// `AUTOINCREMENT`
        auto_increment: bool,
    },
    /// `NULL`
    NotNull {
        /// `NOT`
        nullable: bool,
        /// `ON CONFLICT` clause
        conflict_clause: Option<ResolveType>,
    },
    /// `UNIQUE`
    Unique(Option<ResolveType>),
    /// `CHECK`
    Check(Expr),
    /// `DEFAULT`
    Default(Expr),
    /// `DEFERRABLE`
    Defer(DeferSubclause), // FIXME
    /// `COLLATE`
    Collate {
        /// collation name
        collation_name: Name, // FIXME Ids
    },
    /// `REFERENCES` foreign-key clause
    ForeignKey {
        /// clause
        clause: ForeignKeyClause,
        /// `DEFERRABLE`
        deref_clause: Option<DeferSubclause>,
    },
    /// `GENERATED`
    Generated {
        /// expression
        expr: Expr,
        /// `STORED` / `VIRTUAL`
        typ: Option<Id>,
    },
}
/// `LIMIT`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Limit {
    /// count
    pub expr: Expr,
    /// `OFFSET`
    pub offset: Option<Expr>, // TODO distinction between LIMIT offset, count and LIMIT count OFFSET offset
}
/// Table or subquery
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/table-or-subquery.html
pub enum SelectTable {
    /// table
    Table(QualifiedName, Option<As>, Option<Indexed>),
    /// table function call
    TableCall(QualifiedName, Option<Vec<Expr>>, Option<As>),
    /// `SELECT` subquery
    Select(Box<Select>, Option<As>),
    /// subquery
    Sub(FromClause, Option<As>),
}
/// Upsert conflict targets
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UpsertIndex {
    /// columns
    pub targets: Vec<SortedColumn>,
    /// `WHERE` clause
    pub where_clause: Option<Expr>,
}
/// Upsert clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_upsert.html
// https://sqlite.org/syntax/upsert-clause.html
pub struct Upsert {
    /// conflict targets
    pub index: Option<Box<UpsertIndex>>,
    /// `DO` clause
    pub do_clause: Box<UpsertDo>,
    /// next upsert
    pub next: Option<Box<Upsert>>,
}
// TODO ids (identifier or string)

/// identifier or string or `CROSS` or `FULL` or `INNER` or `LEFT` or `NATURAL` or `OUTER` or `RIGHT`.
#[derive(Clone, Debug, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Name(pub String); // TODO distinction between Name and "Name"/[Name]/`Name`
impl Name {
    /// Constructor
    pub fn from_token(ty: YYCODETYPE, token: Token) -> Self {
        Self(from_token(ty, token))
    }
    pub(super) fn as_bytes(&self) -> QuotedIterator<'_> {
        if self.0.is_empty() {
            return QuotedIterator(self.0.bytes(), 0);
        }
        let bytes = self.0.as_bytes();
        let mut quote = bytes[0];
        if quote != b'"' && quote != b'`' && quote != b'\'' && quote != b'[' {
            return QuotedIterator(self.0.bytes(), 0);
        } else if quote == b'[' {
            quote = b']';
        }
        debug_assert!(bytes.len() > 1);
        debug_assert_eq!(quote, bytes[bytes.len() - 1]);
        let sub = &self.0.as_str()[1..bytes.len() - 1];
        if quote == b']' {
            return QuotedIterator(sub.bytes(), 0); // no escape
        }
        QuotedIterator(sub.bytes(), quote)
    }
    /// Logical (unquoted, unescaped) form of this name, e.g. `"a""b"` becomes
    /// `a"b` and `[my col]` becomes `my col`. Names that were never quoted
    /// are returned unchanged.
    ///
    /// Intended for human-readable messages; use [`ToTokens`](fmt::ToTokens)
    /// (or `Display`) instead to render a re-parseable, quote-preserving form.
    pub(crate) fn unquoted(&self) -> String {
        // `as_bytes()` only ever strips/un-escapes ASCII quote characters, so
        // the result stays valid UTF-8 whenever the original name was.
        String::from_utf8(self.as_bytes().collect()).unwrap_or_else(|_| self.0.clone())
    }
}
/// `CREATE TRIGGER` time
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TriggerTime {
    /// `BEFORE`
    Before, // default
    /// `AFTER`
    After,
    /// `INSTEAD OF`
    InsteadOf,
}
/// Named column constraint
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/column-constraint.html
pub struct NamedColumnConstraint {
    /// constraint name
    pub name: Option<Name>,
    /// constraint
    pub constraint: ColumnConstraint,
}
