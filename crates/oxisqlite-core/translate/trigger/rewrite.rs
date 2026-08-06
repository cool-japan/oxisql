//! Rewriting `OLD.<col>` / `NEW.<col>` references into [`ast::Expr::Register`].
//!
//! A trigger body is inlined into the program that fires it, so by the time the
//! body is translated the row images it refers to already live in VDBE
//! registers. Rewriting the references *before* translation means the ordinary
//! planner/optimizer/emitter path handles a trigger body exactly like any other
//! statement — no "am I inside a trigger?" flag has to be threaded through query
//! planning, and OLD/NEW work uniformly in `WHERE`, `SET`, `VALUES`, `HAVING`,
//! correlated subqueries and CTEs alike.
//!
//! Anything named `old.x` / `new.x` that the trigger's target table has no
//! column `x` for is a hard error (`no such column: NEW.x`), matching SQLite,
//! rather than being left to resolve against some unrelated table in scope.

use limbo_sqlite3_parser::ast;

use crate::schema::BTreeTable;
use crate::util::normalize_ident;
use crate::{LimboError, Result};

/// Which row image a qualifier refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowImageKind {
    Old,
    New,
}

impl RowImageKind {
    fn from_qualifier(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("old") {
            Some(RowImageKind::Old)
        } else if name.eq_ignore_ascii_case("new") {
            Some(RowImageKind::New)
        } else {
            None
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            RowImageKind::Old => "OLD",
            RowImageKind::New => "NEW",
        }
    }
}

/// Where a row image's values live in the register file.
#[derive(Clone, Copy, Debug)]
pub struct RowImage {
    /// Register holding the row's rowid.
    pub rowid_reg: usize,
    /// First register of the contiguous per-column block: column `i` of the
    /// table lives in `cols_start_reg + i`.
    pub cols_start_reg: usize,
}

/// The register bindings in scope while a trigger body is translated.
#[derive(Clone, Copy, Debug)]
pub struct TriggerRowBindings<'a> {
    /// The table the trigger is attached to; used to map column names to
    /// offsets and to produce SQLite-shaped "no such column" errors.
    pub table: &'a BTreeTable,
    /// `OLD.*` bindings — absent for INSERT triggers.
    pub old: Option<RowImage>,
    /// `NEW.*` bindings — absent for DELETE triggers.
    pub new: Option<RowImage>,
}

impl TriggerRowBindings<'_> {
    fn image(&self, kind: RowImageKind) -> Option<RowImage> {
        match kind {
            RowImageKind::Old => self.old,
            RowImageKind::New => self.new,
        }
    }

    /// Resolve `<kind>.<column>` to the register holding it.
    fn resolve(&self, kind: RowImageKind, column: &str) -> Result<usize> {
        let Some(image) = self.image(kind) else {
            // SQLite rejects NEW in a DELETE trigger and OLD in an INSERT
            // trigger at CREATE TRIGGER time with exactly this wording.
            return Err(LimboError::ParseError(format!(
                "no such column: {}.{}",
                kind.as_str(),
                column
            )));
        };
        let normalized = normalize_ident(column);
        // `rowid` / `oid` / `_rowid_` all name the rowid, unless the table
        // declares a real column with that name (SQLite's shadowing rule).
        if let Some(pos) =
            self.table.columns.iter().position(|c| {
                c.name.as_deref().map(normalize_ident).as_deref() == Some(&normalized)
            })
        {
            return Ok(image.cols_start_reg + pos);
        }
        if matches!(normalized.as_str(), "rowid" | "oid" | "_rowid_") {
            return Ok(image.rowid_reg);
        }
        Err(LimboError::ParseError(format!(
            "no such column: {}.{}",
            kind.as_str(),
            column
        )))
    }
}

