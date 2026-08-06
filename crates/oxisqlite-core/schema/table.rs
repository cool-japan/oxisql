//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::translate::collate::CollationSeq;
use crate::translate::plan::{Plan, QueryDestination};
use crate::VirtualTable;
use crate::{util::normalize_ident, Result};
use fallible_iterator::FallibleIterator;
use limbo_sqlite3_parser::ast::{Expr, Literal, ResolveType, Select, SortOrder, TableOptions};
use limbo_sqlite3_parser::{
    ast::{Cmd, CreateTableBody, QualifiedName, Stmt},
    lexer::sql::Parser,
};
use std::collections::BTreeSet;
use std::rc::Rc;
use tracing::trace;

use super::column::{Column, Type};

/// A table-level `UNIQUE(col1, col2, ...)` constraint, together with the
/// `ON CONFLICT <action>` resolution declared on it (defaults to
/// [`ResolveType::Abort`] when unspecified, matching SQLite's default).
#[derive(Clone, Debug)]
pub struct UniqueSet {
    pub columns: Vec<(String, SortOrder)>,
    pub resolve: ResolveType,
}

#[derive(Clone, Debug)]
pub struct BTreeTable {
    pub root_page: usize,
    /// Which database of the owning connection this table lives in: 0 = `main`,
    /// 1 = `temp`, 2.. = `ATTACH`ed (see [`crate::multidb`]). `root_page` is
    /// only meaningful relative to that database's pager, so every cursor-open
    /// opcode carries this index alongside the root page.
    pub db_index: usize,
    pub name: String,
    pub primary_key_columns: Vec<(String, SortOrder)>,
    pub columns: Vec<Column>,
    pub has_rowid: bool,
    pub is_strict: bool,
    pub unique_sets: Option<Vec<UniqueSet>>,
    /// The `ON CONFLICT <action>` resolution declared on this table's
    /// `PRIMARY KEY` constraint (table-level `PRIMARY KEY(...)` or
    /// column-level `col ... PRIMARY KEY`). Meaningless when
    /// `primary_key_columns` is empty. Defaults to [`ResolveType::Abort`]
    /// when unspecified. Independent from a statement-level
    /// `INSERT/UPDATE OR <action>`, which takes precedence when present.
    pub primary_key_conflict: ResolveType,
    /// Foreign-key constraints declared on this table.
    pub foreign_keys: Vec<ForeignKeyDef>,
}
impl BTreeTable {
    pub fn get_rowid_alias_column(&self) -> Option<(usize, &Column)> {
        if self.primary_key_columns.len() == 1 {
            let (idx, col) = self.get_column(&self.primary_key_columns[0].0)?;
            if self.column_is_rowid_alias(col) {
                return Some((idx, col));
            }
        }
        None
    }
    pub fn column_is_rowid_alias(&self, col: &Column) -> bool {
        col.is_rowid_alias
    }
    /// Returns the column position and column for a given column name.
    /// Returns None if the column name is not found.
    /// E.g. if table is CREATE TABLE t(a, b, c)
    /// then get_column("b") returns (1, &Column { .. })
    pub fn get_column(&self, name: &str) -> Option<(usize, &Column)> {
        let name = normalize_ident(name);
        self.columns
            .iter()
            .enumerate()
            .find(|(_, column)| column.name.as_ref() == Some(&name))
    }
    pub fn from_sql(sql: &str, root_page: usize) -> Result<BTreeTable> {
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser.next()?;
        match cmd {
            Some(Cmd::Stmt(Stmt::CreateTable { tbl_name, body, .. })) => {
                create_table(tbl_name, *body, root_page)
            }
            // The sole caller (schema reload via `ParseSchema`/reopen) only ever
            // feeds this function trusted, self-generated SQL text read back from
            // `sqlite_schema`. Reaching this arm means that text no longer parses
            // as a CREATE TABLE statement, i.e. the persisted schema is corrupt.
            _ => crate::bail_corrupt_error!(
                "malformed sqlite_schema entry: expected a CREATE TABLE statement, got: {}",
                sql
            ),
        }
    }
    pub fn to_sql(&self) -> String {
        let mut sql = format!("CREATE TABLE {} (", self.name);
        for (i, column) in self.columns.iter().enumerate() {
            if i > 0 {
                sql.push(',');
            }
            sql.push(' ');
            sql.push_str(column.name.as_ref().expect("column name is None"));
            sql.push(' ');
            sql.push_str(&column.ty.to_string());
            if column.unique {
                sql.push_str(" UNIQUE");
                if column.unique_conflict != ResolveType::Abort {
                    sql.push_str(" ON CONFLICT ");
                    sql.push_str(resolve_type_to_str(column.unique_conflict));
                }
            }
            if column.primary_key {
                sql.push_str(" PRIMARY KEY");
                if self.primary_key_conflict != ResolveType::Abort {
                    sql.push_str(" ON CONFLICT ");
                    sql.push_str(resolve_type_to_str(self.primary_key_conflict));
                }
            }
            if let Some(default) = &column.default {
                sql.push_str(" DEFAULT ");
                sql.push_str(&default.to_string());
            }
        }
        sql.push_str(" )");
        sql
    }
    pub fn column_collations(&self) -> Vec<Option<CollationSeq>> {
        self.columns.iter().map(|column| column.collation).collect()
    }
}
/// One foreign-key constraint stored in a [`BTreeTable`].
///
/// Mirrors the 8-column shape of `PRAGMA foreign_key_list`:
/// `id, seq, table, from, to, on_update, on_delete, match`.
#[derive(Clone, Debug)]
pub struct ForeignKeyDef {
    /// FK index within the table (0-based).
    pub id: u32,
    /// Child (referencing) column names, ordered by `seq`.
    pub from_cols: Vec<String>,
    /// Parent (referenced) table name.
    pub to_table: String,
    /// Parent column names (empty = use parent PK).
    pub to_cols: Vec<String>,
    /// Action on `UPDATE` of parent row.  Default `"NO ACTION"`.
    pub on_update: String,
    /// Action on `DELETE` of parent row.  Default `"NO ACTION"`.
    pub on_delete: String,
    /// `MATCH` clause.  Default `"NONE"`.
    pub match_clause: String,
}
/// A derived table from a FROM clause subquery.
#[derive(Debug, Clone)]
pub struct FromClauseSubquery {
    /// The name of the derived table; uses the alias if available.
    pub name: String,
    /// The query plan for the derived table. A [`Plan`] (rather than a plain
    /// `SelectPlan`) so that a compound (`UNION [ALL]`/`INTERSECT`/`EXCEPT`)
    /// body -- e.g. a view whose definition is a `UNION ALL` chain -- can also
    /// be used as a FROM-clause subquery.
    pub plan: Box<Plan>,
    /// The columns of the derived table.
    pub columns: Vec<Column>,
    /// The start register for the result columns of the derived table;
    /// must be set before data is read from it.
    pub result_columns_start_reg: Option<usize>,
}

