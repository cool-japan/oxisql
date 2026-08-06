//! In-memory catalog entry for a `CREATE TRIGGER` statement.
//!
//! A trigger, like a view, owns no B-tree: it is persisted as a single
//! `type='trigger'` row in `sqlite_schema` (`rootpage = 0`, `tbl_name` = the
//! table it watches) and reconstructed by re-parsing that row's SQL text on
//! every schema load. The parsed [`ast::CreateTrigger`] body is kept verbatim so
//! the code generator can inline it at each write site — see
//! `crate::translate::trigger`.

use std::sync::Arc;

use fallible_iterator::FallibleIterator;
use limbo_sqlite3_parser::ast::{self, Cmd, Stmt, TriggerEvent, TriggerTime};
use limbo_sqlite3_parser::lexer::sql::Parser;

use crate::util::normalize_ident;
use crate::Result;

/// Which write operation a trigger watches, after `UPDATE OF (a, b)` has been
/// separated from a bare `UPDATE`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerOp {
    /// `INSERT`
    Insert,
    /// `DELETE`
    Delete,
    /// `UPDATE` — `Some(cols)` for `UPDATE OF a, b` (normalized, lowercased),
    /// `None` for a bare `UPDATE` (fires for any column).
    Update(Option<Vec<String>>),
}

impl TriggerOp {
    /// Whether this trigger op matches an `UPDATE` that assigns `changed_columns`
    /// (normalized names). A bare `UPDATE` trigger matches any assignment list;
    /// an `UPDATE OF` trigger matches only when at least one of its named
    /// columns is assigned, exactly like SQLite's `checkColumnOverlap()`.
    pub fn matches_update_of(&self, changed_columns: &[String]) -> bool {
        match self {
            TriggerOp::Update(None) => true,
            TriggerOp::Update(Some(cols)) => cols
                .iter()
                .any(|c| changed_columns.iter().any(|changed| changed == c)),
            _ => false,
        }
    }
}

/// A parsed, validated trigger from `sqlite_schema`.
#[derive(Clone, Debug)]
pub struct Trigger {
    /// Trigger name, normalized (unquoted).
    pub name: String,
    /// Name of the table (or view) the trigger is attached to, normalized.
    pub tbl_name: String,
    /// `BEFORE` / `AFTER` / `INSTEAD OF`. A trigger declared without an explicit
    /// time defaults to `BEFORE`, matching SQLite.
    pub time: TriggerTime,
    /// The write operation watched.
    pub op: TriggerOp,
    /// Optional `WHEN <expr>` guard, evaluated per row with `OLD`/`NEW` in scope.
    pub when_clause: Option<ast::Expr>,
    /// Body statements, in order.
    pub commands: Vec<ast::TriggerCmd>,
    /// The exact SQL text persisted in `sqlite_schema.sql`.
    pub sql: String,
}

impl Trigger {
    /// Rebuild a trigger from its persisted `sqlite_schema.sql` text.
    ///
    /// The only caller is the schema loader, which feeds back text this engine
    /// itself wrote, so anything that does not re-parse as a `CREATE TRIGGER`
    /// means the persisted schema is corrupt.
    pub fn from_sql(sql: &str) -> Result<Self> {
        let mut parser = Parser::new(sql.as_bytes());
        match parser.next()? {
            Some(Cmd::Stmt(Stmt::CreateTrigger(create))) => Self::from_ast(&create, sql),
            _ => crate::bail_corrupt_error!(
                "malformed sqlite_schema entry: expected a CREATE TRIGGER statement, got: {}",
                sql
            ),
        }
    }

    /// Build the catalog entry from a freshly parsed `CREATE TRIGGER` AST.
    pub fn from_ast(create: &ast::CreateTrigger, sql: &str) -> Result<Self> {
        let op = match &create.event {
            TriggerEvent::Insert => TriggerOp::Insert,
            TriggerEvent::Delete => TriggerOp::Delete,
            TriggerEvent::Update => TriggerOp::Update(None),
            TriggerEvent::UpdateOf(names) => TriggerOp::Update(Some(
                names
                    .iter()
                    .map(|n| normalize_ident(n.0.as_str()))
                    .collect(),
            )),
        };
        Ok(Self {
            name: normalize_ident(create.trigger_name.name.0.as_str()),
            tbl_name: normalize_ident(create.tbl_name.name.0.as_str()),
            // SQLite's grammar makes the time optional and defaults it to BEFORE.
            time: create.time.unwrap_or(TriggerTime::Before),
            op,
            when_clause: create.when_clause.clone(),
            commands: create.commands.clone(),
            sql: sql.to_string(),
        })
    }
}

/// A trigger renders as the exact SQL text persisted in `sqlite_schema.sql`,
/// which is also what `SELECT sql FROM sqlite_master WHERE type='trigger'`
/// returns.
impl std::fmt::Display for Trigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sql)
    }
}

/// A reference-counted trigger as stored in the catalog.
pub type TriggerRef = Arc<Trigger>;