/// Rewrite every `OLD.*` / `NEW.*` reference inside `expr` in place.
pub fn rewrite_expr(expr: &mut ast::Expr, bindings: &TriggerRowBindings<'_>) -> Result<()> {
    // Replace this node first: a `Qualified` node has no children worth
    // descending into once it has been turned into a `Register`.
    if let ast::Expr::Qualified(qualifier, column) = expr {
        if let Some(kind) = RowImageKind::from_qualifier(qualifier.0.as_str()) {
            let reg = bindings.resolve(kind, column.0.as_str())?;
            *expr = ast::Expr::Register(reg);
            return Ok(());
        }
    }
    if let ast::Expr::DoublyQualified(db, qualifier, column) = expr {
        // `main.new.x` is not valid SQLite either; only flag it when the middle
        // component actually names a row image, so unrelated three-part names
        // keep their normal resolution path.
        if RowImageKind::from_qualifier(qualifier.0.as_str()).is_some() {
            return Err(LimboError::ParseError(format!(
                "misuse of aliased row reference {}.{}.{}",
                db.0, qualifier.0, column.0
            )));
        }
    }

    match expr {
        ast::Expr::Between {
            lhs, start, end, ..
        } => {
            rewrite_expr(lhs, bindings)?;
            rewrite_expr(start, bindings)?;
            rewrite_expr(end, bindings)?;
        }
        ast::Expr::Binary(lhs, _, rhs) => {
            rewrite_expr(lhs, bindings)?;
            rewrite_expr(rhs, bindings)?;
        }
        ast::Expr::Case {
            base,
            when_then_pairs,
            else_expr,
        } => {
            if let Some(base) = base {
                rewrite_expr(base, bindings)?;
            }
            for (when_expr, then_expr) in when_then_pairs.iter_mut() {
                rewrite_expr(when_expr, bindings)?;
                rewrite_expr(then_expr, bindings)?;
            }
            if let Some(else_expr) = else_expr {
                rewrite_expr(else_expr, bindings)?;
            }
        }
        ast::Expr::Cast { expr, .. } => rewrite_expr(expr, bindings)?,
        ast::Expr::Collate(expr, _) => rewrite_expr(expr, bindings)?,
        ast::Expr::Exists(select) | ast::Expr::Subquery(select) => {
            rewrite_select(select, bindings)?
        }
        ast::Expr::FunctionCall {
            args,
            order_by,
            filter_over,
            ..
        } => {
            if let Some(args) = args {
                for arg in args.iter_mut() {
                    rewrite_expr(arg, bindings)?;
                }
            }
            if let Some(order_by) = order_by {
                for sc in order_by.iter_mut() {
                    rewrite_expr(&mut sc.expr, bindings)?;
                }
            }
            if let Some(tail) = filter_over {
                if let Some(filter) = tail.filter_clause.as_mut() {
                    rewrite_expr(filter, bindings)?;
                }
            }
        }
        ast::Expr::FunctionCallStar { filter_over, .. } => {
            if let Some(tail) = filter_over {
                if let Some(filter) = tail.filter_clause.as_mut() {
                    rewrite_expr(filter, bindings)?;
                }
            }
        }
        ast::Expr::InList { lhs, rhs, .. } => {
            rewrite_expr(lhs, bindings)?;
            if let Some(rhs) = rhs {
                for item in rhs.iter_mut() {
                    rewrite_expr(item, bindings)?;
                }
            }
        }
        ast::Expr::InSelect { lhs, rhs, .. } => {
            rewrite_expr(lhs, bindings)?;
            rewrite_select(rhs, bindings)?;
        }
        ast::Expr::InTable { lhs, args, .. } => {
            rewrite_expr(lhs, bindings)?;
            if let Some(args) = args {
                for arg in args.iter_mut() {
                    rewrite_expr(arg, bindings)?;
                }
            }
        }
        ast::Expr::IsNull(expr) | ast::Expr::NotNull(expr) => rewrite_expr(expr, bindings)?,
        ast::Expr::Like {
            lhs, rhs, escape, ..
        } => {
            rewrite_expr(lhs, bindings)?;
            rewrite_expr(rhs, bindings)?;
            if let Some(escape) = escape {
                rewrite_expr(escape, bindings)?;
            }
        }
        ast::Expr::Parenthesized(exprs) => {
            for expr in exprs.iter_mut() {
                rewrite_expr(expr, bindings)?;
            }
        }
        ast::Expr::Raise(_, message) => {
            if let Some(message) = message {
                rewrite_expr(message, bindings)?;
            }
        }
        ast::Expr::Unary(_, expr) => rewrite_expr(expr, bindings)?,
        // Leaves, and nodes whose contents cannot name a row image.
        ast::Expr::Column { .. }
        | ast::Expr::DoublyQualified(..)
        | ast::Expr::Id(_)
        | ast::Expr::Literal(_)
        | ast::Expr::Name(_)
        | ast::Expr::Qualified(..)
        | ast::Expr::Register(_)
        | ast::Expr::RowId { .. }
        | ast::Expr::Variable(_) => {}
    }
    Ok(())
}