impl FromClauseSubquery {
    /// The coroutine [`QueryDestination`] this subquery yields rows through, read
    /// from the (rightmost) SELECT arm of its plan. `None` for a non-SELECT plan
    /// (which never occurs for a FROM-clause subquery body).
    pub fn query_destination(&self) -> Option<&QueryDestination> {
        match self.plan.as_ref() {
            Plan::Select(select) => Some(&select.query_destination),
            Plan::CompoundSelect { right_most, .. } => Some(&right_most.query_destination),
            Plan::Delete(_) | Plan::Update(_) => None,
        }
    }
}
/// A `CREATE VIEW` object registered in the schema.
///
/// A view has no B-tree of its own: whenever its name is referenced in a FROM
/// clause it is expanded into a [`FromClauseSubquery`] by re-planning its stored
/// `SELECT` body (see `translate::planner::parse_from_clause_table`). The
/// `columns` list is inferred once (from the body's result columns, or from the
/// explicit `CREATE VIEW v(a, b, c)` column list when present) and is used to
/// answer name/`PRAGMA table_info` lookups without re-planning.
#[derive(Debug, Clone)]
pub struct View {
    /// Normalized view name.
    pub name: String,
    /// The original `CREATE VIEW ...` SQL text as persisted in `sqlite_schema`.
    pub sql: String,
    /// The parsed `SELECT` body, re-planned on every reference.
    pub select: Rc<Select>,
    /// The explicit column-list names from `CREATE VIEW v(a, b, c) AS ...`, if any.
    pub explicit_column_names: Option<Vec<String>>,
    /// The inferred output columns. Empty until column resolution succeeds (a
    /// malformed/cyclic view keeps an empty list but stays name-resolvable).
    pub columns: Vec<Column>,
}

