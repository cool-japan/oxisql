use crate::{
    schema::{self, Column, Schema, Type},
    translate::{
        collate::CollationSeq,
        expr::walk_expr,
        plan::{JoinOrderMember, Plan, QueryDestination},
        select::prepare_select_plan,
    },
    types::{Value, ValueType},
    vdbe::builder::TableRefIdCounter,
    LimboError, OpenFlags, Result, Statement, StepResult, SymbolTable, IO,
};
use limbo_sqlite3_parser::ast::{
    self, CreateTableBody, Expr, FunctionTail, Literal, UnaryOperator,
};
use std::{rc::Rc, sync::Arc};

pub trait RoundToPrecision {
    fn round_to_precision(self, precision: i32) -> f64;
}

impl RoundToPrecision for f64 {
    fn round_to_precision(self, precision: i32) -> f64 {
        let factor = 10f64.powi(precision);
        (self * factor).round() / factor
    }
}

// https://sqlite.org/lang_keywords.html
const QUOTE_PAIRS: &[(char, char)] = &[('"', '"'), ('[', ']'), ('`', '`')];

pub fn normalize_ident(identifier: &str) -> String {
    let quote_pair = QUOTE_PAIRS
        .iter()
        .find(|&(start, end)| identifier.starts_with(*start) && identifier.ends_with(*end));

    if let Some(&(_, _)) = quote_pair {
        &identifier[1..identifier.len() - 1]
    } else {
        identifier
    }
    .to_lowercase()
}

/// Strip the surrounding SQL quotes from an identifier *without* altering its
/// case, unescaping SQLite's doubled-quote escapes (e.g. `"a""b"` becomes
/// `a"b`). Unlike [`normalize_ident`], the original letter case is preserved,
/// which is what SQLite does when it derives a result-column name from an
/// `AS`-alias: `SELECT a AS "Col One"` yields the column name `Col One`, not
/// `col one`. An unquoted identifier is returned unchanged.
pub fn dequote_ident(identifier: &str) -> String {
    if identifier.len() >= 2 {
        if let Some(&(_, end)) = QUOTE_PAIRS
            .iter()
            .find(|&(start, end)| identifier.starts_with(*start) && identifier.ends_with(*end))
        {
            let inner = &identifier[1..identifier.len() - 1];
            // `[...]` identifiers have no escape mechanism; the other quote
            // styles escape their delimiter by doubling it.
            if end == ']' {
                return inner.to_string();
            }
            let quote = end.to_string();
            let doubled = format!("{quote}{quote}");
            return inner.replace(&doubled, &quote);
        }
    }
    identifier.to_string()
}

/// Extract the case-preserving, dequoted name of a result-column `AS`-alias,
/// used for the output column's name (result-set header, view/subquery column
/// names, `PRAGMA table_info`). See [`dequote_ident`] for why the case is kept.
pub fn alias_name(alias: &ast::As) -> String {
    let name = match alias {
        ast::As::As(id) | ast::As::Elided(id) => &id.0,
    };
    dequote_ident(name)
}

pub const PRIMARY_KEY_AUTOMATIC_INDEX_NAME_PREFIX: &str = "sqlite_autoindex_";

/// Unparsed index that comes from a sql query, i.e not an automatic index
///
/// CREATE INDEX idx ON table_name(sql)
struct UnparsedFromSqlIndex {
    table_name: String,
    root_page: usize,
    sql: String,
}