/// Rewrite every `OLD.*` / `NEW.*` reference inside a `SELECT`.
pub fn rewrite_select(select: &mut ast::Select, bindings: &TriggerRowBindings<'_>) -> Result<()> {
    if let Some(with) = select.with.as_mut() {
        for cte in with.ctes.iter_mut() {
            rewrite_select(&mut cte.select, bindings)?;
        }
    }
    rewrite_one_select(&mut select.body.select, bindings)?;
    if let Some(compounds) = select.body.compounds.as_mut() {
        for compound in compounds.iter_mut() {
            rewrite_one_select(&mut compound.select, bindings)?;
        }
    }
    if let Some(order_by) = select.order_by.as_mut() {
        for sc in order_by.iter_mut() {
            rewrite_expr(&mut sc.expr, bindings)?;
        }
    }
    if let Some(limit) = select.limit.as_mut() {
        rewrite_expr(&mut limit.expr, bindings)?;
        if let Some(offset) = limit.offset.as_mut() {
            rewrite_expr(offset, bindings)?;
        }
    }
    Ok(())
}

fn rewrite_one_select(one: &mut ast::OneSelect, bindings: &TriggerRowBindings<'_>) -> Result<()> {
    match one {
        ast::OneSelect::Select(inner) => {
            for column in inner.columns.iter_mut() {
                if let ast::ResultColumn::Expr(expr, _) = column {
                    rewrite_expr(expr, bindings)?;
                }
            }
            if let Some(from) = inner.from.as_mut() {
                rewrite_from_clause(from, bindings)?;
            }
            if let Some(where_clause) = inner.where_clause.as_mut() {
                rewrite_expr(where_clause, bindings)?;
            }
            if let Some(group_by) = inner.group_by.as_mut() {
                for expr in group_by.exprs.iter_mut() {
                    rewrite_expr(expr, bindings)?;
                }
                if let Some(having) = group_by.having.as_mut() {
                    rewrite_expr(having, bindings)?;
                }
            }
        }
        ast::OneSelect::Values(rows) => {
            for row in rows.iter_mut() {
                for expr in row.iter_mut() {
                    rewrite_expr(expr, bindings)?;
                }
            }
        }
    }
    Ok(())
}

fn rewrite_from_clause(
    from: &mut ast::FromClause,
    bindings: &TriggerRowBindings<'_>,
) -> Result<()> {
    if let Some(table) = from.select.as_mut() {
        rewrite_select_table(table, bindings)?;
    }
    if let Some(joins) = from.joins.as_mut() {
        for join in joins.iter_mut() {
            rewrite_select_table(&mut join.table, bindings)?;
            if let Some(ast::JoinConstraint::On(expr)) = join.constraint.as_mut() {
                rewrite_expr(expr, bindings)?;
            }
        }
    }
    Ok(())
}

fn rewrite_select_table(
    table: &mut ast::SelectTable,
    bindings: &TriggerRowBindings<'_>,
) -> Result<()> {
    match table {
        ast::SelectTable::Table(..) => {}
        ast::SelectTable::TableCall(_, args, _) => {
            if let Some(args) = args {
                for arg in args.iter_mut() {
                    rewrite_expr(arg, bindings)?;
                }
            }
        }
        ast::SelectTable::Select(select, _) => rewrite_select(select, bindings)?,
        ast::SelectTable::Sub(from, _) => rewrite_from_clause(from, bindings)?,
    }
    Ok(())
}

