//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::dialect::TokenType::*;
use crate::dialect::{from_token, Token};
use crate::parser::{parse::YYCODETYPE, ParserError};
use indexmap::IndexMap;

use super::macros::TableOptions;
use super::types::{
    AlterTableBody, Distinctness, FromClause, FunctionTail, Limit, Name, Operator, ResultColumn,
    SelectTable, TransactionType, With,
};
use super::types_10::{
    CreateTrigger, Id, Indexed, IndexedColumn, Insert, JoinConstraint, JoinOperator, LikeOperator,
    Literal, ResolveType, Select, Set, TableReferenceId, Type, UnaryOperator,
};
use super::types_11::{
    ColumnDefinition, CreateVirtualTable, NamedTableConstraint, PragmaBody, QualifiedName,
    SortedColumn,
};

/// Compound operators
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/compound-operator.html
pub enum CompoundOperator {
    /// `UNION`
    Union,
    /// `UNION ALL`
    UnionAll,
    /// `EXCEPT`
    Except,
    /// `INTERSECT`
    Intersect,
}
/// SQL expression
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/expr.html
pub enum Expr {
    /// `BETWEEN`
    Between {
        /// expression
        lhs: Box<Expr>,
        /// `NOT`
        not: bool,
        /// start
        start: Box<Expr>,
        /// end
        end: Box<Expr>,
    },
    /// binary expression
    Binary(Box<Expr>, Operator, Box<Expr>),
    /// `CASE` expression
    Case {
        /// operand
        base: Option<Box<Expr>>,
        /// `WHEN` condition `THEN` result
        when_then_pairs: Vec<(Expr, Expr)>,
        /// `ELSE` result
        else_expr: Option<Box<Expr>>,
    },
    /// CAST expression
    Cast {
        /// expression
        expr: Box<Expr>,
        /// `AS` type name
        type_name: Option<Type>,
    },
    /// `COLLATE`: expression
    Collate(Box<Expr>, String),
    /// schema-name.table-name.column-name
    DoublyQualified(Name, Name, Name),
    /// `EXISTS` subquery
    Exists(Box<Select>),
    /// call to a built-in function
    FunctionCall {
        /// function name
        name: Id,
        /// `DISTINCT`
        distinctness: Option<Distinctness>,
        /// arguments
        args: Option<Vec<Expr>>,
        /// `ORDER BY`
        order_by: Option<Vec<SortedColumn>>,
        /// `FILTER`
        filter_over: Option<FunctionTail>,
    },
    /// Function call expression with '*' as arg
    FunctionCallStar {
        /// function name
        name: Id,
        /// `FILTER`
        filter_over: Option<FunctionTail>,
    },
    /// Identifier
    Id(Id),
    /// Column
    Column {
        /// the x in `x.y.z`. index of the db in catalog.
        database: Option<usize>,
        /// the y in `x.y.z`. index of the table in catalog.
        table: TableReferenceId,
        /// the z in `x.y.z`. index of the column in the table.
        column: usize,
        /// is the column a rowid alias
        is_rowid_alias: bool,
    },
    /// `ROWID`
    RowId {
        /// the x in `x.y.z`. index of the db in catalog.
        database: Option<usize>,
        /// the y in `x.y.z`. index of the table in catalog.
        table: TableReferenceId,
    },
    /// `IN`
    InList {
        /// expression
        lhs: Box<Expr>,
        /// `NOT`
        not: bool,
        /// values
        rhs: Option<Vec<Expr>>,
    },
    /// `IN` subselect
    InSelect {
        /// expression
        lhs: Box<Expr>,
        /// `NOT`
        not: bool,
        /// subquery
        rhs: Box<Select>,
    },
    /// `IN` table name / function
    InTable {
        /// expression
        lhs: Box<Expr>,
        /// `NOT`
        not: bool,
        /// table name
        rhs: QualifiedName,
        /// table function arguments
        args: Option<Vec<Expr>>,
    },
    /// `IS NULL`
    IsNull(Box<Expr>),
    /// `LIKE`
    Like {
        /// expression
        lhs: Box<Expr>,
        /// `NOT`
        not: bool,
        /// operator
        op: LikeOperator,
        /// pattern
        rhs: Box<Expr>,
        /// `ESCAPE` char
        escape: Option<Box<Expr>>,
    },
    /// Literal expression
    Literal(Literal),
    /// Name
    Name(Name),
    /// `NOT NULL` or `NOTNULL`
    NotNull(Box<Expr>),
    /// Parenthesized subexpression
    Parenthesized(Vec<Expr>),
    /// Qualified name
    Qualified(Name, Name),
    /// `RAISE` function call
    Raise(ResolveType, Option<Box<Expr>>),
    /// A value that already lives in a VDBE register.
    ///
    /// Never produced by the parser — the grammar has no syntax for it. It is
    /// created by the code generator when an expression tree has to reference a
    /// value that was computed earlier in the same program, most notably the
    /// `OLD.<col>` / `NEW.<col>` row images of a `CREATE TRIGGER` body (see
    /// `oxisqlite_core::translate::trigger`). Rewriting those references into
    /// `Register` before the body statement is translated means every existing
    /// planner/optimizer/emitter path handles them for free, with no special
    /// "am I inside a trigger?" plumbing threaded through the query planner.
    Register(usize),
    /// Subquery expression
    Subquery(Box<Select>),
    /// Unary expression
    Unary(UnaryOperator, Box<Expr>),
    /// Parameters
    Variable(String),
}
impl Expr {
    /// Constructor
    pub fn parenthesized(x: Self) -> Self {
        Self::Parenthesized(vec![x])
    }
    /// Constructor
    pub fn id(xt: YYCODETYPE, x: Token) -> Self {
        Self::Id(Id::from_token(xt, x))
    }
    /// Constructor
    pub fn collate(x: Self, ct: YYCODETYPE, c: Token) -> Self {
        Self::Collate(Box::new(x), from_token(ct, c))
    }
    /// Constructor
    pub fn cast(x: Self, type_name: Option<Type>) -> Self {
        Self::Cast {
            expr: Box::new(x),
            type_name,
        }
    }
    /// Constructor
    pub fn binary(left: Self, op: YYCODETYPE, right: Self) -> Self {
        Self::Binary(Box::new(left), Operator::from(op), Box::new(right))
    }
    /// Constructor
    pub fn ptr(left: Self, op: Token, right: Self) -> Self {
        let mut ptr = Operator::ArrowRight;
        if op.1 == "->>" {
            ptr = Operator::ArrowRightShift;
        }
        Self::Binary(Box::new(left), ptr, Box::new(right))
    }
    /// Constructor
    pub fn like(lhs: Self, not: bool, op: LikeOperator, rhs: Self, escape: Option<Self>) -> Self {
        Self::Like {
            lhs: Box::new(lhs),
            not,
            op,
            rhs: Box::new(rhs),
            escape: escape.map(Box::new),
        }
    }
    /// Constructor
    pub fn not_null(x: Self, op: YYCODETYPE) -> Self {
        if op == TK_ISNULL as YYCODETYPE {
            Self::IsNull(Box::new(x))
        } else if op == TK_NOTNULL as YYCODETYPE {
            Self::NotNull(Box::new(x))
        } else {
            unreachable!()
        }
    }
    /// Constructor
    pub fn unary(op: UnaryOperator, x: Self) -> Self {
        Self::Unary(op, Box::new(x))
    }
    /// Constructor
    pub fn between(lhs: Self, not: bool, start: Self, end: Self) -> Self {
        Self::Between {
            lhs: Box::new(lhs),
            not,
            start: Box::new(start),
            end: Box::new(end),
        }
    }
    /// Constructor
    pub fn in_list(lhs: Self, not: bool, rhs: Option<Vec<Self>>) -> Self {
        Self::InList {
            lhs: Box::new(lhs),
            not,
            rhs,
        }
    }
    /// Constructor
    pub fn in_select(lhs: Self, not: bool, rhs: Select) -> Self {
        Self::InSelect {
            lhs: Box::new(lhs),
            not,
            rhs: Box::new(rhs),
        }
    }
    /// Constructor
    pub fn in_table(lhs: Self, not: bool, rhs: QualifiedName, args: Option<Vec<Self>>) -> Self {
        Self::InTable {
            lhs: Box::new(lhs),
            not,
            rhs,
            args,
        }
    }
    /// Constructor
    pub fn sub_query(query: Select) -> Self {
        Self::Subquery(Box::new(query))
    }
}
/// `CREATE TABLE` body
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/lang_createtable.html
// https://sqlite.org/syntax/create-table-stmt.html
pub enum CreateTableBody {
    /// columns and constraints
    ColumnsAndConstraints {
        /// table column definitions
        columns: IndexMap<Name, ColumnDefinition>,
        /// table constraints
        constraints: Option<Vec<NamedTableConstraint>>,
        /// table options
        options: TableOptions,
    },
    /// `AS` select
    AsSelect(Box<Select>),
}
impl CreateTableBody {
    /// Constructor
    pub fn columns_and_constraints(
        columns: IndexMap<Name, ColumnDefinition>,
        constraints: Option<Vec<NamedTableConstraint>>,
        options: TableOptions,
    ) -> Result<Self, ParserError> {
        Ok(Self::ColumnsAndConstraints {
            columns,
            constraints,
            options,
        })
    }
}
/// `JOIN` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/join-clause.html
pub struct JoinedSelectTable {
    /// operator
    pub operator: JoinOperator,
    /// table
    pub table: SelectTable,
    /// constraint
    pub constraint: Option<JoinConstraint>,
}
/// SQL statement
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
// https://sqlite.org/syntax/sql-stmt.html
pub enum Stmt {
    /// `ALTER TABLE`: table name, body
    AlterTable(Box<(QualifiedName, AlterTableBody)>),
    /// `ANALYSE`: object name
    Analyze(Option<QualifiedName>),
    /// `ATTACH DATABASE`
    Attach {
        /// filename
        expr: Box<Expr>,
        /// schema name
        db_name: Box<Expr>,
        /// password
        key: Option<Box<Expr>>,
        /// `true` if the `DATABASE` keyword was present (`ATTACH DATABASE ...`
        /// vs `ATTACH ...`)
        database_kw: bool,
    },
    /// `BEGIN`: tx type, tx name
    Begin(Option<TransactionType>, Option<Name>),
    /// `COMMIT`/`END`: tx name
    Commit(Option<Name>), // TODO distinction between COMMIT and END
    /// `CREATE INDEX`
    CreateIndex {
        /// `UNIQUE`
        unique: bool,
        /// `IF NOT EXISTS`
        if_not_exists: bool,
        /// index name
        idx_name: Box<QualifiedName>,
        /// table name
        tbl_name: Name,
        /// indexed columns or expressions
        columns: Vec<SortedColumn>,
        /// partial index
        where_clause: Option<Box<Expr>>,
    },
    /// `CREATE TABLE`
    CreateTable {
        /// `TEMPORARY`
        temporary: bool, // TODO distinction between TEMP and TEMPORARY
        /// `IF NOT EXISTS`
        if_not_exists: bool,
        /// table name
        tbl_name: QualifiedName,
        /// table body
        body: Box<CreateTableBody>,
    },
    /// `CREATE TRIGGER`
    CreateTrigger(Box<CreateTrigger>),
    /// `CREATE VIEW`
    CreateView {
        /// `TEMPORARY`
        temporary: bool,
        /// `IF NOT EXISTS`
        if_not_exists: bool,
        /// view name
        view_name: QualifiedName,
        /// columns
        columns: Option<Vec<IndexedColumn>>,
        /// query
        select: Box<Select>,
    },
    /// `CREATE VIRTUAL TABLE`
    CreateVirtualTable(Box<CreateVirtualTable>),
    /// `DELETE`
    Delete(Box<Delete>),
    /// `DETACH DATABASE`: db name
    Detach(Box<Expr>), // TODO distinction between DETACH and DETACH DATABASE
    /// `DROP INDEX`
    DropIndex {
        /// `IF EXISTS`
        if_exists: bool,
        /// index name
        idx_name: QualifiedName,
    },
    /// `DROP TABLE`
    DropTable {
        /// `IF EXISTS`
        if_exists: bool,
        /// table name
        tbl_name: QualifiedName,
    },
    /// `DROP TRIGGER`
    DropTrigger {
        /// `IF EXISTS`
        if_exists: bool,
        /// trigger name
        trigger_name: QualifiedName,
    },
    /// `DROP VIEW`
    DropView {
        /// `IF EXISTS`
        if_exists: bool,
        /// view name
        view_name: QualifiedName,
    },
    /// `INSERT`
    Insert(Box<Insert>),
    /// `PRAGMA`: pragma name, body
    Pragma(Box<QualifiedName>, Option<Box<PragmaBody>>),
    /// `REINDEX`
    Reindex {
        /// collation or index or table name
        obj_name: Option<QualifiedName>,
    },
    /// `RELEASE`: savepoint name
    Release(Name), // TODO distinction between RELEASE and RELEASE SAVEPOINT
    /// `ROLLBACK`
    Rollback {
        /// transaction name
        tx_name: Option<Name>,
        /// savepoint name
        savepoint_name: Option<Name>, // TODO distinction between TO and TO SAVEPOINT
    },
    /// `SAVEPOINT`: savepoint name
    Savepoint(Name),
    /// `SELECT`
    Select(Box<Select>),
    /// `UPDATE`
    Update(Box<Update>),
    /// `VACUUM`: database name, into expr
    Vacuum(Option<Name>, Option<Box<Expr>>),
}
/// `UPDATE` clause
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Update {
    /// CTE
    pub with: Option<With>,
    /// `OR`
    pub or_conflict: Option<ResolveType>,
    /// table name
    pub tbl_name: QualifiedName,
    /// `INDEXED`
    pub indexed: Option<Indexed>,
    /// `SET` assignments
    pub sets: Vec<Set>,
    /// `FROM`
    pub from: Option<FromClause>,
    /// `WHERE` clause
    pub where_clause: Option<Box<Expr>>,
    /// `RETURNING`
    pub returning: Option<Vec<ResultColumn>>,
    /// `ORDER BY`
    pub order_by: Option<Vec<SortedColumn>>,
    /// `LIMIT`
    pub limit: Option<Box<Limit>>,
}
/// `DELETE`
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Delete {
    /// CTE
    pub with: Option<With>,
    /// `FROM` table name
    pub tbl_name: QualifiedName,
    /// `INDEXED`
    pub indexed: Option<Indexed>,
    /// `WHERE` clause
    pub where_clause: Option<Box<Expr>>,
    /// `RETURNING`
    pub returning: Option<Vec<ResultColumn>>,
    /// `ORDER BY`
    pub order_by: Option<Vec<SortedColumn>>,
    /// `LIMIT`
    pub limit: Option<Box<Limit>>,
}