pub fn parse_schema_rows(
    rows: Option<Statement>,
    schema: &mut Schema,
    io: Arc<dyn IO>,
    syms: &SymbolTable,
    mv_tx_id: Option<u64>,
) -> Result<()> {
    if let Some(mut rows) = rows {
        rows.set_mv_tx_id(mv_tx_id);
        // TODO: if we IO, this unparsed indexes is lost. Will probably need some state between
        // IO runs
        let mut from_sql_indexes = Vec::with_capacity(10);
        let mut automatic_indices: std::collections::HashMap<String, Vec<(String, usize)>> =
            std::collections::HashMap::with_capacity(10);
        loop {
            match rows.step()? {
                StepResult::Row => {
                    let row = rows
                        .row()
                        .expect("invariant: row available after StepResult::Row"); // UPSTREAM (Limbo): unwrap — needs proper error propagation
                    let ty = row.get::<&str>(0)?;
                    if !["table", "index", "view", "trigger"].contains(&ty) {
                        continue;
                    }
                    match ty {
                        "table" => {
                            let root_page: i64 = row.get::<i64>(3)?;
                            let sql: &str = row.get::<&str>(4)?;
                            if root_page == 0 && sql.to_lowercase().contains("create virtual") {
                                let name: &str = row.get::<&str>(1)?;
                                // a virtual table is found in the sqlite_schema, but it's no
                                // longer in the in-memory schema. We need to recreate it if
                                // the module is loaded in the symbol table.
                                let vtab = if let Some(vtab) = syms.vtabs.get(name) {
                                    vtab.clone()
                                } else {
                                    let mod_name = module_name_from_sql(sql)?;
                                    crate::VirtualTable::table(
                                        Some(name),
                                        mod_name,
                                        module_args_from_sql(sql)?,
                                        syms,
                                    )?
                                };
                                schema.add_virtual_table(vtab);
                            } else {
                                let table = schema::BTreeTable::from_sql(sql, root_page as usize)?;
                                schema.add_btree_table(Rc::new(table));
                            }
                        }
                        "index" => {
                            let root_page: i64 = row.get::<i64>(3)?;
                            match row.get::<&str>(4) {
                                Ok(sql) => {
                                    from_sql_indexes.push(UnparsedFromSqlIndex {
                                        table_name: row.get::<&str>(2)?.to_string(),
                                        root_page: root_page as usize,
                                        sql: sql.to_string(),
                                    });
                                }
                                _ => {
                                    // Automatic index on primary key and/or unique constraint, e.g.
                                    // table|foo|foo|2|CREATE TABLE foo (a text PRIMARY KEY, b)
                                    // index|sqlite_autoindex_foo_1|foo|3|
                                    let index_name = row.get::<&str>(1)?.to_string();
                                    let table_name = row.get::<&str>(2)?.to_string();
                                    let root_page = row.get::<i64>(3)?;
                                    match automatic_indices.entry(table_name) {
                                        std::collections::hash_map::Entry::Vacant(e) => {
                                            e.insert(vec![(index_name, root_page as usize)]);
                                        }
                                        std::collections::hash_map::Entry::Occupied(mut e) => {
                                            e.get_mut().push((index_name, root_page as usize));
                                        }
                                    }
                                }
                            }
                        }
                        "view" => {
                            let name: &str = row.get::<&str>(1)?;
                            let sql: &str = row.get::<&str>(4)?;
                            // Register the view with an empty `columns` list so it
                            // is immediately name-resolvable regardless of scan
                            // order. Output columns are NOT inferred here: doing so
                            // requires planning the view's `SELECT` body (for
                            // `object_view` in `proj.db`, a large `UNION ALL`), and
                            // that cost was paid on EVERY connection open even
                            // though the view body is re-planned from scratch when
                            // the view is referenced in a query, and the inferred
                            // `columns` list is consulted only by `PRAGMA
                            // table_info`. Column inference is therefore deferred to
                            // first `PRAGMA table_info` use (see
                            // `translate::pragma`), keeping schema load cheap. A
                            // view whose SQL no longer parses as CREATE VIEW is
                            // skipped entirely (it degrades to "not found" on use)
                            // rather than aborting the whole schema load.
                            match schema::View::from_sql(sql, name) {
                                Ok(view) => {
                                    schema.add_view(Rc::new(view));
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        "skipping malformed view {} in sqlite_schema: {}",
                                        name,
                                        e
                                    );
                                }
                            }
                        }
                        "trigger" => {
                            let name: &str = row.get::<&str>(1)?;
                            let sql: &str = row.get::<&str>(4)?;
                            // Same failure policy as views: a trigger row whose
                            // SQL no longer re-parses is skipped with a warning
                            // instead of aborting the whole schema load, so one
                            // bad row can never make a database unopenable.
                            match schema::Trigger::from_sql(sql) {
                                Ok(trigger) => schema.add_trigger_owned(trigger),
                                Err(e) => {
                                    tracing::warn!(
                                        "skipping malformed trigger {} in sqlite_schema: {}",
                                        name,
                                        e
                                    );
                                }
                            }
                        }
                        _ => continue,
                    }
                }
                StepResult::IO => {
                    // TODO: How do we ensure that the I/O we submitted to
                    // read the schema is actually complete?
                    io.run_once()?;
                }
                StepResult::Interrupt => break,
                StepResult::Done => break,
                StepResult::Busy => break,
            }
        }
        for unparsed_sql_from_index in from_sql_indexes {
            #[cfg(not(feature = "index_experimental"))]
            schema.table_set_has_index(&unparsed_sql_from_index.table_name);
            #[cfg(feature = "index_experimental")]
            {
                let table = schema
                    .get_btree_table(&unparsed_sql_from_index.table_name)
                    .expect("invariant: table must exist before index is added"); // UPSTREAM (Limbo): unwrap — needs proper error propagation
                let index = schema::Index::from_sql(
                    &unparsed_sql_from_index.sql,
                    unparsed_sql_from_index.root_page as usize,
                    table.as_ref(),
                )?;
                schema.add_index(Arc::new(index));
            }
        }
        for automatic_index in automatic_indices {
            #[cfg(not(feature = "index_experimental"))]
            schema.table_set_has_index(&automatic_index.0);
            #[cfg(feature = "index_experimental")]
            {
                let table = schema
                    .get_btree_table(&automatic_index.0)
                    .expect("invariant: table must exist before automatic index"); // UPSTREAM (Limbo): unwrap — needs proper error propagation
                let ret_index = schema::Index::automatic_from_primary_key_and_unique(
                    table.as_ref(),
                    automatic_index.1,
                )?;
                for index in ret_index {
                    schema.add_index(Arc::new(index));
                }
            }
        }
        // View output columns are resolved lazily on first `PRAGMA table_info`
        // use (see `translate::pragma::query_pragma`), not eagerly here, so a
        // connection open never pays the view-body planning cost.
    }
    Ok(())
}

/// Infer the output [`Column`]s of a view by planning its stored `SELECT` body.
///
/// Column *names* follow the same rule SQLite uses: the body's result-column
/// names, or -- for a compound (`UNION [ALL]`/`INTERSECT`/`EXCEPT`) body -- the
/// left-most arm's names. An explicit `CREATE VIEW v(a, b, c)` column list
/// overrides the inferred names (with a count-mismatch check). Types are left
/// unspecified (empty), matching SQLite's `PRAGMA table_info` output for views.
pub(crate) fn resolve_view_columns(
    schema: &Schema,
    syms: &SymbolTable,
    view: &schema::View,
) -> Result<Vec<Column>> {
    let mut counter = TableRefIdCounter::new();
    let plan = prepare_select_plan(
        schema,
        (*view.select).clone(),
        syms,
        &[],
        &mut counter,
        QueryDestination::ResultRows,
    )?;
    let inferred = view_column_names(&plan);
    let names = match &view.explicit_column_names {
        Some(explicit) => {
            if explicit.len() != inferred.len() {
                crate::bail_parse_error!(
                    "expected {} columns for {} but got {}",
                    explicit.len(),
                    view.name,
                    inferred.len()
                );
            }
            explicit.iter().cloned().map(Some).collect()
        }
        None => inferred,
    };
    Ok(names.into_iter().map(new_view_column).collect())
}

