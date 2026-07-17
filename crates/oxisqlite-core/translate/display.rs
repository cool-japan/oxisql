use core::fmt;
use std::fmt::{Display, Formatter};

use limbo_sqlite3_parser::{
    ast::{SortOrder, TableInternalId},
    to_sql_string::{ToSqlContext, ToSqlString},
};

use crate::{schema::Table, translate::plan::TableReferences, util::normalize_ident};

use super::plan::{
    Aggregate, DeletePlan, JoinedTable, Operation, Plan, Search, SelectPlan, UpdatePlan,
};

impl Display for Aggregate {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        let args_str = self
            .args
            .iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<String>>()
            .join(", ");
        write!(f, "{:?}({})", self.func, args_str)
    }
}

/// For EXPLAIN QUERY PLAN
impl Display for Plan {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Select(select_plan) => select_plan.fmt(f),
            Self::CompoundSelect {
                left,
                right_most,
                limit,
                offset,
                order_by,
                ..
            } => {
                for (plan, operator) in left {
                    plan.fmt(f)?;
                    writeln!(f, "{}", operator)?;
                }
                right_most.fmt(f)?;
                if let Some(limit) = limit {
                    writeln!(f, "LIMIT: {}", limit)?;
                }
                if let Some(offset) = offset {
                    writeln!(f, "OFFSET: {}", offset)?;
                }
                if let Some(order_by) = order_by {
                    writeln!(f, "ORDER BY:")?;
                    for (expr, dir) in order_by {
                        writeln!(
                            f,
                            "  - {} {}",
                            expr,
                            if *dir == SortOrder::Asc {
                                "ASC"
                            } else {
                                "DESC"
                            }
                        )?;
                    }
                }
                Ok(())
            }
            Self::Delete(delete_plan) => delete_plan.fmt(f),
            Self::Update(update_plan) => update_plan.fmt(f),
        }
    }
}

impl Display for SelectPlan {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        writeln!(f, "QUERY PLAN")?;

        // Print each table reference with appropriate indentation based on join depth
        for (i, reference) in self.table_references.joined_tables().iter().enumerate() {
            let is_last = i == self.table_references.joined_tables().len() - 1;
            let indent = if i == 0 {
                if is_last { "`--" } else { "|--" }.to_string()
            } else {
                format!(
                    "   {}{}",
                    "|  ".repeat(i - 1),
                    if is_last { "`--" } else { "|--" }
                )
            };

            match &reference.op {
                Operation::Scan { .. } => {
                    let table_name = if reference.table.get_name() == reference.identifier {
                        reference.identifier.clone()
                    } else {
                        format!("{} AS {}", reference.table.get_name(), reference.identifier)
                    };

                    writeln!(f, "{}SCAN {}", indent, table_name)?;
                }
                Operation::Search(search) => match search {
                    Search::RowidEq { .. } | Search::Seek { index: None, .. } => {
                        writeln!(
                            f,
                            "{}SEARCH {} USING INTEGER PRIMARY KEY (rowid=?)",
                            indent, reference.identifier
                        )?;
                    }
                    Search::Seek {
                        index: Some(index), ..
                    } => {
                        writeln!(
                            f,
                            "{}SEARCH {} USING INDEX {}",
                            indent, reference.identifier, index.name
                        )?;
                    }
                },
            }
        }
        Ok(())
    }
}

impl Display for DeletePlan {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        writeln!(f, "QUERY PLAN")?;

