use crate::schema::Table;
use crate::translate::emitter::emit_program;
use crate::translate::optimizer::optimize_plan;
use crate::translate::plan::{DeletePlan, Operation, Plan};
use crate::translate::planner::{parse_limit_full, parse_where, LimitValue};
use crate::vdbe::builder::{ProgramBuilder, ProgramBuilderOpts, QueryMode, TableRefIdCounter};
use crate::{schema::Schema, Result, SymbolTable};
use limbo_sqlite3_parser::ast::{Expr, Limit, QualifiedName};

use super::plan::{ColumnUsedMask, IterationDirection, JoinedTable, TableReferences};

pub fn translate_delete(
    query_mode: QueryMode,
    schema: &Schema,
    tbl_name: &QualifiedName,
    where_clause: Option<Box<Expr>>,
    limit: Option<Box<Limit>>,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    #[cfg(not(feature = "index_experimental"))]
    {
        if schema.table_has_indexes(&tbl_name.name.to_string()) {
            // Let's disable altering a table with indices altogether instead of checking column by
            // column to be extra safe.
            crate::bail_parse_error!(
                "DELETE into table disabled for table with indexes and without index_experimental feature flag"
            );
        }
    }
    let mut delete_plan = prepare_delete_plan(
        schema,
        tbl_name,
        where_clause,
        limit,
        &mut program.table_reference_counter,
    )?;
    optimize_plan(&mut delete_plan, schema)?;
    let Plan::Delete(ref delete) = delete_plan else {
        panic!("delete_plan is not a DeletePlan");
    };
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: estimate_num_instructions(delete),
        approx_num_labels: 0,
    };
    program.extend(&opts);
    emit_program(&mut program, delete_plan, schema, syms, |_| {})?;
    Ok(program)
}

pub fn prepare_delete_plan(
    schema: &Schema,
    tbl_name: &QualifiedName,
    where_clause: Option<Box<Expr>>,
    limit: Option<Box<Limit>>,
    table_ref_counter: &mut TableRefIdCounter,
) -> Result<Plan> {
    let table = match schema.get_table(tbl_name.name.0.as_str()) {
        Some(table) => table,
        None => crate::bail_parse_error!("no such table: {}", tbl_name),
    };
    if matches!(table.as_ref(), Table::View(_)) {
        crate::bail_parse_error!("cannot modify {} because it is a view", tbl_name.name.0);
    }
    let table = if let Some(table) = table.virtual_table() {
        Table::Virtual(table.clone())
    } else if let Some(table) = table.btree() {
        Table::BTree(table.clone())
    } else {
        crate::bail_parse_error!("Table is neither a virtual table nor a btree table");
    };
    let name = tbl_name.name.0.as_str().to_string();
    let indexes = schema
        .get_indices(table.get_name())
        .iter()
        .cloned()
        .collect();
    let joined_tables = vec![JoinedTable {
        table,
        identifier: name,
        internal_id: table_ref_counter.next(),
        op: Operation::Scan {
            iter_dir: IterationDirection::Forwards,
            index: None,
        },
        join_info: None,
        col_used_mask: ColumnUsedMask::new(),
    }];
    let mut table_references = TableReferences::new(joined_tables, vec![]);

    let mut where_predicates = vec![];

    // Parse the WHERE clause
    parse_where(
        where_clause.map(|e| *e),
        &mut table_references,
        None,
        &mut where_predicates,
    )?;

    // Parse the LIMIT/OFFSET clause
    let (limit_val, offset_val) = limit.map_or(Ok((LimitValue::None, LimitValue::None)), |l| {
        parse_limit_full(&l)
    })?;
    let resolved_limit = match &limit_val {
        LimitValue::Literal(n) => Some(*n),
        _ => None,
    };
    let resolved_offset = match &offset_val {
        LimitValue::Literal(n) => Some(*n),
        _ => None,
    };
    let limit_expr = match limit_val {
        LimitValue::Expr(e) => Some(e),
        _ => None,
    };
    let offset_expr = match offset_val {
        LimitValue::Expr(e) => Some(e),
        _ => None,
    };

    let plan = DeletePlan {
        table_references,
        result_columns: vec![],
        where_clause: where_predicates,
        order_by: None,
        limit: resolved_limit,
        offset: resolved_offset,
        limit_expr,
        offset_expr,
        contains_constant_false_condition: false,
        indexes,
    };

    Ok(Plan::Delete(plan))
}

fn estimate_num_instructions(plan: &DeletePlan) -> usize {
    let base = 20;

    base + plan.table_references.joined_tables().len() * 10
}