fn rewrite_upsert(upsert: &mut ast::Upsert, bindings: &TriggerRowBindings<'_>) -> Result<()> {
    if let Some(index) = upsert.index.as_mut() {
        for target in index.targets.iter_mut() {
            rewrite_expr(&mut target.expr, bindings)?;
        }
        if let Some(where_clause) = index.where_clause.as_mut() {
            rewrite_expr(where_clause, bindings)?;
        }
    }
    if let ast::UpsertDo::Set { sets, where_clause } = upsert.do_clause.as_mut() {
        for set in sets.iter_mut() {
            rewrite_expr(&mut set.expr, bindings)?;
        }
        if let Some(where_clause) = where_clause.as_mut() {
            rewrite_expr(where_clause, bindings)?;
        }
    }
    if let Some(next) = upsert.next.as_mut() {
        rewrite_upsert(next, bindings)?;
    }
    Ok(())
}

/// Turn one trigger body command into a standalone statement whose `OLD.*` /
/// `NEW.*` references have been replaced by register reads.
///
/// The returned statement is translated inline (nested) into the firing
/// program, so it must be self-contained.
pub fn trigger_cmd_to_stmt(
    cmd: &ast::TriggerCmd,
    bindings: &TriggerRowBindings<'_>,
) -> Result<ast::Stmt> {
    match cmd {
        ast::TriggerCmd::Select(select) => {
            let mut select = select.clone();
            rewrite_select(&mut select, bindings)?;
            Ok(ast::Stmt::Select(select))
        }
        ast::TriggerCmd::Delete(delete) => {
            let mut where_clause = delete.where_clause.clone();
            if let Some(where_clause) = where_clause.as_mut() {
                rewrite_expr(where_clause, bindings)?;
            }
            Ok(ast::Stmt::Delete(Box::new(ast::Delete {
                with: None,
                tbl_name: ast::QualifiedName::single(delete.tbl_name.clone()),
                indexed: None,
                where_clause: where_clause.map(Box::new),
                returning: None,
                order_by: None,
                limit: None,
            })))
        }
        ast::TriggerCmd::Insert(insert) => {
            let mut select = insert.select.clone();
            rewrite_select(&mut select, bindings)?;
            let mut upsert = insert.upsert.clone();
            if let Some(upsert) = upsert.as_mut() {
                rewrite_upsert(upsert, bindings)?;
            }
            Ok(ast::Stmt::Insert(Box::new(ast::Insert {
                with: None,
                or_conflict: insert.or_conflict,
                tbl_name: ast::QualifiedName::single(insert.tbl_name.clone()),
                columns: insert.col_names.clone(),
                body: ast::InsertBody::Select(select, upsert),
                returning: None,
            })))
        }
        ast::TriggerCmd::Update(update) => {
            let mut sets = update.sets.clone();
            for set in sets.iter_mut() {
                rewrite_expr(&mut set.expr, bindings)?;
            }
            let mut from = update.from.clone();
            if let Some(from) = from.as_mut() {
                rewrite_from_clause(from, bindings)?;
            }
            let mut where_clause = update.where_clause.clone();
            if let Some(where_clause) = where_clause.as_mut() {
                rewrite_expr(where_clause, bindings)?;
            }
            Ok(ast::Stmt::Update(Box::new(ast::Update {
                with: None,
                or_conflict: update.or_conflict,
                tbl_name: ast::QualifiedName::single(update.tbl_name.clone()),
                indexed: None,
                sets,
                from,
                where_clause: where_clause.map(Box::new),
                returning: None,
                order_by: None,
                limit: None,
            })))
        }
    }
}

/// Rewrite a trigger's `WHEN` guard.
pub fn rewrite_when_clause(
    when_clause: &ast::Expr,
    bindings: &TriggerRowBindings<'_>,
) -> Result<ast::Expr> {
    let mut when_clause = when_clause.clone();
    rewrite_expr(&mut when_clause, bindings)?;
    Ok(when_clause)
}