        // Delete plan should only have one table reference
        if let Some(reference) = self.table_references.joined_tables().first() {
            let indent = "`--";

            match &reference.op {
                Operation::Scan { .. } => {
                    let table_name = if reference.table.get_name() == reference.identifier {
                        reference.identifier.clone()
                    } else {
                        format!("{} AS {}", reference.table.get_name(), reference.identifier)
                    };

                    writeln!(f, "{}DELETE FROM {}", indent, table_name)?;
                }
                Operation::Search(search) => match search {
                    Search::RowidEq { .. } | Search::Seek { index: None, .. } => {
                        writeln!(
                            f,
                            "{}DELETE FROM {} USING INTEGER PRIMARY KEY (rowid=?)",
                            indent, reference.identifier
                        )?;
                    }
                    Search::Seek {
                        index: Some(index), ..
                    } => {
                        writeln!(
                            f,
                            "{}DELETE FROM {} USING INDEX {}",
                            indent, reference.identifier, index.name
                        )?;
                    }
                },
            }
        }
        Ok(())
    }
}

impl fmt::Display for UpdatePlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "QUERY PLAN")?;

        for (i, reference) in self.table_references.joined_tables().iter().enumerate() {
            let is_last = i == self.table_references.joined_tables().len() - 1;
            let indent = if i == 0 {
                if is_last { "`--" } else { "|--" }.to_string()
            } else {
                format!(
                    "   {}{}",
                    "|  ".repeat(i - 1),
                    if is_last { "`--" } else { "|--" }
                )
            };

            match &reference.op {
                Operation::Scan { .. } => {
                    let table_name = if reference.table.get_name() == reference.identifier {
                        reference.identifier.clone()
                    } else {
                        format!("{} AS {}", reference.table.get_name(), reference.identifier)
                    };

                    if i == 0 {
                        writeln!(f, "{}UPDATE {}", indent, table_name)?;
                    } else {
                        writeln!(f, "{}SCAN {}", indent, table_name)?;
                    }
                }
                Operation::Search(search) => match search {
                    Search::RowidEq { .. } | Search::Seek { index: None, .. } => {
                        writeln!(
                            f,
                            "{}SEARCH {} USING INTEGER PRIMARY KEY (rowid=?)",
                            indent, reference.identifier
                        )?;
                    }
                    Search::Seek {
                        index: Some(index), ..
                    } => {
                        writeln!(
                            f,
                            "{}SEARCH {} USING INDEX {}",
                            indent, reference.identifier, index.name
                        )?;
                    }
                },
            }
        }
        if let Some(order_by) = &self.order_by {
            writeln!(f, "ORDER BY:")?;
            for (expr, dir) in order_by {
                writeln!(
                    f,
                    "  - {} {}",
                    expr,
                    if *dir == SortOrder::Asc {
                        "ASC"
                    } else {
                        "DESC"
                    }
                )?;
            }
        }
        if let Some(limit) = self.limit {
            writeln!(f, "LIMIT: {}", limit)?;
        }
        if let Some(ret) = &self.returning {
            writeln!(f, "RETURNING:")?;
            for col in ret {
                writeln!(f, "  - {}", col.expr)?;
            }
        }

        Ok(())
    }
}

pub struct PlanContext<'a>(pub &'a [&'a TableReferences]);

// Definitely not perfect yet
impl ToSqlContext for PlanContext<'_> {
    fn get_column_name(&self, table_id: TableInternalId, col_idx: usize) -> &str {
        let table = self
            .0
            .iter()
            .map(|table_ref| table_ref.find_table_by_internal_id(table_id))
            .reduce(|accum, curr| match (accum, curr) {
                (Some(table), _) | (_, Some(table)) => Some(table),
                _ => None,
            })
            .flatten();
        let Some(table) = table else {
            return "";
        };
        let cols = table.columns();
        cols.get(col_idx)
            .and_then(|col| col.name.as_deref())
            .unwrap_or("")
    }

    fn get_table_name(&self, id: TableInternalId) -> &str {
        let table_ref = self
            .0
            .iter()
            .find(|table_ref| table_ref.find_table_by_internal_id(id).is_some());
        let Some(table_ref) = table_ref else {
            return "";
        };
        let joined_table = table_ref.find_joined_table_by_internal_id(id);
        let outer_query = table_ref.find_outer_query_ref_by_internal_id(id);
        match (joined_table, outer_query) {
            (Some(table), None) => &table.identifier,
            (None, Some(table)) => &table.identifier,
            _ => "",
        }
    }
}