/// The output column names of a planned view body: the left-most SELECT arm's
/// result-column names (SQLite names a compound SELECT's columns after its
/// left-most arm).
fn view_column_names(plan: &Plan) -> Vec<Option<String>> {
    let name_source = match plan {
        Plan::Select(select) => select,
        Plan::CompoundSelect {
            left, right_most, ..
        } => left.first().map(|(p, _)| p).unwrap_or(right_most),
        Plan::Delete(_) | Plan::Update(_) => return Vec::new(),
    };
    name_source
        .result_columns
        .iter()
        .map(|rc| rc.name(&name_source.table_references).map(String::from))
        .collect()
}

/// Build a view output [`Column`] with an unspecified (empty) type, mirroring
/// SQLite's treatment of view columns.
fn new_view_column(name: Option<String>) -> Column {
    Column {
        name,
        ty: Type::Null,
        ty_str: String::new(),
        primary_key: false,
        is_rowid_alias: false,
        notnull: false,
        default: None,
        unique: false,
        unique_conflict: limbo_sqlite3_parser::ast::ResolveType::Abort,
        collation: None,
        is_generated: false,
    }
}

/// Load persisted `sqlite_stat1` rows into `schema.stats`.
///
/// Mirrors [`parse_schema_rows`]'s row-stepping mechanism. Robust: NULL/non-text
/// columns or malformed `stat` strings are skipped, never panic. An absent
/// `sqlite_stat1` table is handled by the caller (it passes `rows = None`,
/// making this a no-op).
pub fn load_stat1(
    rows: Option<Statement>,
    schema: &mut Schema,
    io: Arc<dyn IO>,
    mv_tx_id: Option<u64>,
) -> Result<()> {
    if let Some(mut rows) = rows {
        rows.set_mv_tx_id(mv_tx_id);
        loop {
            match rows.step()? {
                StepResult::Row => {
                    let Some(row) = rows.row() else {
                        continue;
                    };
                    // col0 tbl (text, required); skip row if NULL/non-text.
                    let Ok(tbl) = row.get::<&str>(0) else {
                        continue;
                    };
                    // col1 idx (text, nullable): NULL/non-text => None (not an error).
                    let idx = row.get::<&str>(1).ok();
                    // col2 stat (text, required); skip row if NULL/non-text.
                    let Ok(stat) = row.get::<&str>(2) else {
                        continue;
                    };
                    schema.stats.record(tbl, idx, stat);
                }
                StepResult::IO => {
                    io.run_once()?;
                }
                StepResult::Interrupt => break,
                StepResult::Done => break,
                StepResult::Busy => break,
            }
        }
    }
    Ok(())
}

fn cmp_numeric_strings(num_str: &str, other: &str) -> bool {
    match (num_str.parse::<f64>(), other.parse::<f64>()) {
        (Ok(num), Ok(other)) => num == other,
        _ => num_str == other,
    }
}

pub fn check_ident_equivalency(ident1: &str, ident2: &str) -> bool {
    fn strip_quotes(identifier: &str) -> &str {
        for &(start, end) in QUOTE_PAIRS {
            if identifier.starts_with(start) && identifier.ends_with(end) {
                return &identifier[1..identifier.len() - 1];
            }
        }
        identifier
    }
    strip_quotes(ident1).eq_ignore_ascii_case(strip_quotes(ident2))
}

fn module_name_from_sql(sql: &str) -> Result<&str> {
    if let Some(start) = sql.find("USING") {
        let start = start + 6;
        // stop at the first space, semicolon, or parenthesis
        let end = sql[start..]
            .find(|c: char| c.is_whitespace() || c == ';' || c == '(')
            .unwrap_or(sql.len() - start)
            + start;
        Ok(sql[start..end].trim())
    } else {
        Err(LimboError::InvalidArgument(
            "Expected 'USING' in module name".to_string(),
        ))
    }
}

// CREATE VIRTUAL TABLE table_name USING module_name(arg1, arg2, ...);
// CREATE VIRTUAL TABLE table_name USING module_name;
fn module_args_from_sql(sql: &str) -> Result<Vec<limbo_ext::Value>> {
    if !sql.contains('(') {
        return Ok(vec![]);
    }
    let start = sql.find('(').ok_or_else(|| {
        LimboError::InvalidArgument("Expected '(' in module argument list".to_string())
    })? + 1;
    let end = sql.rfind(')').ok_or_else(|| {
        LimboError::InvalidArgument("Expected ')' in module argument list".to_string())
    })?;

    let mut args = Vec::new();
    let mut current_arg = String::new();
    let mut chars = sql[start..end].chars().peekable();
    let mut in_quotes = false;

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                if in_quotes {
                    if chars.peek() == Some(&'\'') {
                        // Escaped quote
                        current_arg.push('\'');
                        chars.next();
                    } else {
                        in_quotes = false;
                        args.push(limbo_ext::Value::from_text(current_arg.trim().to_string()));
                        current_arg.clear();
                        // Skip until comma or end
                        while let Some(&nc) = chars.peek() {
                            if nc == ',' {
                                chars.next(); // Consume comma
                                break;
                            } else if nc.is_whitespace() {
                                chars.next();
                            } else {
                                return Err(LimboError::InvalidArgument(
                                    "Unexpected characters after quoted argument".to_string(),
                                ));
                            }
                        }
                    }
                } else {
                    in_quotes = true;
                }
            }
            ',' => {
                if !in_quotes {
                    if !current_arg.trim().is_empty() {
                        args.push(limbo_ext::Value::from_text(current_arg.trim().to_string()));
                        current_arg.clear();
                    }
                } else {
                    current_arg.push(c);
                }
            }
            _ => {
                current_arg.push(c);
            }
        }
    }

    if !current_arg.trim().is_empty() && !in_quotes {
        args.push(limbo_ext::Value::from_text(current_arg.trim().to_string()));
    }

    if in_quotes {
        return Err(LimboError::InvalidArgument(
            "Unterminated string literal in module arguments".to_string(),
        ));
    }

    Ok(args)
}