impl View {
    /// Parse a `CREATE VIEW` statement read back from `sqlite_schema` into a
    /// placeholder [`View`] (with an empty `columns` list, resolved later).
    ///
    /// Mirrors [`BTreeTable::from_sql`]: the sole caller feeds trusted,
    /// self-generated SQL text, so anything that isn't a `CREATE VIEW` means the
    /// persisted schema is corrupt.
    pub fn from_sql(sql: &str, fallback_name: &str) -> Result<View> {
        let mut parser = Parser::new(sql.as_bytes());
        match parser.next()? {
            Some(Cmd::Stmt(Stmt::CreateView {
                view_name,
                columns,
                select,
                ..
            })) => {
                let explicit_column_names = columns.map(|cols| {
                    cols.iter()
                        .map(|c| normalize_ident(c.col_name.0.as_str()))
                        .collect::<Vec<_>>()
                });
                let name = normalize_ident(view_name.name.0.as_str());
                Ok(View {
                    name,
                    sql: sql.to_string(),
                    select: Rc::new(*select),
                    explicit_column_names,
                    columns: Vec::new(),
                })
            }
            _ => crate::bail_corrupt_error!(
                "malformed sqlite_schema entry: expected a CREATE VIEW statement for {}, got: {}",
                fallback_name,
                sql
            ),
        }
    }
}

#[derive(Debug, Default)]
pub struct PseudoTable {
    pub columns: Vec<Column>,
}
impl PseudoTable {
    pub fn new() -> Self {
        Self { columns: vec![] }
    }
    pub fn new_with_columns(columns: Vec<Column>) -> Self {
        Self { columns }
    }
    pub fn add_column(&mut self, name: &str, ty: Type, primary_key: bool) {
        self.columns.push(Column {
            name: Some(normalize_ident(name)),
            ty,
            ty_str: ty.to_string().to_uppercase(),
            primary_key,
            is_rowid_alias: false,
            notnull: false,
            default: None,
            unique: false,
            unique_conflict: ResolveType::Abort,
            collation: None,
            is_generated: false,
        });
    }
    pub fn get_column(&self, name: &str) -> Option<(usize, &Column)> {
        let name = normalize_ident(name);
        for (i, column) in self.columns.iter().enumerate() {
            if column.name.as_ref().map_or(false, |n| *n == name) {
                return Some((i, column));
            }
        }
        None
    }
}
#[derive(Clone, Debug)]
pub enum Table {
    BTree(Rc<BTreeTable>),
    Pseudo(Rc<PseudoTable>),
    Virtual(Rc<VirtualTable>),
    FromClauseSubquery(FromClauseSubquery),
    /// A `CREATE VIEW` object. A `Table::View` must never survive past name
    /// resolution: `translate::planner::parse_from_clause_table` expands it into
    /// a [`FromClauseSubquery`] before planning proceeds.
    View(Rc<View>),
}
impl Table {
    /// The database registry index this object lives in.
    ///
    /// Only B-tree tables have a pager-relative root page; every other kind
    /// (pseudo/virtual/subquery/view) has no b-tree of its own and reports
    /// `main`.
    pub fn db_index(&self) -> usize {
        match self {
            Table::BTree(btree) => btree.db_index,
            _ => crate::multidb::DB_MAIN,
        }
    }