impl ToSqlString for Plan {
    fn to_sql_string<C: ToSqlContext>(&self, context: &C) -> String {
        // Make the Plans pass their own context
        match self {
            Self::Select(select) => select.to_sql_string(&PlanContext(&[&select.table_references])),
            Self::CompoundSelect {
                left,
                right_most,
                limit,
                offset,
                order_by,
                ..
            } => {
                let all_refs = left
                    .iter()
                    .flat_map(|(plan, _)| std::iter::once(&plan.table_references))
                    .chain(std::iter::once(&right_most.table_references))
                    .collect::<Vec<_>>();
                let context = &PlanContext(all_refs.as_slice());

                let mut ret = Vec::new();
                for (plan, operator) in left {
                    ret.push(format!("{} {}", plan.to_sql_string(context), operator));
                }
                ret.push(right_most.to_sql_string(context));
                if let Some(order_by) = &order_by {
                    ret.push(format!(
                        "ORDER BY {}",
                        order_by
                            .iter()
                            .map(|(expr, order)| format!(
                                "{} {}",
                                expr.to_sql_string(context),
                                order
                            ))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                if let Some(limit) = &limit {
                    ret.push(format!("LIMIT {}", limit));
                }
                if let Some(offset) = &offset {
                    ret.push(format!("OFFSET {}", offset));
                }
                ret.join(" ")
            }
            Self::Delete(delete) => delete.to_sql_string(context),
            Self::Update(update) => update.to_sql_string(context),
        }
    }
}

impl ToSqlString for JoinedTable {
    fn to_sql_string<C: limbo_sqlite3_parser::to_sql_string::ToSqlContext>(
        &self,
        _context: &C,
    ) -> String {
        let table_or_subquery = match &self.table {
            Table::BTree(..) | Table::Pseudo(..) | Table::Virtual(..) | Table::View(..) => {
                self.table.get_name().to_string()
            }
            Table::FromClauseSubquery(from_clause_subquery) => {
                // `Plan`'s own `ToSqlString` builds its per-arm context, so the
                // outer context is passed through unused.
                format!("({})", from_clause_subquery.plan.to_sql_string(_context))
            }
        };
        // JOIN is done at a higher level
        format!(
            "{}{}",
            table_or_subquery,
            if self.identifier != table_or_subquery {
                format!(" AS {}", self.identifier)
            } else {
                "".to_string()
            }
        )
    }
}

impl ToSqlString for SelectPlan {
    fn to_sql_string<C: limbo_sqlite3_parser::to_sql_string::ToSqlContext>(
        &self,
        context: &C,
    ) -> String {
        let mut ret = Vec::new();
        // Re-emit the original `WITH ... AS (...)` syntax, if this SELECT had
        // one. `self.with` retains the pre-desugaring CTE AST specifically so
        // this is possible (see `SelectPlan::with`) — by the time
        // `table_references`/`join_order` are built below, every CTE has
        // already been desugared into an ordinary `FromClauseSubquery`
        // wherever it's referenced, indistinguishable from a hand-written
        // inline subquery. `ast::With`'s own `ToSqlString` impl (in
        // `limbo_sqlite3_parser::to_sql_string`) walks the untranslated AST,
        // so nested/chained CTEs (a CTE referencing an earlier CTE, or a CTE
        // with its own nested WITH) are rendered correctly for free, and
        // recursively for FROM-clause subqueries that themselves have a
        // WITH clause, since this whole function runs again for those.
        if let Some(with) = &self.with {
            ret.push(with.to_sql_string(context));
        }
        // VALUES SELECT statement
        if !self.values.is_empty() {
            ret.push(format!(
                "VALUES {}",
                self.values
                    .iter()
                    .map(|value| {
                        let joined_value = value
                            .iter()
                            .map(|e| e.to_sql_string(context))
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("({})", joined_value)
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        } else {
            // standard SELECT statement
            ret.push("SELECT".to_string());
            if self.distinctness.is_distinct() {
                ret.push("DISTINCT".to_string());
            }
            ret.push(
                self.result_columns
                    .iter()
                    .map(|cols| {
                        format!(
                            "{}{}",
                            cols.expr.to_sql_string(context),
                            cols.alias
                                .as_ref()
                                .map_or("".to_string(), |alias| format!(" AS {}", alias))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            ret.push("FROM".to_string());

            // A joined table whose identifier names one of `self.with`'s
            // CTEs came from the CTE desugaring in
            // `translate::planner::parse_from` (it reuses the CTE's own
            // `JoinedTable`, identifier and all, whenever it's referenced
            // without an alias). Reference it by name here instead of
            // re-inlining its subquery body: the `WITH` clause pushed above
            // already carries the full definition, so printing the inline
            // `(SELECT ...) AS name` form here too would just be a
            // redundant, less faithful reconstruction of the original
            // CTE-based query.
            let is_cte_reference = |identifier: &str| -> bool {
                self.with.as_ref().is_some_and(|with| {
                    with.ctes
                        .iter()
                        .any(|cte| normalize_ident(&cte.tbl_name.0) == identifier)
                })
            };

            ret.extend(
                self.join_order
                    .iter()
                    .enumerate()
                    .filter_map(|(idx, order)| {
                        let table_ref = self.joined_tables().get(order.original_idx)?;
                        let rendered = if is_cte_reference(&table_ref.identifier) {
                            table_ref.identifier.clone()
                        } else {
                            table_ref.to_sql_string(context)
                        };
                        if idx == 0 {
                            Some(rendered)
                        } else {
                            Some(format!(
                                "{}JOIN {}",
                                if order.is_outer { "OUTER " } else { "" },
                                rendered
                            ))
                        }
                    }),
            );
            if !self.where_clause.is_empty() {
                ret.push("WHERE".to_string());
                ret.push(
                    self.where_clause
                        .iter()
                        .map(|where_clause| where_clause.expr.to_sql_string(context))
                        .collect::<Vec<_>>()
                        .join(" AND "),
                );
            }
            if let Some(group_by) = &self.group_by {
                // Expressions here go through the same `PlanContext` as every
                // other expression in this plan (WHERE, result columns, ...),
                // so a GROUP BY expression that referenced a result-column
                // ordinal (`GROUP BY 1`) or alias is printed correctly: both
                // are resolved into an ordinary expression tree (a clone of
                // the referenced result column's already-bound expression)
                // by `translate::select::prepare_one_select_plan` before this
                // plan is ever built — see
                // `replace_column_number_with_copy_of_column_expr` and the
                // `bind_column_references(..., Some(&plan.result_columns))`
                // call right after it. By the time we get here there is
                // nothing left that's special-cased about GROUP BY exprs.
                ret.push("GROUP BY".to_string());
                ret.push(
                    match &group_by.sort_order {
                        // A sorter is required: `sort_order[i]` is the real
                        // direction the GROUP BY sorter uses for
                        // `exprs[i]`, mirroring how ORDER BY renders
                        // `SortOrder` above.
                        Some(sort_order) => group_by
                            .exprs
                            .iter()
                            .zip(sort_order.iter())
                            .map(|(expr, order)| {
                                format!("{} {}", expr.to_sql_string(context), order)
                            })
                            .collect::<Vec<_>>(),
                        // No sorter needed (rows already arrive in group
                        // order via the chosen scan/index) — there is no
                        // per-expression direction to report, so print the
                        // bare expressions as before.
                        None => group_by
                            .exprs
                            .iter()
                            .map(|expr| expr.to_sql_string(context))
                            .collect::<Vec<_>>(),
                    }
                    .join(", "),
                );
                if let Some(having) = &group_by.having {
                    ret.push("HAVING".to_string());
                    ret.push(
                        having
                            .iter()
                            .map(|expr| expr.to_sql_string(context))
                            .collect::<Vec<_>>()
                            .join(" AND "),
                    );
                }
            }
        }
        if let Some(order_by) = &self.order_by {
            ret.push(format!(
                "ORDER BY {}",
                order_by
                    .iter()
                    .map(|(expr, order)| format!("{} {}", expr.to_sql_string(context), order))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(limit) = &self.limit {
            ret.push(format!("LIMIT {}", limit));
        }
        if let Some(offset) = &self.offset {
            ret.push(format!("OFFSET {}", offset));
        }
        ret.join(" ")
    }
}

impl ToSqlString for DeletePlan {
    fn to_sql_string<C: ToSqlContext>(&self, _context: &C) -> String {
        let table = self
            .table_references
            .joined_tables()
            .first()
            .expect("Delete Plan should have only one table reference");
        let context = &[&self.table_references];
        let context = &PlanContext(context);
        let mut ret = Vec::new();

        ret.push(format!("DELETE FROM {}", table.table.get_name()));

        if !self.where_clause.is_empty() {
            ret.push("WHERE".to_string());
            ret.push(
                self.where_clause
                    .iter()
                    .map(|where_clause| where_clause.expr.to_sql_string(context))
                    .collect::<Vec<_>>()
                    .join(" AND "),
            );
        }
        if let Some(order_by) = &self.order_by {
            ret.push(format!(
                "ORDER BY {}",
                order_by
                    .iter()
                    .map(|(expr, order)| format!("{} {}", expr.to_sql_string(context), order))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(limit) = &self.limit {
            ret.push(format!("LIMIT {}", limit));
        }
        if let Some(offset) = &self.offset {
            ret.push(format!("OFFSET {}", offset));
        }
        ret.join(" ")
    }
}

impl ToSqlString for UpdatePlan {
    fn to_sql_string<C: ToSqlContext>(&self, _context: &C) -> String {
        let table = self
            .table_references
            .joined_tables()
            .first()
            .expect("UPDATE Plan should have only one table reference");
        let context = [&self.table_references];
        let context = &PlanContext(&context);
        let mut ret = Vec::new();

        // `UPDATE OR <action> ...` — mirrors how `INSERT OR <action>` prints
        // (see `translate::insert`); `self.or_conflict` is already populated
        // by the real UPDATE emitter (`translate::update::prepare_update_plan`)
        // from `Update.or_conflict`.
        ret.push(format!(
            "UPDATE{} {} SET",
            self.or_conflict
                .map_or(String::new(), |action| format!(" OR {}", action)),
            table.table.get_name()
        ));

        // NOTE: this only ever prints `col = expr`, never the SQLite
        // `UPDATE t (a, b) = (x, y)` column-list form, because
        // `translate::update::prepare_update_plan` doesn't support parsing
        // that form in the first place: it reads only
        // `set.col_names[0]` for each `Set` and silently discards any
        // further names (see `update.rs`), so `UpdatePlan.set_clauses` can
        // only ever represent one `(column, expr)` pair per SET item. This
        // is therefore a real planner-level limitation, not just a display
        // gap — fixing it here would require the planner to support the
        // column-list form first, which is out of scope for this
        // pretty-printer.
        ret.push(
            self.set_clauses
                .iter()
                .map(|(col_idx, set_expr)| {
                    let col_name = table
                        .table
                        .get_column_at(*col_idx)
                        .as_ref()
                        .and_then(|col| col.name.as_deref())
                        .unwrap_or("");
                    format!("{} = {}", col_name, set_expr.to_sql_string(context))
                })
                .collect::<Vec<_>>()
                .join(", "),
        );

        if !self.where_clause.is_empty() {
            ret.push("WHERE".to_string());
            ret.push(
                self.where_clause
                    .iter()
                    .map(|where_clause| where_clause.expr.to_sql_string(context))
                    .collect::<Vec<_>>()
                    .join(" AND "),
            );
        }
        if let Some(order_by) = &self.order_by {
            ret.push(format!(
                "ORDER BY {}",
                order_by
                    .iter()
                    .map(|(expr, order)| format!("{} {}", expr.to_sql_string(context), order))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Some(limit) = &self.limit {
            ret.push(format!("LIMIT {}", limit));
        }
        if let Some(offset) = &self.offset {
            ret.push(format!("OFFSET {}", offset));
        }
        ret.join(" ")
    }
}

#[cfg(test)]
mod tests {
    //! These exercise the debug pretty-printer (`ToSqlString`) through the
    //! real parse-and-plan pipeline (`prepare_select_plan` / `prepare_update_plan`
    //! against a real, `CREATE TABLE`-populated `Schema`) rather than
    //! hand-constructing `SelectPlan`/`UpdatePlan` literals, mirroring the
    //! proven pattern in `translate::optimizer::dml_safety`'s own tests. This
    //! keeps the tests decoupled from the internal shape of plan-building
    //! helper types and exercises the exact same code the real optimizer
    //! debug-logging call site (`translate::optimizer::optimize_plan`) does.
    use super::*;
    use crate::translate::plan::QueryDestination;
    use crate::translate::select::prepare_select_plan;
    use crate::translate::update::prepare_update_plan;
    use crate::vdbe::builder::TableRefIdCounter;
    use crate::{Connection, Database, MemoryIO, StepResult, IO};
    use fallible_iterator::FallibleIterator;
    use limbo_sqlite3_parser::{ast, lexer::sql::Parser};
    use std::sync::Arc;

    fn new_conn() -> (Arc<dyn IO>, Arc<Connection>) {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db = Database::open_file(io.clone(), ":memory:", false).expect("open in-memory db");
        let conn = db.connect().expect("connect");
        (io, conn)
    }

    fn exec(io: &Arc<dyn IO>, conn: &Arc<Connection>, sql: &str) {
        let mut stmt = conn
            .prepare(sql)
            .unwrap_or_else(|e| panic!("prepare {sql:?}: {e:?}"));
        loop {
            match stmt
                .step()
                .unwrap_or_else(|e| panic!("step {sql:?}: {e:?}"))
            {
                StepResult::Done => return,
                StepResult::IO | StepResult::Busy => io.run_once().expect("io run_once"),
                StepResult::Row => {}
                StepResult::Interrupt => panic!("interrupted"),
            }
        }
    }

    fn parse_one(sql: &str) -> ast::Stmt {
        let mut parser = Parser::new(sql.as_bytes());
        match parser
            .next()
            .expect("parse")
            .expect("nonempty input yields a command")
        {
            ast::Cmd::Stmt(stmt) => stmt,
            other => panic!("expected Cmd::Stmt, got {other:?}"),
        }
    }

    /// Parses `sql` as a `SELECT` and runs it through `prepare_select_plan`
    /// against `conn`'s current schema, WITHOUT running the optimizer --
    /// `GroupBy::sort_order` is deterministically `Some(all ASC)` at this
    /// stage (see `translate::select::prepare_one_select_plan`) and is only
    /// ever narrowed to `None` or re-ordered by `optimize_plan`, so skipping
    /// it keeps the sort-order assertions below stable and independent of
    /// optimizer/index internals.
    fn select_plan(conn: &Arc<Connection>, sql: &str) -> SelectPlan {
        let ast::Stmt::Select(select) = parse_one(sql) else {
            panic!("expected a SELECT statement: {sql:?}");
        };
        let schema = conn.schema.read();
        let syms = conn.syms.borrow();
        let mut counter = TableRefIdCounter::new();
        let plan = prepare_select_plan(
            &schema,
            *select,
            &syms,
            &[],
            &mut counter,
            QueryDestination::ResultRows,
        )
        .expect("prepare_select_plan");
        let Plan::Select(select_plan) = plan else {
            panic!("prepare_select_plan did not return Plan::Select");
        };
        select_plan
    }

    /// Parses `sql` as an `UPDATE` and runs it through `prepare_update_plan`
    /// against `conn`'s current schema (this is the "real UPDATE emitter"
    /// that populates `UpdatePlan::or_conflict`).
    fn update_plan(conn: &Arc<Connection>, sql: &str) -> UpdatePlan {
        let ast::Stmt::Update(update) = parse_one(sql) else {
            panic!("expected an UPDATE statement: {sql:?}");
        };
        let mut update = *update;
        let schema = conn.schema.read();
        let mut counter = TableRefIdCounter::new();
        let plan =
            prepare_update_plan(&schema, &mut update, &mut counter).expect("prepare_update_plan");
        let Plan::Update(update_plan) = plan else {
            panic!("prepare_update_plan did not return Plan::Update");
        };
        update_plan
    }

    /// Item 1: a `WITH cte AS (...)` query must re-emit the original CTE
    /// syntax, not an inlined `FROM (SELECT ...) AS cte` subquery.
    #[test]
    fn cte_re_renders_as_with_clause_not_inline_subquery() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = select_plan(&conn, "WITH cte AS (SELECT id, v FROM t) SELECT v FROM cte");
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.contains("WITH cte AS ("),
            "expected the original WITH ... AS ( syntax, got: {sql}"
        );
        assert!(
            sql.contains("FROM cte"),
            "expected the CTE to be referenced by name instead of being \
             re-inlined as a subquery, got: {sql}"
        );
    }

    /// Item 2 (ordinal form): `GROUP BY 1` must print the underlying
    /// result-column expression, not a bare `1`.
    #[test]
    fn group_by_ordinal_reference_prints_underlying_expr() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = select_plan(&conn, "SELECT v, COUNT(*) FROM t GROUP BY 1");
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.contains("GROUP BY t.v"),
            "expected GROUP BY 1 to print the underlying column via the \
             shared PlanContext, got: {sql}"
        );
    }

    /// Item 2 (alias form): `GROUP BY <result-column alias>` must print the
    /// underlying expression the alias stands for.
    #[test]
    fn group_by_alias_reference_prints_underlying_expr() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = select_plan(
            &conn,
            "SELECT v AS alias_v, COUNT(*) FROM t GROUP BY alias_v",
        );
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.contains("GROUP BY t.v"),
            "expected the alias to resolve to its underlying column via the \
             shared PlanContext, got: {sql}"
        );
    }

    /// Item 3: each GROUP BY expression must print its `ASC`/`DESC` sort
    /// order (mirroring ORDER BY), not a bare expression list.
    #[test]
    fn group_by_prints_sort_order() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = select_plan(&conn, "SELECT v, COUNT(*) FROM t GROUP BY v");
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.contains("GROUP BY t.v ASC"),
            "expected an ASC/DESC suffix on the GROUP BY expression, got: {sql}"
        );
    }

    /// Item 4: `UPDATE OR <action> ...` must print the conflict clause.
    #[test]
    fn update_or_conflict_clause_prints() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = update_plan(&conn, "UPDATE OR REPLACE t SET v = 'x'");
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.contains("UPDATE OR REPLACE"),
            "expected the OR REPLACE conflict clause to print, got: {sql}"
        );
    }

    /// Regression guard for a plain `UPDATE` (no conflict clause): must NOT
    /// gain a spurious `OR ...` now that item 4 threads `or_conflict` through.
    #[test]
    fn update_without_conflict_clause_prints_plain_update() {
        let (io, conn) = new_conn();
        exec(&io, &conn, "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)");
        let plan = update_plan(&conn, "UPDATE t SET v = 'x'");
        let sql = plan.to_sql_string(&PlanContext(&[&plan.table_references]));
        assert!(
            sql.starts_with("UPDATE t SET"),
            "expected a plain UPDATE with no OR clause, got: {sql}"
        );
    }
}