pub fn check_literal_equivalency(lhs: &Literal, rhs: &Literal) -> bool {
    match (lhs, rhs) {
        (Literal::Numeric(n1), Literal::Numeric(n2)) => cmp_numeric_strings(n1, n2),
        (Literal::String(s1), Literal::String(s2)) => check_ident_equivalency(s1, s2),
        (Literal::Blob(b1), Literal::Blob(b2)) => b1 == b2,
        (Literal::Keyword(k1), Literal::Keyword(k2)) => check_ident_equivalency(k1, k2),
        (Literal::Null, Literal::Null) => true,
        (Literal::CurrentDate, Literal::CurrentDate) => true,
        (Literal::CurrentTime, Literal::CurrentTime) => true,
        (Literal::CurrentTimestamp, Literal::CurrentTimestamp) => true,
        _ => false,
    }
}

/// This function is used to determine whether two expressions are logically
/// equivalent in the context of queries, even if their representations
/// differ. e.g.: `SUM(x)` and `sum(x)`, `x + y` and `y + x`
///
/// *Note*: doesn't attempt to evaluate/compute "constexpr" results
pub fn exprs_are_equivalent(expr1: &Expr, expr2: &Expr) -> bool {
    match (expr1, expr2) {
        (
            Expr::Between {
                lhs: lhs1,
                not: not1,
                start: start1,
                end: end1,
            },
            Expr::Between {
                lhs: lhs2,
                not: not2,
                start: start2,
                end: end2,
            },
        ) => {
            not1 == not2
                && exprs_are_equivalent(lhs1, lhs2)
                && exprs_are_equivalent(start1, start2)
                && exprs_are_equivalent(end1, end2)
        }
        (Expr::Binary(lhs1, op1, rhs1), Expr::Binary(lhs2, op2, rhs2)) => {
            op1 == op2
                && ((exprs_are_equivalent(lhs1, lhs2) && exprs_are_equivalent(rhs1, rhs2))
                    || (op1.is_commutative()
                        && exprs_are_equivalent(lhs1, rhs2)
                        && exprs_are_equivalent(rhs1, lhs2)))
        }
        (
            Expr::Case {
                base: base1,
                when_then_pairs: pairs1,
                else_expr: else1,
            },
            Expr::Case {
                base: base2,
                when_then_pairs: pairs2,
                else_expr: else2,
            },
        ) => {
            base1 == base2
                && pairs1.len() == pairs2.len()
                && pairs1.iter().zip(pairs2).all(|((w1, t1), (w2, t2))| {
                    exprs_are_equivalent(w1, w2) && exprs_are_equivalent(t1, t2)
                })
                && else1 == else2
        }
        (
            Expr::Cast {
                expr: expr1,
                type_name: type1,
            },
            Expr::Cast {
                expr: expr2,
                type_name: type2,
            },
        ) => {
            exprs_are_equivalent(expr1, expr2)
                && match (type1, type2) {
                    (Some(t1), Some(t2)) => t1.name.eq_ignore_ascii_case(&t2.name),
                    _ => false,
                }
        }
        (Expr::Collate(expr1, collation1), Expr::Collate(expr2, collation2)) => {
            exprs_are_equivalent(expr1, expr2) && collation1.eq_ignore_ascii_case(collation2)
        }
        (
            Expr::FunctionCall {
                name: name1,
                distinctness: distinct1,
                args: args1,
                order_by: order1,
                filter_over: filter1,
            },
            Expr::FunctionCall {
                name: name2,
                distinctness: distinct2,
                args: args2,
                order_by: order2,
                filter_over: filter2,
            },
        ) => {
            name1.0.eq_ignore_ascii_case(&name2.0)
                && distinct1 == distinct2
                && args1 == args2
                && order1 == order2
                && filter1 == filter2
        }
        (
            Expr::FunctionCallStar {
                name: name1,
                filter_over: filter1,
            },
            Expr::FunctionCallStar {
                name: name2,
                filter_over: filter2,
            },
        ) => {
            name1.0.eq_ignore_ascii_case(&name2.0)
                && match (filter1, filter2) {
                    (None, None) => true,
                    (
                        Some(FunctionTail {
                            filter_clause: fc1,
                            over_clause: oc1,
                        }),
                        Some(FunctionTail {
                            filter_clause: fc2,
                            over_clause: oc2,
                        }),
                    ) => match ((fc1, fc2), (oc1, oc2)) {
                        ((Some(fc1), Some(fc2)), (Some(oc1), Some(oc2))) => {
                            exprs_are_equivalent(fc1, fc2) && oc1 == oc2
                        }
                        ((Some(fc1), Some(fc2)), _) => exprs_are_equivalent(fc1, fc2),
                        _ => false,
                    },
                    _ => false,
                }
        }
        (Expr::NotNull(expr1), Expr::NotNull(expr2)) => exprs_are_equivalent(expr1, expr2),
        (Expr::IsNull(expr1), Expr::IsNull(expr2)) => exprs_are_equivalent(expr1, expr2),
        (Expr::Literal(lit1), Expr::Literal(lit2)) => check_literal_equivalency(lit1, lit2),
        (Expr::Id(id1), Expr::Id(id2)) => check_ident_equivalency(&id1.0, &id2.0),
        (Expr::Unary(op1, expr1), Expr::Unary(op2, expr2)) => {
            op1 == op2 && exprs_are_equivalent(expr1, expr2)
        }
        // Variables that are not bound to a specific value, are treated as NULL
        // https://sqlite.org/lang_expr.html#varparam
        (Expr::Variable(var), Expr::Variable(var2)) if var == "" && var2 == "" => false,
        // Named variables can be compared by their name
        (Expr::Variable(val), Expr::Variable(val2)) => val == val2,
        (Expr::Parenthesized(exprs1), Expr::Parenthesized(exprs2)) => {
            exprs1.len() == exprs2.len()
                && exprs1
                    .iter()
                    .zip(exprs2)
                    .all(|(e1, e2)| exprs_are_equivalent(e1, e2))
        }
        (Expr::Parenthesized(exprs1), exprs2) | (exprs2, Expr::Parenthesized(exprs1)) => {
            exprs1.len() == 1 && exprs_are_equivalent(&exprs1[0], exprs2)
        }
        (Expr::Qualified(tn1, cn1), Expr::Qualified(tn2, cn2)) => {
            check_ident_equivalency(&tn1.0, &tn2.0) && check_ident_equivalency(&cn1.0, &cn2.0)
        }
        (Expr::DoublyQualified(sn1, tn1, cn1), Expr::DoublyQualified(sn2, tn2, cn2)) => {
            check_ident_equivalency(&sn1.0, &sn2.0)
                && check_ident_equivalency(&tn1.0, &tn2.0)
                && check_ident_equivalency(&cn1.0, &cn2.0)
        }
        (
            Expr::InList {
                lhs: lhs1,
                not: not1,
                rhs: rhs1,
            },
            Expr::InList {
                lhs: lhs2,
                not: not2,
                rhs: rhs2,
            },
        ) => {
            *not1 == *not2
                && exprs_are_equivalent(lhs1, lhs2)
                && rhs1
                    .as_ref()
                    .zip(rhs2.as_ref())
                    .map(|(list1, list2)| {
                        list1.len() == list2.len()
                            && list1
                                .iter()
                                .zip(list2)
                                .all(|(e1, e2)| exprs_are_equivalent(e1, e2))
                    })
                    .unwrap_or(false)
        }
        // fall back to naive equality check
        _ => expr1 == expr2,
    }
}