    /// Returns the B-tree root page for this table.
    ///
    /// # Errors
    ///
    /// Returns [`crate::LimboError::InternalError`] for table kinds that have
    /// no B-tree root page ([`Table::Pseudo`], [`Table::Virtual`],
    /// [`Table::FromClauseSubquery`], [`Table::View`]). These are expected to
    /// be internal planner invariants (never reachable once the caller has
    /// already dispatched on table kind), but a typed error lets a planner
    /// regression surface as a query error instead of aborting the process.
    pub fn get_root_page(&self) -> Result<usize> {
        match self {
            Table::BTree(table) => Ok(table.root_page),
            Table::Pseudo(_) => Err(crate::LimboError::InternalError(
                "Pseudo table has no B-tree root page".to_string(),
            )),
            Table::Virtual(_) => Err(crate::LimboError::InternalError(
                "Virtual table has no B-tree root page".to_string(),
            )),
            Table::FromClauseSubquery(_) => Err(crate::LimboError::InternalError(
                "FromClauseSubquery has no B-tree root page".to_string(),
            )),
            Table::View(_) => Err(crate::LimboError::InternalError(
                "View has no B-tree root page".to_string(),
            )),
        }
    }
    pub fn get_name(&self) -> &str {
        match self {
            Self::BTree(table) => &table.name,
            Self::Pseudo(_) => "",
            Self::Virtual(table) => &table.name,
            Self::FromClauseSubquery(from_clause_subquery) => &from_clause_subquery.name,
            Self::View(view) => &view.name,
        }
    }
    pub fn get_column_at(&self, index: usize) -> Option<&Column> {
        match self {
            Self::BTree(table) => table.columns.get(index),
            Self::Pseudo(table) => table.columns.get(index),
            Self::Virtual(table) => table.columns.get(index),
            Self::FromClauseSubquery(from_clause_subquery) => {
                from_clause_subquery.columns.get(index)
            }
            Self::View(view) => view.columns.get(index),
        }
    }
    pub fn columns(&self) -> &Vec<Column> {
        match self {
            Self::BTree(table) => &table.columns,
            Self::Pseudo(table) => &table.columns,
            Self::Virtual(table) => &table.columns,
            Self::FromClauseSubquery(from_clause_subquery) => &from_clause_subquery.columns,
            Self::View(view) => &view.columns,
        }
    }
    pub fn btree(&self) -> Option<Rc<BTreeTable>> {
        match self {
            Self::BTree(table) => Some(table.clone()),
            Self::Pseudo(_) => None,
            Self::Virtual(_) => None,
            Self::FromClauseSubquery(_) => None,
            Self::View(_) => None,
        }
    }
    pub fn virtual_table(&self) -> Option<Rc<VirtualTable>> {
        match self {
            Self::Virtual(table) => Some(table.clone()),
            _ => None,
        }
    }
}
impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::BTree(a), Self::BTree(b)) => Rc::ptr_eq(a, b),
            (Self::Pseudo(a), Self::Pseudo(b)) => Rc::ptr_eq(a, b),
            (Self::Virtual(a), Self::Virtual(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}
#[derive(Debug, Eq)]
pub(super) struct UniqueColumnProps {
    pub(super) column_name: String,
    pub(super) order: SortOrder,
}
impl PartialEq for UniqueColumnProps {
    fn eq(&self, other: &Self) -> bool {
        self.column_name.eq(&other.column_name)
    }
}
impl PartialOrd for UniqueColumnProps {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for UniqueColumnProps {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.column_name.cmp(&other.column_name)
    }
}
/// Stringify a [`ResolveType`] into the canonical SQLite `ON CONFLICT` text form.
pub(super) fn resolve_type_to_str(resolve: ResolveType) -> &'static str {
    match resolve {
        ResolveType::Rollback => "ROLLBACK",
        ResolveType::Abort => "ABORT",
        ResolveType::Fail => "FAIL",
        ResolveType::Ignore => "IGNORE",
        ResolveType::Replace => "REPLACE",
    }
}
/// Stringify a [`limbo_sqlite3_parser::ast::RefAct`] into the canonical SQLite text form.
pub(super) fn ref_act_to_str(act: limbo_sqlite3_parser::ast::RefAct) -> &'static str {
    use limbo_sqlite3_parser::ast::RefAct;
    match act {
        RefAct::SetNull => "SET NULL",
        RefAct::SetDefault => "SET DEFAULT",
        RefAct::Cascade => "CASCADE",
        RefAct::Restrict => "RESTRICT",
        RefAct::NoAction => "NO ACTION",
    }
}
/// Build a [`ForeignKeyDef`] from a [`limbo_sqlite3_parser::ast::ForeignKeyClause`]
/// and the list of local (child) column names.
pub(super) fn build_fk_def(
    id: u32,
    from_cols: Vec<String>,
    clause: &limbo_sqlite3_parser::ast::ForeignKeyClause,
) -> ForeignKeyDef {
    use limbo_sqlite3_parser::ast::RefArg;
    let to_table = normalize_ident(&clause.tbl_name.0);
    let to_cols: Vec<String> = clause
        .columns
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|c| normalize_ident(&c.col_name.0))
        .collect();
    let mut on_update = "NO ACTION".to_string();
    let mut on_delete = "NO ACTION".to_string();
    let mut match_clause = "NONE".to_string();
    for arg in &clause.args {
        match arg {
            RefArg::OnUpdate(act) => on_update = ref_act_to_str(*act).to_string(),
            RefArg::OnDelete(act) => on_delete = ref_act_to_str(*act).to_string(),
            RefArg::Match(name) => match_clause = name.0.clone(),
            RefArg::OnInsert(_) => {}
        }
    }
    ForeignKeyDef {
        id,
        from_cols,
        to_table,
        to_cols,
        on_update,
        on_delete,
        match_clause,
    }
}
pub(super) fn create_table(
    tbl_name: QualifiedName,
    body: CreateTableBody,
    root_page: usize,
) -> Result<BTreeTable> {
    let table_name = normalize_ident(&tbl_name.name.0);
    trace!("Creating table {}", table_name);
    let mut has_rowid = true;
    let mut primary_key_columns = vec![];
    // The `ON CONFLICT <action>` declared on the table's PRIMARY KEY constraint,
    // whether it comes from a table-level `PRIMARY KEY(...)` clause or a
    // column-level `col ... PRIMARY KEY` clause. Only one PRIMARY KEY per table
    // is valid SQL, so the last constraint that specifies a conflict clause wins
    // (this function only ever re-parses trusted, previously-validated SQL, so
    // there is never more than one to begin with).
    let mut primary_key_conflict = ResolveType::Abort;
    let mut cols = vec![];
    let is_strict: bool;
    let mut unique_sets: Vec<(BTreeSet<UniqueColumnProps>, ResolveType)> = vec![];
    let mut raw_fks: Vec<(Vec<String>, limbo_sqlite3_parser::ast::ForeignKeyClause)> = vec![];
    match body {
        CreateTableBody::ColumnsAndConstraints {
            columns,
            constraints,
            options,
        } => {
            is_strict = options.contains(TableOptions::STRICT);
            if let Some(constraints) = constraints {
                for c in constraints {
                    match c.constraint {
                        limbo_sqlite3_parser::ast::TableConstraint::PrimaryKey {
                            columns,
                            conflict_clause,
                            ..
                        } => {
                            if let Some(resolve) = conflict_clause {
                                primary_key_conflict = resolve;
                            }
                            for column in columns {
                                let col_name = match column.expr {
                                    Expr::Id(id) => normalize_ident(&id.0),
                                    Expr::Literal(Literal::String(value)) => {
                                        value.trim_matches('\'').to_owned()
                                    }
                                    _ => {
                                        crate::bail_parse_error!(
                                            "expressions prohibited in PRIMARY KEY and UNIQUE constraints"
                                        );
                                    }
                                };
                                primary_key_columns
                                    .push((col_name, column.order.unwrap_or(SortOrder::Asc)));
                            }
                        }
                        limbo_sqlite3_parser::ast::TableConstraint::Unique {
                            columns,
                            conflict_clause,
                        } => {
                            let resolve = conflict_clause.unwrap_or(ResolveType::Abort);
                            let unique_set = columns
                                .into_iter()
                                .map(|column| {
                                    let column_name = match column.expr {
                                        Expr::Id(id) => normalize_ident(&id.0),
                                        _ => {
                                            crate::bail_parse_error!(
                                                "expressions prohibited in PRIMARY KEY and UNIQUE constraints"
                                            );
                                        }
                                    };
                                    Ok(UniqueColumnProps {
                                        column_name,
                                        order: column.order.unwrap_or(SortOrder::Asc),
                                    })
                                })
                                .collect::<Result<BTreeSet<_>>>()?;
                            unique_sets.push((unique_set, resolve));
                        }
                        limbo_sqlite3_parser::ast::TableConstraint::ForeignKey {
                            columns,
                            clause,
                            ..
                        } => {
                            let from_cols: Vec<String> = columns
                                .iter()
                                .map(|c| normalize_ident(&c.col_name.0))
                                .collect();
                            raw_fks.push((from_cols, clause));
                        }
                        limbo_sqlite3_parser::ast::TableConstraint::Check(_) => {}
                    }
                }
            }
            for (col_name, col_def) in columns {
                let name = col_name.0.to_string();
                let (ty, ty_str) = match col_def.col_type {
                    Some(data_type) => {
                        let s = data_type.name.as_str();
                        let ty_str = if matches!(
                            s.to_uppercase().as_str(),
                            "TEXT" | "INT" | "INTEGER" | "BLOB" | "REAL"
                        ) {
                            s.to_uppercase().to_string()
                        } else {
                            s.to_string()
                        };
                        let type_name = ty_str.to_uppercase();
                        if type_name.contains("INT") {
                            (Type::Integer, ty_str)
                        } else if type_name.contains("CHAR")
                            || type_name.contains("CLOB")
                            || type_name.contains("TEXT")
                        {
                            (Type::Text, ty_str)
                        } else if type_name.contains("BLOB") {
                            (Type::Blob, ty_str)
                        } else if type_name.is_empty() {
                            (Type::Blob, "".to_string())
                        } else if type_name.contains("REAL")
                            || type_name.contains("FLOA")
                            || type_name.contains("DOUB")
                        {
                            (Type::Real, ty_str)
                        } else {
                            (Type::Numeric, ty_str)
                        }
                    }
                    None => (Type::Null, "".to_string()),
                };
                let mut default = None;
                let mut primary_key = false;
                let mut notnull = false;
                let mut order = SortOrder::Asc;
                let mut unique = false;
                let mut unique_conflict = ResolveType::Abort;
                let mut collation = None;
                let mut is_generated = false;
                for c_def in &col_def.constraints {
                    match &c_def.constraint {
                        limbo_sqlite3_parser::ast::ColumnConstraint::PrimaryKey {
                            order: o,
                            conflict_clause,
                            ..
                        } => {
                            primary_key = true;
                            if let Some(o) = o {
                                order = o.clone();
                            }
                            if let Some(resolve) = conflict_clause {
                                primary_key_conflict = *resolve;
                            }
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::NotNull { .. } => {
                            notnull = true;
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::Default(expr) => {
                            default = Some(expr.clone());
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::Unique(on_conflict) => {
                            unique = true;
                            if let Some(resolve) = on_conflict {
                                unique_conflict = *resolve;
                            }
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::Collate { collation_name } => {
                            collation = Some(CollationSeq::new(collation_name.0.as_str())?);
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::ForeignKey {
                            clause, ..
                        } => {
                            raw_fks.push((vec![normalize_ident(&name)], clause.clone()));
                        }
                        limbo_sqlite3_parser::ast::ColumnConstraint::Generated { .. } => {
                            is_generated = true;
                        }
                        _ => {}
                    }
                }
                if primary_key {
                    primary_key_columns.push((normalize_ident(&name), order));
                }
                cols.push(Column {
                    name: Some(normalize_ident(&name)),
                    ty,
                    ty_str,
                    primary_key,
                    is_rowid_alias: false,
                    notnull,
                    default,
                    unique,
                    unique_conflict,
                    collation,
                    is_generated,
                });
            }
            if options.contains(TableOptions::WITHOUT_ROWID) {
                has_rowid = false;
            }
        }
        // `CREATE TABLE ... AS SELECT` is translated (see
        // `translate::schema::translate_create_table_as_select`) by synthesizing
        // an ordinary column-list `CREATE TABLE` statement from the SELECT's
        // result columns *before* it is ever persisted to `sqlite_schema` — the
        // literal `AS SELECT` text is never written to disk. Reaching this arm
        // therefore means the persisted schema is corrupt (or this function was
        // handed untrusted SQL directly).
        CreateTableBody::AsSelect(_) => {
            crate::bail_corrupt_error!(
                "malformed sqlite_schema entry: CREATE TABLE ... AS SELECT must not appear in \
                 persisted schema text for table '{}'",
                table_name
            );
        }
    };
    let single_int_rowid_alias = has_rowid && primary_key_columns.len() == 1;
    for col in cols.iter_mut() {
        let is_pk = col.name.as_ref().is_some_and(|name| {
            primary_key_columns
                .iter()
                .any(|(pk_name, _)| pk_name == name)
        });
        if is_pk {
            col.primary_key = true;
        }
        col.is_rowid_alias =
            single_int_rowid_alias && is_pk && col.ty == Type::Integer && col.ty_str == "INTEGER";
    }
    let foreign_keys: Vec<ForeignKeyDef> = raw_fks
        .into_iter()
        .enumerate()
        .map(|(idx, (from_cols, clause))| build_fk_def(idx as u32, from_cols, &clause))
        .collect();
    Ok(BTreeTable {
        root_page,
        // Schema rows are always parsed as if they belonged to `main`; a
        // non-`main` catalog is re-tagged wholesale after parsing (see
        // `multidb::retag_schema_db_index`).
        db_index: 0,
        name: table_name,
        has_rowid,
        primary_key_columns,
        columns: cols,
        is_strict,
        primary_key_conflict,
        unique_sets: if unique_sets.is_empty() {
            None
        } else {
            unique_sets.dedup();
            Some(
                unique_sets
                    .into_iter()
                    .map(|(set, resolve)| UniqueSet {
                        columns: set
                            .into_iter()
                            .map(|UniqueColumnProps { column_name, order }| (column_name, order))
                            .collect(),
                        resolve,
                    })
                    .collect(),
            )
        },
        foreign_keys,
    })
}