pub fn columns_from_create_table_body(body: &ast::CreateTableBody) -> crate::Result<Vec<Column>> {
    let CreateTableBody::ColumnsAndConstraints { columns, .. } = body else {
        return Err(crate::LimboError::ParseError(
            "CREATE TABLE body must contain columns and constraints".to_string(),
        ));
    };

    Ok(columns
        .into_iter()
        .filter_map(|(name, column_def)| {
            // if column_def.col_type includes HIDDEN, omit it for now
            if let Some(data_type) = column_def.col_type.as_ref() {
                if data_type.name.as_str().contains("HIDDEN") {
                    return None;
                }
            }
            let column =
                Column {
                    name: Some(normalize_ident(&name.0)),
                    ty: match column_def.col_type {
                        Some(ref data_type) => {
                            // https://www.sqlite.org/datatype3.html
                            let type_name = data_type.name.as_str().to_uppercase();
                            if type_name.contains("INT") {
                                Type::Integer
                            } else if type_name.contains("CHAR")
                                || type_name.contains("CLOB")
                                || type_name.contains("TEXT")
                            {
                                Type::Text
                            } else if type_name.contains("BLOB") || type_name.is_empty() {
                                Type::Blob
                            } else if type_name.contains("REAL")
                                || type_name.contains("FLOA")
                                || type_name.contains("DOUB")
                            {
                                Type::Real
                            } else {
                                Type::Numeric
                            }
                        }
                        None => Type::Null,
                    },
                    default: column_def
                        .constraints
                        .iter()
                        .find_map(|c| match &c.constraint {
                            limbo_sqlite3_parser::ast::ColumnConstraint::Default(val) => {
                                Some(val.clone())
                            }
                            _ => None,
                        }),
                    notnull: column_def.constraints.iter().any(|c| {
                        matches!(
                            c.constraint,
                            limbo_sqlite3_parser::ast::ColumnConstraint::NotNull { .. }
                        )
                    }),
                    ty_str: column_def
                        .col_type
                        .clone()
                        .map(|t| t.name.to_string())
                        .unwrap_or_default(),
                    primary_key: column_def.constraints.iter().any(|c| {
                        matches!(
                            c.constraint,
                            limbo_sqlite3_parser::ast::ColumnConstraint::PrimaryKey { .. }
                        )
                    }),
                    is_rowid_alias: false,
                    unique: column_def.constraints.iter().any(|c| {
                        matches!(
                            c.constraint,
                            limbo_sqlite3_parser::ast::ColumnConstraint::Unique(..)
                        )
                    }),
                    unique_conflict: column_def
                        .constraints
                        .iter()
                        .find_map(|c| match &c.constraint {
                            limbo_sqlite3_parser::ast::ColumnConstraint::Unique(Some(resolve)) => {
                                Some(*resolve)
                            }
                            _ => None,
                        })
                        .unwrap_or(limbo_sqlite3_parser::ast::ResolveType::Abort),
                    collation: column_def
                        .constraints
                        .iter()
                        .find_map(|c| match &c.constraint {
                            // TODO: see if this should be the correct behavior
                            // currently there cannot be any user defined collation sequences.
                            // But in the future, when a user defines a collation sequence, creates a table with it,
                            // then closes the db and opens it again. This may panic here if the collation seq is not registered
                            // before reading the columns
                            limbo_sqlite3_parser::ast::ColumnConstraint::Collate {
                                collation_name,
                            } => Some(CollationSeq::new(collation_name.0.as_str()).expect(
                                "collation should have been set correctly in create table",
                            )),
                            _ => None,
                        }),
                    is_generated: column_def.constraints.iter().any(|c| {
                        matches!(
                            c.constraint,
                            limbo_sqlite3_parser::ast::ColumnConstraint::Generated { .. }
                        )
                    }),
                };
            Some(column)
        })
        .collect::<Vec<_>>())
}

/// This function checks if a given expression is a constant value that can be pushed down to the database engine.
/// It is expected to be called with the other half of a binary expression with an Expr::Column
pub fn can_pushdown_predicate(
    top_level_expr: &Expr,
    table_idx: usize,
    join_order: &[JoinOrderMember],
) -> Result<bool> {
    let mut can_pushdown = true;
    walk_expr(top_level_expr, &mut |expr: &Expr| -> Result<()> {
        match expr {
            Expr::Column { table, .. } | Expr::RowId { table, .. } => {
                let join_idx = join_order
                    .iter()
                    .position(|t| t.table_id == *table)
                    .expect("table not found in join_order");
                can_pushdown &= join_idx <= table_idx;
            }
            Expr::FunctionCall { args, name, .. } => {
                let function = crate::function::Func::resolve_function(
                    &name.0,
                    args.as_ref().map_or(0, |a| a.len()),
                )?;
                // is deterministic
                can_pushdown &= function.is_deterministic();
            }
            _ => {}
        };
        Ok(())
    })?;

    Ok(can_pushdown)
}

#[derive(Debug, Default, PartialEq)]
pub struct OpenOptions<'a> {
    /// The authority component of the URI. may be 'localhost' or empty
    pub authority: Option<&'a str>,
    /// The normalized path to the database file
    pub path: String,
    /// The vfs query parameter causes the database connection to be opened using the VFS called NAME
    pub vfs: Option<String>,
    /// read-only, read-write, read-write and created if it does not exist, or pure in-memory database that never interacts with disk
    pub mode: OpenMode,
    /// Attempt to set the permissions of the new database file to match the existing file "filename".
    pub modeof: Option<String>,
    /// Specifies Cache mode shared | private
    pub cache: CacheMode,
    /// immutable=1|0 specifies that the database is stored on read-only media
    pub immutable: bool,
}

#[derive(Clone, Default, Debug, Copy, PartialEq)]
pub enum OpenMode {
    ReadOnly,
    ReadWrite,
    Memory,
    #[default]
    ReadWriteCreate,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub enum CacheMode {
    #[default]
    Private,
    Shared,
}

impl From<&str> for CacheMode {
    fn from(s: &str) -> Self {
        match s {
            "private" => CacheMode::Private,
            "shared" => CacheMode::Shared,
            _ => CacheMode::Private,
        }
    }
}

impl OpenMode {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_lowercase().as_str() {
            "ro" => Ok(OpenMode::ReadOnly),
            "rw" => Ok(OpenMode::ReadWrite),
            "memory" => Ok(OpenMode::Memory),
            "rwc" => Ok(OpenMode::ReadWriteCreate),
            _ => Err(LimboError::InvalidArgument(format!(
                "Invalid mode: '{}'. Expected one of 'ro', 'rw', 'memory', 'rwc'",
                s
            ))),
        }
    }
    pub fn get_flags(&self) -> OpenFlags {
        match self {
            OpenMode::ReadWriteCreate => OpenFlags::Create,
            _ => OpenFlags::None,
        }
    }
}

fn is_windows_path(path: &str) -> bool {
    path.len() >= 3
        && path.chars().nth(1) == Some(':')
        && (path.chars().nth(2) == Some('/') || path.chars().nth(2) == Some('\\'))
}

/// converts windows-style paths to forward slashes, per SQLite spec.
fn normalize_windows_path(path: &str) -> String {
    let mut normalized = path.replace("\\", "/");

    // remove duplicate slashes (`//` → `/`)
    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }

    // if absolute windows path (`C:/...`), ensure it starts with `/`
    if normalized.len() >= 3
        && !normalized.starts_with('/')
        && normalized.chars().nth(1) == Some(':')
        && normalized.chars().nth(2) == Some('/')
    {
        normalized.insert(0, '/');
    }
    normalized
}

/// Parses a SQLite URI, handling Windows and Unix paths separately.
pub fn parse_sqlite_uri(uri: &str) -> Result<OpenOptions> {
    if !uri.starts_with("file:") {
        return Ok(OpenOptions {
            path: uri.to_string(),
            ..Default::default()
        });
    }

    let mut opts = OpenOptions::default();
    let without_scheme = &uri[5..];

    let (without_fragment, _) = without_scheme
        .split_once('#')
        .unwrap_or((without_scheme, ""));

    let (without_query, query) = without_fragment
        .split_once('?')
        .unwrap_or((without_fragment, ""));
    parse_query_params(query, &mut opts)?;

    // handle authority + path separately
    if let Some(after_slashes) = without_query.strip_prefix("//") {
        let (authority, path) = after_slashes.split_once('/').unwrap_or((after_slashes, ""));

        // sqlite allows only `localhost` or empty authority.
        if !(authority.is_empty() || authority == "localhost") {
            return Err(LimboError::InvalidArgument(format!(
                "Invalid authority '{}'. Only '' or 'localhost' allowed.",
                authority
            )));
        }
        opts.authority = if authority.is_empty() {
            None
        } else {
            Some(authority)
        };

        if is_windows_path(path) {
            opts.path = normalize_windows_path(&decode_percent(path));
        } else if !path.is_empty() {
            opts.path = format!("/{}", decode_percent(path));
        } else {
            opts.path = String::new();
        }
    } else {
        // no authority, must be a normal absolute or relative path.
        opts.path = decode_percent(without_query);
    }

    Ok(opts)
}

// parses query parameters and updates OpenOptions
fn parse_query_params(query: &str, opts: &mut OpenOptions) -> Result<()> {
    for param in query.split('&') {
        if let Some((key, value)) = param.split_once('=') {
            let decoded_value = decode_percent(value);
            match key {
                "mode" => opts.mode = OpenMode::from_str(value)?,
                "modeof" => opts.modeof = Some(decoded_value),
                "cache" => opts.cache = decoded_value.as_str().into(),
                "immutable" => opts.immutable = decoded_value == "1",
                "vfs" => opts.vfs = Some(decoded_value),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Decodes percent-encoded characters
/// this function was adapted from the 'urlencoding' crate. MIT
pub fn decode_percent(uri: &str) -> String {
    let from_hex_digit = |digit: u8| -> Option<u8> {
        match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'A'..=b'F' => Some(digit - b'A' + 10),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            _ => None,
        }
    };

    let offset = uri.chars().take_while(|&c| c != '%').count();

    if offset >= uri.len() {
        return uri.to_string();
    }

    let mut decoded: Vec<u8> = Vec::with_capacity(uri.len());
    let (ascii, mut data) = uri.as_bytes().split_at(offset);
    decoded.extend_from_slice(ascii);

    loop {
        let mut parts = data.splitn(2, |&c| c == b'%');
        let non_escaped_part = parts
            .next()
            .expect("invariant: splitn(2,..) always yields first element"); // UPSTREAM (Limbo): unwrap — needs proper error propagation
        let rest = parts.next();
        if rest.is_none() && decoded.is_empty() {
            return String::from_utf8_lossy(data).to_string();
        }
        decoded.extend_from_slice(non_escaped_part);
        match rest {
            Some(rest) => match rest.get(0..2) {
                Some([first, second]) => match from_hex_digit(*first) {
                    Some(first_val) => match from_hex_digit(*second) {
                        Some(second_val) => {
                            decoded.push((first_val << 4) | second_val);
                            data = &rest[2..];
                        }
                        None => {
                            decoded.extend_from_slice(&[b'%', *first]);
                            data = &rest[1..];
                        }
                    },
                    None => {
                        decoded.push(b'%');
                        data = rest;
                    }
                },
                _ => {
                    decoded.push(b'%');
                    decoded.extend_from_slice(rest);
                    break;
                }
            },
            None => break,
        }
    }
    String::from_utf8_lossy(&decoded).to_string()
}

/// When casting a TEXT value to INTEGER, the longest possible prefix of the value that can be interpreted as an integer number
/// is extracted from the TEXT value and the remainder ignored. Any leading spaces in the TEXT value when converting from TEXT to INTEGER are ignored.
/// If there is no prefix that can be interpreted as an integer number, the result of the conversion is 0.
/// If the prefix integer is greater than +9223372036854775807 then the result of the cast is exactly +9223372036854775807.
/// Similarly, if the prefix integer is less than -9223372036854775808 then the result of the cast is exactly -9223372036854775808.
/// When casting to INTEGER, if the text looks like a floating point value with an exponent, the exponent will be ignored
/// because it is no part of the integer prefix. For example, "CAST('123e+5' AS INTEGER)" results in 123, not in 12300000.
/// The CAST operator understands decimal integers only — conversion of hexadecimal integers stops at the "x" in the "0x" prefix of the hexadecimal integer string and thus result of the CAST is always zero.
pub fn cast_text_to_integer(text: &str) -> Value {
    let text = text.trim();
    if text.is_empty() {
        return Value::Integer(0);
    }
    if let Ok(i) = text.parse::<i64>() {
        return Value::Integer(i);
    }
    let bytes = text.as_bytes();
    let mut end = 0;
    if bytes[0] == b'-' {
        end = 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    text[..end]
        .parse::<i64>()
        .map_or(Value::Integer(0), Value::Integer)
}

/// When casting a TEXT value to REAL, the longest possible prefix of the value that can be interpreted
/// as a real number is extracted from the TEXT value and the remainder ignored. Any leading spaces in
/// the TEXT value are ignored when converging from TEXT to REAL.
/// If there is no prefix that can be interpreted as a real number, the result of the conversion is 0.0.
pub fn cast_text_to_real(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::Float(0.0);
    }
    let Ok((_, text)) = parse_numeric_str(trimmed) else {
        return Value::Float(0.0);
    };
    text.parse::<f64>().map_or(Value::Float(0.0), Value::Float)
}

/// NUMERIC Casting a TEXT or BLOB value into NUMERIC yields either an INTEGER or a REAL result.
/// If the input text looks like an integer (there is no decimal point nor exponent) and the value
/// is small enough to fit in a 64-bit signed integer, then the result will be INTEGER.
/// Input text that looks like floating point (there is a decimal point and/or an exponent)
/// and the text describes a value that can be losslessly converted back and forth between IEEE 754
/// 64-bit float and a 51-bit signed integer, then the result is INTEGER. (In the previous sentence,
/// a 51-bit integer is specified since that is one bit less than the length of the mantissa of an
/// IEEE 754 64-bit float and thus provides a 1-bit of margin for the text-to-float conversion operation.)
/// Any text input that describes a value outside the range of a 64-bit signed integer yields a REAL result.
/// Casting a REAL or INTEGER value to NUMERIC is a no-op, even if a real value could be losslessly converted to an integer.
pub fn checked_cast_text_to_numeric(text: &str) -> std::result::Result<Value, ()> {
    // sqlite will parse the first N digits of a string to numeric value, then determine
    // whether _that_ value is more likely a real or integer value. e.g.
    // '-100234-2344.23e14' evaluates to -100234 instead of -100234.0
    let (kind, text) = parse_numeric_str(text)?;
    match kind {
        ValueType::Integer => match text.parse::<i64>() {
            Ok(i) => Ok(Value::Integer(i)),
            Err(e) => {
                if matches!(
                    e.kind(),
                    std::num::IntErrorKind::PosOverflow | std::num::IntErrorKind::NegOverflow
                ) {
                    // if overflow, we return the representation as a real.
                    // we have to match sqlite exactly here, so we match sqlite3AtoF
                    let value = text.parse::<f64>().unwrap_or_default();
                    let factor = 10f64.powi(15 - value.abs().log10().ceil() as i32);
                    Ok(Value::Float((value * factor).round() / factor))
                } else {
                    Err(())
                }
            }
        },
        ValueType::Float => Ok(text.parse::<f64>().map_or(Value::Float(0.0), Value::Float)),
        _ => unreachable!(),
    }
}

fn parse_numeric_str(text: &str) -> Result<(ValueType, &str), ()> {
    let text = text.trim();
    let bytes = text.as_bytes();

    if matches!(
        bytes,
        [] | [b'e', ..] | [b'E', ..] | [b'.', b'e' | b'E', ..]
    ) {
        return Err(());
    }

    let mut end = 0;
    let mut has_decimal = false;
    let mut has_exponent = false;
    if bytes[0] == b'-' {
        end = 1;
    }
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => end += 1,
            b'.' if !has_decimal && !has_exponent => {
                has_decimal = true;
                end += 1;
            }
            b'e' | b'E' if !has_exponent => {
                has_exponent = true;
                end += 1;
                // allow exponent sign
                if end < bytes.len() && (bytes[end] == b'+' || bytes[end] == b'-') {
                    end += 1;
                }
            }
            _ => break,
        }
    }
    if end == 0 || (end == 1 && bytes[0] == b'-') {
        return Err(());
    }
    // edge case: if it ends with exponent, strip and cast valid digits as float
    let last = bytes[end - 1];
    if last.eq_ignore_ascii_case(&b'e') {
        return Ok((ValueType::Float, &text[0..end - 1]));
    // edge case: ends with extponent / sign
    } else if has_exponent && (last == b'-' || last == b'+') {
        return Ok((ValueType::Float, &text[0..end - 2]));
    }
    Ok((
        if !has_decimal && !has_exponent {
            ValueType::Integer
        } else {
            ValueType::Float
        },
        &text[..end],
    ))
}

pub fn cast_text_to_numeric(txt: &str) -> Value {
    checked_cast_text_to_numeric(txt).unwrap_or(Value::Integer(0))
}

// Check if float can be losslessly converted to 51-bit integer
pub fn cast_real_to_integer(float: f64) -> std::result::Result<i64, ()> {
    let i = float as i64;
    if float == i as f64 && i.abs() < (1i64 << 51) {
        return Ok(i);
    }
    Err(())
}

// we don't need to verify the numeric literal here, as it is already verified by the parser
pub fn parse_numeric_literal(text: &str) -> Result<Value> {
    // a single extra underscore ("_") character can exist between any two digits
    let text = text.replace("_", "");

    if text.starts_with("0x") || text.starts_with("0X") {
        let value = u64::from_str_radix(&text[2..], 16)? as i64;
        return Ok(Value::Integer(value));
    } else if text.starts_with("-0x") || text.starts_with("-0X") {
        let value = u64::from_str_radix(&text[3..], 16)? as i64;
        if value == i64::MIN {
            return Err(LimboError::IntegerOverflow);
        }
        return Ok(Value::Integer(-value));
    }

    if let Ok(int_value) = text.parse::<i64>() {
        return Ok(Value::Integer(int_value));
    }

    let float_value = text.parse::<f64>()?;
    Ok(Value::Float(float_value))
}

pub fn parse_signed_number(expr: &Expr) -> Result<Value> {
    match expr {
        Expr::Literal(Literal::Numeric(num)) => parse_numeric_literal(num),
        Expr::Unary(op, expr) => match (op, expr.as_ref()) {
            (UnaryOperator::Negative, Expr::Literal(Literal::Numeric(num))) => {
                let data = "-".to_owned() + &num.to_string();
                parse_numeric_literal(&data)
            }
            (UnaryOperator::Positive, Expr::Literal(Literal::Numeric(num))) => {
                parse_numeric_literal(num)
            }
            _ => Err(LimboError::InvalidArgument(
                "signed-number must follow the format: ([+|-] numeric-literal)".to_string(),
            )),
        },
        _ => Err(LimboError::InvalidArgument(
            "signed-number must follow the format: ([+|-] numeric-literal)".to_string(),
        )),
    }
}

// for TVF's we need these at planning time so we cannot emit translate_expr
pub fn vtable_args(args: &[ast::Expr]) -> Vec<limbo_ext::Value> {
    let mut vtable_args = Vec::new();
    for arg in args {
        match arg {
            Expr::Literal(lit) => match lit {
                Literal::Numeric(i) => {
                    if i.contains('.') {
                        vtable_args.push(limbo_ext::Value::from_float(
                            i.parse()
                                .expect("invariant: parser-validated float literal"),
                        )); // UPSTREAM (Limbo): unwrap — needs proper error propagation
                    } else {
                        vtable_args.push(limbo_ext::Value::from_integer(
                            i.parse()
                                .expect("invariant: parser-validated integer literal"),
                        )); // UPSTREAM (Limbo): unwrap — needs proper error propagation
                    }
                }
                Literal::String(s) => {
                    vtable_args.push(limbo_ext::Value::from_text(s.clone()));
                }
                Literal::Blob(b) => {
                    vtable_args.push(limbo_ext::Value::from_blob(b.as_bytes().into()));
                }
                _ => {
                    vtable_args.push(limbo_ext::Value::null());
                }
            },
            _ => vtable_args.push(limbo_ext::Value::null()),
        }
    }
    vtable_args
}

#[cfg(test)]
pub mod tests;
