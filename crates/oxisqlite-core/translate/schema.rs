use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;

use crate::ast;
use crate::schema::BTreeTable;
use crate::schema::Column;
use crate::schema::Schema;
use crate::schema::Table;
use crate::schema::Type;
use crate::storage::pager::CreateBTreeFlags;
use crate::translate::plan::{Plan, QueryDestination, ResultSetColumn, TableReferences};
use crate::translate::select::{prepare_select_plan, translate_select};
use crate::translate::ProgramBuilder;
use crate::translate::ProgramBuilderOpts;
use crate::translate::QueryMode;
use crate::util::PRIMARY_KEY_AUTOMATIC_INDEX_NAME_PREFIX;
use crate::vdbe::builder::{CursorType, TableRefIdCounter};
use crate::vdbe::insn::{CmpInsFlags, InsertFlags, Insn, RegisterOrLiteral};
use crate::LimboError;
use crate::SymbolTable;
use crate::{bail_parse_error, Result};

use limbo_ext::VTabKind;
use limbo_sqlite3_parser::ast::{fmt::ToTokens, CreateVirtualTable};

#[allow(clippy::too_many_arguments)]
pub fn translate_create_table(
    query_mode: QueryMode,
    tbl_name: ast::QualifiedName,
    temporary: bool,
    body: ast::CreateTableBody,
    if_not_exists: bool,
    schema: &Schema,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    // Which database this table is created in: `TEMP` -> the `temp` database
    // (materialized here, on first use), an explicit `<db>.<name>` qualifier ->
    // that database, otherwise `main`.
    let db_index = program.resolve_ddl_db_index(
        tbl_name.db_name.as_ref().map(|name| name.0.as_str()),
        temporary,
    )?;
    let db_qualifier = program.db_qualifier(db_index);
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: 30,
        approx_num_labels: 1,
    };
    program.extend(&opts);
    if schema
        .get_table_qualified(Some(&db_qualifier), tbl_name.name.0.as_str())
        .is_some()
    {
        if if_not_exists {
            program.epilogue(crate::translate::emitter::TransactionMode::Write);

            return Ok(program);
        }
        bail_parse_error!("Table {} already exists", tbl_name);
    }

    // `CREATE TABLE ... AS SELECT` has no column list, table constraints, or
    // WITHOUT ROWID/STRICT options (the grammar doesn't allow them for this
    // form), and must plan+run a SELECT before its schema text can even be
    // synthesized, so it takes an entirely separate path from here on. The
    // existence/`IF NOT EXISTS` check above already covers this form too.
    let body = match body {
        ast::CreateTableBody::AsSelect(select) => {
            return translate_create_table_as_select(
                query_mode, tbl_name, db_index, *select, schema, syms, program,
            );
        }
        body @ ast::CreateTableBody::ColumnsAndConstraints { .. } => body,
    };

    let sql = create_table_body_to_str(&tbl_name, &body)?;

    // Detect WITHOUT ROWID before any register/label allocation so we can pick
    // the right B-Tree flags.  WITHOUT ROWID tables use an index-format B-Tree
    // (the PK columns are the key; the full row is the record payload).
    let is_without_rowid = matches!(
        &body,
        ast::CreateTableBody::ColumnsAndConstraints { options, .. }
            if options.contains(ast::TableOptions::WITHOUT_ROWID)
    );

    // Validate WITHOUT ROWID constraints early (before any B-Tree allocations).
    if is_without_rowid {
        validate_without_rowid_table(&body, &tbl_name.name.0)?;
    }

    let parse_schema_label = program.allocate_label();

    // Create the table B-tree.
    // WITHOUT ROWID tables use an index-format B-Tree, so we must pass
    // `new_index()` flags — the pager initialises the root page as an
    // index-leaf page rather than a table-leaf page.
    let table_root_reg = program.alloc_register();
    let btree_flags = if is_without_rowid {
        CreateBTreeFlags::new_index()
    } else {
        CreateBTreeFlags::new_table()
    };
    program.emit_insn(Insn::CreateBtree {
        db: db_index,
        root: table_root_reg,
        flags: btree_flags,
    });

    // Create an automatic index B-tree if needed
    //
    // NOTE: we are deviating from SQLite bytecode here. For some reason, SQLite first creates a placeholder entry
    // for the table in sqlite_schema, then writes the index to sqlite_schema, then UPDATEs the table placeholder entry
    // in sqlite_schema with actual data.
    //
    // What we do instead is:
    // 1. Create the table B-tree
    // 2. Create the index B-tree
    // 3. Add the table entry to sqlite_schema
    // 4. Add the index entry to sqlite_schema
    //
    // I.e. we skip the weird song and dance with the placeholder entry. Unclear why sqlite does this.
    // The sqlite code has this comment:
    //
    // "This just creates a place-holder record in the sqlite_schema table.
    // The record created does not contain anything yet.  It will be replaced
    // by the real entry in code generated at sqlite3EndTable()."
    //
    // References:
    // https://github.com/sqlite/sqlite/blob/95f6df5b8d55e67d1e34d2bff217305a2f21b1fb/src/build.c#L1355
    // https://github.com/sqlite/sqlite/blob/95f6df5b8d55e67d1e34d2bff217305a2f21b1fb/src/build.c#L2856-L2871
    // https://github.com/sqlite/sqlite/blob/95f6df5b8d55e67d1e34d2bff217305a2f21b1fb/src/build.c#L1334C5-L1336C65

    let index_regs = check_automatic_pk_index_required(&body, &mut program, &tbl_name.name.0)?;
    if let Some(index_regs) = index_regs.as_ref() {
        if cfg!(not(feature = "index_experimental")) {
            bail_parse_error!("Constraints UNIQUE and PRIMARY KEY (unless INTEGER PRIMARY KEY) on table are not supported without indexes");
        }
        for index_reg in index_regs.clone() {
            program.emit_insn(Insn::CreateBtree {
                db: db_index,
                root: index_reg,
                flags: CreateBTreeFlags::new_index(),
            });
        }
    }

    let table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(table.clone()));
    program.emit_insn(Insn::OpenWrite {
        db: db_index,
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: tbl_name.name.0.clone(),
    });

    // Add the table entry to sqlite_schema
    emit_schema_entry(
        &mut program,
        sqlite_schema_cursor_id,
        SchemaEntryType::Table,
        &tbl_name.name.0,
        &tbl_name.name.0,
        table_root_reg,
        Some(sql),
    );

    // If we need an automatic index, add its entry to sqlite_schema
    if let Some(index_regs) = index_regs {
        for (idx, index_reg) in index_regs.into_iter().enumerate() {
            let index_name = format!(
                "{}{}_{}",
                PRIMARY_KEY_AUTOMATIC_INDEX_NAME_PREFIX,
                tbl_name.name.0,
                idx + 1
            );
            emit_schema_entry(
                &mut program,
                sqlite_schema_cursor_id,
                SchemaEntryType::Index,
                &index_name,
                &tbl_name.name.0,
                index_reg,
                None,
            );
        }
    }

    program.resolve_label(parse_schema_label, program.offset());
    program.emit_schema_change_for(db_index);
    // TODO: remove format, it sucks for performance but is convenient
    let parse_schema_where_clause =
        format!("tbl_name = '{}' AND type != 'trigger'", tbl_name.name.0);
    program.emit_insn(Insn::ParseSchema {
        db: db_index,
        where_clause: Some(parse_schema_where_clause),
    });

    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}

/// Translate `CREATE TABLE <name> AS SELECT ...`.
///
/// Unlike an ordinary `CREATE TABLE`, the new table's column list isn't
/// written in the statement — it has to be derived from the `SELECT`'s result
/// columns (names and, where possible, types). This mirrors SQLite's own
/// behavior: the table is created with an *ordinary* column-list schema
/// synthesized from the query, and it is that synthesized text — never the
/// literal `AS SELECT` — that gets persisted to `sqlite_schema`. That keeps
/// the schema-reload path (`schema::table::create_table`, invoked whenever
/// this table's persisted SQL is reparsed via `ParseSchema`, including on
/// reopen) completely ordinary: it never has to handle an `AsSelect` body for
/// a real table.
///
/// The data is populated by mirroring the coroutine-based row-pump that
/// `INSERT INTO ... SELECT` uses (see `translate::insert::translate_insert`
/// and `emit_ctas_row_pump` below) rather than inventing new machinery.
///
/// The caller (`translate_create_table`) has already checked for an existing
/// table of the same name / handled `IF NOT EXISTS`, so this function can
/// assume the table does not yet exist.
fn translate_create_table_as_select(
    query_mode: QueryMode,
    tbl_name: ast::QualifiedName,
    db_index: usize,
    select: ast::Select,
    schema: &Schema,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 2,
        approx_num_insns: 40,
        approx_num_labels: 4,
    };
    program.extend(&opts);

    // Plan the SELECT purely to discover the result set's column names and
    // inferred types. This `Plan` is discarded without emitting any bytecode
    // from it: `emit_ctas_row_pump` below re-plans (and this time emits) the
    // very same `select` via `translate_select`, exactly mirroring the
    // coroutine pattern already used by `INSERT INTO ... SELECT`. Re-planning
    // once more is cheap relative to a DDL statement and avoids having to
    // duplicate `translate_select`'s internals to salvage an already-built plan.
    let mut inspect_counter = TableRefIdCounter::new();
    let inspect_plan = prepare_select_plan(
        schema,
        select.clone(),
        syms,
        &[],
        &mut inspect_counter,
        QueryDestination::ResultRows,
    )?;
    let new_columns = columns_from_select_plan(&inspect_plan)?;
    if new_columns.is_empty() {
        bail_parse_error!("cannot create a table without any columns");
    }

    let table_name = tbl_name.name.0.clone();
    let sql = ctas_table_sql(&table_name, &new_columns);

    let table_root_reg = program.alloc_register();
    program.emit_insn(Insn::CreateBtree {
        db: db_index,
        root: table_root_reg,
        flags: CreateBTreeFlags::new_table(),
    });

    let sqlite_schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id =
        program.alloc_cursor_id(CursorType::BTreeTable(sqlite_schema_table.clone()));
    program.emit_insn(Insn::OpenWrite {
        db: db_index,
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: table_name.clone(),
    });
    emit_schema_entry(
        &mut program,
        sqlite_schema_cursor_id,
        SchemaEntryType::Table,
        &table_name,
        &table_name,
        table_root_reg,
        Some(sql),
    );

    // Build an in-memory handle for the not-yet-persisted table so the
    // row-pump below can address its columns/cursor directly. The compile-time
    // `schema` snapshot passed into this function won't see the new table
    // until the `ParseSchema` instruction executes further below — i.e. only
    // after this entire statement has already been translated — so it cannot
    // be looked up via `schema.get_table(...)` the way an ordinary
    // `INSERT INTO ... SELECT` looks up its (already-persisted) target.
    let new_table = Rc::new(BTreeTable {
        root_page: 0, // unused: the cursor below is opened via the CreateBtree register, not this field
        db_index,
        name: table_name.clone(),
        primary_key_columns: vec![],
        columns: new_columns,
        has_rowid: true,
        is_strict: false,
        unique_sets: None,
        primary_key_conflict: ast::ResolveType::Abort,
        foreign_keys: vec![],
    });

    program = emit_ctas_row_pump(
        query_mode,
        schema,
        syms,
        program,
        select,
        new_table,
        table_root_reg,
    )?;

    program.emit_schema_change_for(db_index);
    let parse_schema_where_clause = format!("tbl_name = '{}' AND type != 'trigger'", table_name);
    program.emit_insn(Insn::ParseSchema {
        db: db_index,
        where_clause: Some(parse_schema_where_clause),
    });

    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}

/// Derive the column list for a `CREATE TABLE ... AS SELECT` target table from
/// an already-prepared `SELECT` plan.
///
/// Naming and typing mirror `Statement::get_column_name` /
/// `Statement::get_column_decl_type` (see `lib.rs`), which is what a caller
/// preparing a plain `SELECT` statement sees as its result columns: an
/// explicit `AS alias` wins; otherwise a bare column reference keeps the
/// source column's name; when there is neither, the column is named after its
/// raw expression text, matching SQLite's own convention for anonymous
/// computed columns (e.g. `count(*)`).
///
/// A bare column reference also carries over the source column's declared
/// type and collation, exactly as SQLite does. Any other expression
/// (arithmetic, function calls, aggregates, literals, ...) gets no declared
/// type (`BLOB` affinity / empty `ty_str`) — the value already carries the
/// correct runtime type from evaluating the SELECT, so leaving the new column
/// with no affinity means no further coercion is applied on insert and the
/// computed value is stored exactly as produced.
fn columns_from_select_plan(plan: &Plan) -> Result<Vec<Column>> {
    let (result_columns, table_references): (&Vec<ResultSetColumn>, &TableReferences) = match plan {
        Plan::Select(select_plan) => (&select_plan.result_columns, &select_plan.table_references),
        Plan::CompoundSelect { right_most, .. } => {
            (&right_most.result_columns, &right_most.table_references)
        }
        _ => {
            return Err(LimboError::InternalError(
                "CREATE TABLE AS SELECT requires a SELECT statement".to_string(),
            ));
        }
    };

    let mut seen_names: HashSet<String> = HashSet::with_capacity(result_columns.len());
    result_columns
        .iter()
        .map(|rsc| {
            let name = rsc
                .name(table_references)
                .map(|s| s.to_string())
                .unwrap_or_else(|| rsc.expr.to_string());
            let name = crate::util::normalize_ident(&name);
            if !seen_names.insert(name.clone()) {
                bail_parse_error!("duplicate column name: {}", name);
            }
            let (ty, ty_str, collation) = match &rsc.expr {
                ast::Expr::Column {
                    table,
                    column: col_idx,
                    ..
                } => table_references
                    .find_table_by_internal_id(*table)
                    .and_then(|t| t.get_column_at(*col_idx))
                    .map(|c| (c.ty, c.ty_str.clone(), c.collation))
                    .unwrap_or((Type::Blob, String::new(), None)),
                _ => (Type::Blob, String::new(), None),
            };
            Ok(Column {
                name: Some(name),
                ty,
                ty_str,
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                unique_conflict: ast::ResolveType::Abort,
                collation,
                // A `CREATE TABLE ... AS SELECT` column always materializes a
                // one-time snapshot of the SELECT's computed value; it is
                // never a live `GENERATED ALWAYS AS (...)` column.
                is_generated: false,
            })
        })
        .collect()
}

/// Build the plain column-list `CREATE TABLE` SQL text that gets persisted to
/// `sqlite_schema` for a `CREATE TABLE ... AS SELECT`. This mirrors what
/// SQLite itself does: the literal `AS SELECT` is never persisted, only an
/// ordinary column-list form reflecting the query's result columns, so that
/// reloading the schema (`schema::table::create_table`) is completely
/// ordinary.
fn ctas_table_sql(table_name: &str, columns: &[Column]) -> String {
    let mut sql = format!("CREATE TABLE {} (", table_name);
    for (i, column) in columns.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push(' ');
        sql.push_str(column.name.as_deref().unwrap_or(""));
        if !column.ty_str.is_empty() {
            sql.push(' ');
            sql.push_str(&column.ty_str);
        }
    }
    sql.push_str(" )");
    sql
}

/// Populate a freshly created `CREATE TABLE ... AS SELECT` target by running
/// `select` through a coroutine and inserting each yielded row, mirroring the
/// multi-row `INSERT INTO ... SELECT` path in
/// `translate::insert::translate_insert` (`InitCoroutine` / `Yield` /
/// `EndCoroutine` driving a `MakeRecord` + `NewRowid` + `Insert` per row). No
/// new opcodes are introduced.
///
/// Unlike `INSERT INTO ... SELECT`, `new_table` cannot possibly be read by
/// `select` — it does not exist in `schema` yet, and only becomes visible
/// after the `ParseSchema` instruction the caller emits once this returns —
/// so there is no read/write hazard and no ephemeral temp-table indirection is
/// needed: the destination cursor is opened directly on the table's own
/// root-page register.
fn emit_ctas_row_pump(
    query_mode: QueryMode,
    schema: &Schema,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
    select: ast::Select,
    new_table: Rc<BTreeTable>,
    table_root_reg: usize,
) -> Result<ProgramBuilder> {
    let halt_label = program.allocate_label();
    let yield_reg = program.alloc_register();
    let jump_on_definition_label = program.allocate_label();
    let start_offset_label = program.allocate_label();
    program.emit_insn(Insn::InitCoroutine {
        yield_reg,
        jump_on_definition: jump_on_definition_label,
        start_offset: start_offset_label,
    });
    program.preassign_label_to_next_insn(start_offset_label);

    let query_destination = QueryDestination::CoroutineYield {
        yield_reg,
        coroutine_implementation_start: halt_label,
    };
    program.incr_nesting();
    let result = translate_select(query_mode, schema, select, syms, program, query_destination)?;
    program = result.program;
    program.decr_nesting();

    program.emit_insn(Insn::EndCoroutine { yield_reg });
    program.preassign_label_to_next_insn(jump_on_definition_label);

    let cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(new_table.clone()));
    program.emit_insn(Insn::OpenWrite {
        db: new_table.db_index,
        cursor_id,
        root_page: RegisterOrLiteral::Register(table_root_reg),
        name: new_table.name.clone(),
    });

    let loop_start_label = program.allocate_label();
    program.preassign_label_to_next_insn(loop_start_label);
    program.emit_insn(Insn::Yield {
        yield_reg,
        end_offset: halt_label,
    });

    let record_reg = program.alloc_register();
    program.emit_insn(Insn::MakeRecord {
        start_reg: yield_reg + 1,
        count: result.num_result_cols,
        dest_reg: record_reg,
        index_name: None,
    });

    let rowid_reg = program.alloc_register();
    program.emit_insn(Insn::NewRowid {
        cursor: cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });

    program.emit_insn(Insn::Insert {
        cursor: cursor_id,
        key_reg: rowid_reg,
        record_reg,
        flag: InsertFlags::new(),
        table_name: new_table.name.clone(),
    });

    program.emit_insn(Insn::Goto {
        target_pc: loop_start_label,
    });

    program.resolve_label(halt_label, program.offset());

    Ok(program)
}

#[derive(Debug, Clone, Copy)]
pub enum SchemaEntryType {
    Table,
    Index,
    View,
}

impl SchemaEntryType {
    fn as_str(&self) -> &'static str {
        match self {
            SchemaEntryType::Table => "table",
            SchemaEntryType::Index => "index",
            SchemaEntryType::View => "view",
        }
    }
}
pub const SQLITE_TABLEID: &str = "sqlite_schema";

pub fn emit_schema_entry(
    program: &mut ProgramBuilder,
    sqlite_schema_cursor_id: usize,
    entry_type: SchemaEntryType,
    name: &str,
    tbl_name: &str,
    root_page_reg: usize,
    sql: Option<String>,
) {
    emit_schema_entry_raw(
        program,
        sqlite_schema_cursor_id,
        entry_type.as_str(),
        name,
        tbl_name,
        root_page_reg,
        sql,
    )
}

/// `emit_schema_entry` with a free-form `sqlite_schema.type` string.
///
/// Exists so object kinds that live outside [`SchemaEntryType`] — currently
/// `'trigger'`, see `translate::trigger` — can write their catalog row through
/// exactly the same code path instead of duplicating it.
pub fn emit_schema_entry_raw(
    program: &mut ProgramBuilder,
    sqlite_schema_cursor_id: usize,
    entry_type: &str,
    name: &str,
    tbl_name: &str,
    root_page_reg: usize,
    sql: Option<String>,
) {
    let rowid_reg = program.alloc_register();
    program.emit_insn(Insn::NewRowid {
        cursor: sqlite_schema_cursor_id,
        rowid_reg,
        prev_largest_reg: 0,
    });

    let type_reg = program.emit_string8_new_reg(entry_type.to_string());
    program.emit_string8_new_reg(name.to_string());
    program.emit_string8_new_reg(tbl_name.to_string());

    let rootpage_reg = program.alloc_register();
    if root_page_reg == 0 {
        program.emit_insn(Insn::Integer {
            dest: rootpage_reg,
            value: 0, // virtual tables in sqlite always have rootpage=0
        });
    } else {
        program.emit_insn(Insn::Copy {
            src_reg: root_page_reg,
            dst_reg: rootpage_reg,
            amount: 1,
        });
    }

    let sql_reg = program.alloc_register();
    if let Some(sql) = sql {
        program.emit_string8(sql, sql_reg);
    } else {
        program.emit_null(sql_reg, None);
    }

    let record_reg = program.alloc_register();
    program.emit_insn(Insn::MakeRecord {
        start_reg: type_reg,
        count: 5,
        dest_reg: record_reg,
        index_name: None,
    });

    program.emit_insn(Insn::Insert {
        cursor: sqlite_schema_cursor_id,
        key_reg: rowid_reg,
        record_reg,
        flag: InsertFlags::new(),
        table_name: tbl_name.to_string(),
    });
}

/// Validate that a WITHOUT ROWID table declaration is well-formed for this engine.
///
/// Requirements:
/// 1. The table must declare an explicit PRIMARY KEY.
/// 2. For correct B-Tree key comparison, the PK column(s) must be the FIRST
///    declared column(s) (positions 0..pk_count in declaration order).  This
///    constraint comes from our storage format: the full row is the record
///    payload, and key comparison uses the first `pk_count` values of the
///    record.
fn validate_without_rowid_table(body: &ast::CreateTableBody, tbl_name: &str) -> crate::Result<()> {
    let ast::CreateTableBody::ColumnsAndConstraints {
        columns,
        constraints,
        ..
    } = body
    else {
        bail_parse_error!("WITHOUT ROWID is only valid for ColumnsAndConstraints tables");
    };

    // Collect PRIMARY KEY column names from table-level and column-level constraints.
    let mut pk_names: Vec<String> = Vec::new();

    // Table-level PRIMARY KEY clause.
    if let Some(constraints) = constraints {
        for c in constraints {
            if let ast::TableConstraint::PrimaryKey {
                columns: pk_cols, ..
            } = &c.constraint
            {
                for col in pk_cols {
                    if let ast::Expr::Id(id) = &col.expr {
                        pk_names.push(crate::util::normalize_ident(&id.0));
                    }
                }
            }
        }
    }

    // Column-level PRIMARY KEY constraint.
    for (col_name, col_def) in columns {
        for constraint in &col_def.constraints {
            if matches!(
                constraint.constraint,
                ast::ColumnConstraint::PrimaryKey { .. }
            ) {
                pk_names.push(crate::util::normalize_ident(&col_name.0));
            }
        }
    }

    if pk_names.is_empty() {
        bail_parse_error!("WITHOUT ROWID table '{}' must have a PRIMARY KEY", tbl_name);
    }

    // Verify that PK columns are the first pk_count declared columns.
    let pk_count = pk_names.len();
    for (idx, pk_name) in pk_names.iter().enumerate() {
        let col_pos = columns
            .iter()
            .position(|(k, _)| crate::util::normalize_ident(&k.0) == *pk_name);
        match col_pos {
            None => bail_parse_error!(
                "WITHOUT ROWID table '{}': PRIMARY KEY column '{}' not found",
                tbl_name,
                pk_name
            ),
            Some(pos) if pos >= pk_count => bail_parse_error!(
                "WITHOUT ROWID table '{}': PRIMARY KEY column '{}' must be declared \
                 in the first {} column position(s) (found at position {})",
                tbl_name,
                pk_name,
                pk_count,
                pos
            ),
            Some(pos) if pos != idx => bail_parse_error!(
                "WITHOUT ROWID table '{}': PRIMARY KEY column '{}' must appear \
                 in PK declaration order as one of the first {} columns \
                 (found at position {}; expected position {})",
                tbl_name,
                pk_name,
                pk_count,
                pos,
                idx
            ),
            _ => {}
        }
    }

    Ok(())
}

#[derive(Debug)]
struct PrimaryKeyColumnInfo<'a> {
    name: &'a String,
    is_descending: bool,
}

/// Check if an automatic PRIMARY KEY index is required for the table.
/// If so, create a register for the index root page and return it.
///
/// An automatic PRIMARY KEY index is not required if:
/// - The table has no PRIMARY KEY
/// - The table has a single-column PRIMARY KEY whose typename is _exactly_ "INTEGER" e.g. not "INT".
///   In this case, the PRIMARY KEY column becomes an alias for the rowid.
///
/// Otherwise, an automatic PRIMARY KEY index is required.
fn check_automatic_pk_index_required(
    body: &ast::CreateTableBody,
    program: &mut ProgramBuilder,
    tbl_name: &str,
) -> Result<Option<Range<usize>>> {
    match body {
        ast::CreateTableBody::ColumnsAndConstraints {
            columns,
            constraints,
            options,
        } => {
            let mut primary_key_definition = None;
            // Used to dedup named unique constraints
            let mut unique_sets = vec![];

            // Check table constraints for PRIMARY KEY
            if let Some(constraints) = constraints {
                for constraint in constraints {
                    if let ast::TableConstraint::PrimaryKey {
                        columns: pk_cols, ..
                    } = &constraint.constraint
                    {
                        if primary_key_definition.is_some() {
                            bail_parse_error!("table {} has more than one primary key", tbl_name);
                        }
                        let primary_key_column_results = pk_cols
                            .iter()
                            .map(|col| match &col.expr {
                                ast::Expr::Id(name) => {
                                    if !columns.iter().any(|(k, _)| k.0 == name.0) {
                                        bail_parse_error!("No such column: {}", name.0);
                                    }
                                    Ok(PrimaryKeyColumnInfo {
                                        name: &name.0,
                                        is_descending: matches!(
                                            col.order,
                                            Some(ast::SortOrder::Desc)
                                        ),
                                    })
                                }
                                _ => Err(LimboError::ParseError(
                                    "expressions prohibited in PRIMARY KEY and UNIQUE constraints"
                                        .to_string(),
                                )),
                            })
                            .collect::<Result<Vec<_>>>()?;

                        for pk_info in primary_key_column_results {
                            let column_name = pk_info.name;
                            let (_, column_def) = columns
                                .iter()
                                .find(|(k, _)| k.0 == *column_name)
                                .expect("primary key column should be in Create Body columns");

                            match &mut primary_key_definition {
                                Some(PrimaryKeyDefinitionType::Simple { column, .. }) => {
                                    let mut columns = HashSet::new();
                                    columns.insert(std::mem::take(column));
                                    // Have to also insert the current column_name we are iterating over in primary_key_column_results
                                    columns.insert(column_name.clone());
                                    primary_key_definition =
                                        Some(PrimaryKeyDefinitionType::Composite { columns });
                                }
                                Some(PrimaryKeyDefinitionType::Composite { columns }) => {
                                    columns.insert(column_name.clone());
                                }
                                None => {
                                    let typename =
                                        column_def.col_type.as_ref().map(|t| t.name.as_str());
                                    let is_descending = pk_info.is_descending;
                                    primary_key_definition =
                                        Some(PrimaryKeyDefinitionType::Simple {
                                            typename,
                                            is_descending,
                                            column: column_name.clone(),
                                        });
                                }
                            }
                        }
                    } else if let ast::TableConstraint::Unique {
                        columns: unique_columns,
                        // The `ON CONFLICT <action>` resolution is validated and
                        // stored by `schema::table::create_table` when the
                        // persisted SQL is (re)parsed into the real schema
                        // representation (see `Column::unique_conflict` /
                        // `BTreeTable::unique_sets`). This pass only needs to
                        // count how many automatic index B-trees are required,
                        // which is unaffected by the constraint's conflict
                        // resolution.
                        ..
                    } = &constraint.constraint
                    {
                        let col_names = unique_columns
                            .iter()
                            .map(|column| match &column.expr {
                                limbo_sqlite3_parser::ast::Expr::Id(id) => {
                                    if !columns.iter().any(|(k, _)| k.0 == id.0) {
                                        bail_parse_error!("No such column: {}", id.0);
                                    }
                                    Ok(crate::util::normalize_ident(&id.0))
                                }
                                _ => {
                                    bail_parse_error!(
                                        "expressions prohibited in PRIMARY KEY and UNIQUE constraints"
                                    );
                                }
                            })
                            .collect::<Result<HashSet<String>>>()?;
                        unique_sets.push(col_names);
                    }
                }
            }

            // Check column constraints for PRIMARY KEY and UNIQUE
            for (_, col_def) in columns.iter() {
                for constraint in &col_def.constraints {
                    if matches!(
                        constraint.constraint,
                        ast::ColumnConstraint::PrimaryKey { .. }
                    ) {
                        if primary_key_definition.is_some() {
                            bail_parse_error!("table {} has more than one primary key", tbl_name);
                        }
                        let typename = col_def.col_type.as_ref().map(|t| t.name.as_str());
                        primary_key_definition = Some(PrimaryKeyDefinitionType::Simple {
                            typename,
                            is_descending: false,
                            column: col_def.col_name.0.clone(),
                        });
                    } else if matches!(constraint.constraint, ast::ColumnConstraint::Unique(..)) {
                        let mut single_set = HashSet::new();
                        single_set.insert(col_def.col_name.0.clone());
                        unique_sets.push(single_set);
                    }
                }
            }

            // WITHOUT ROWID tables use an index-format B-Tree whose implicit
            // PK index IS the table itself — no separate auto-index entry is
            // needed here.  Schema-level validation (PK required, PK columns
            // first) happens in translate_create_table.
            if options.contains(ast::TableOptions::WITHOUT_ROWID) {
                return Ok(None);
            }

            unique_sets.dedup();

            // Check if we need an automatic index
            let mut pk_is_unique = false;
            let auto_index_pk = if let Some(primary_key_definition) = &primary_key_definition {
                match primary_key_definition {
                    PrimaryKeyDefinitionType::Simple {
                        typename,
                        is_descending,
                        column,
                    } => {
                        pk_is_unique = unique_sets
                            .iter()
                            .any(|set| set.len() == 1 && set.contains(column));
                        let is_integer =
                            typename.is_some_and(|t| t.eq_ignore_ascii_case("INTEGER")); // Should match on any case of INTEGER
                        !is_integer || *is_descending
                    }
                    PrimaryKeyDefinitionType::Composite { columns } => {
                        pk_is_unique = unique_sets.iter().any(|set| set == columns);
                        true
                    }
                }
            } else {
                false
            };
            let mut total_indices = unique_sets.len();
            // if pk needs and index, but we already found out we primary key is unique, we only need a single index since constraint pk == unique
            if auto_index_pk && !pk_is_unique {
                total_indices += 1;
            }

            if total_indices > 0 {
                let index_start_reg = program.alloc_registers(total_indices);
                Ok(Some(index_start_reg..index_start_reg + total_indices))
            } else {
                Ok(None)
            }
        }
        ast::CreateTableBody::AsSelect(_) => {
            bail_parse_error!("CREATE TABLE AS SELECT not supported yet")
        }
    }
}

#[derive(Debug)]
enum PrimaryKeyDefinitionType<'a> {
    Simple {
        column: String,
        typename: Option<&'a str>,
        is_descending: bool,
    },
    Composite {
        columns: HashSet<String>,
    },
}

fn create_table_body_to_str(
    tbl_name: &ast::QualifiedName,
    body: &ast::CreateTableBody,
) -> Result<String> {
    let sql = format!(
        "CREATE TABLE {} {}",
        tbl_name.name.0,
        body.format()
            .map_err(|e| LimboError::InternalError(e.to_string()))?
    );
    match body {
        ast::CreateTableBody::ColumnsAndConstraints {
            columns: _,
            constraints: _,
            options: _,
        } => {}
        // `translate_create_table` diverts to `translate_create_table_as_select`
        // before this function is ever called with an `AsSelect` body, so this
        // arm is unreachable in practice. Kept as a graceful error (rather than a
        // panic) purely as a defensive guard against a future caller change.
        ast::CreateTableBody::AsSelect(_select) => {
            return Err(LimboError::InternalError(
                "create_table_body_to_str called with an AS SELECT body".to_string(),
            ));
        }
    }
    Ok(sql)
}

/// Builds the `CREATE VIRTUAL TABLE ...` DDL text persisted into `sqlite_schema.sql`.
///
/// This intentionally does *not* embed the module's declared column list as a comment.
/// Doing so used to require instantiating the vtab module purely to read its declared
/// schema and then immediately tearing the instance back down again -- a "create-then-
/// destroy" dance run on every `CREATE VIRTUAL TABLE`, for no functional benefit, since
/// nothing ever parsed that comment back out of the persisted text. Column names are
/// resolved on demand instead: from the live `VirtualTable` the `VCreate` instruction
/// (emitted by the caller) creates via `VirtualTable::table`/`resolve_columns`, which is
/// exactly what `PRAGMA table_info`/query compilation already read (`Table::columns`),
/// and, on schema reload, `parse_schema_rows` re-derives `module_name`/args straight from
/// this same DDL text and reconnects the module lazily. This mirrors SQLite itself, which
/// never persists a virtual table's column list in `sqlite_schema.sql` either.
fn create_vtable_body_to_str(vtab: &CreateVirtualTable) -> String {
    let args = if let Some(args) = &vtab.args {
        args.iter()
            .map(|arg| arg.to_string())
            .collect::<Vec<String>>()
            .join(", ")
    } else {
        "".to_string()
    };
    let if_not_exists = if vtab.if_not_exists {
        "IF NOT EXISTS "
    } else {
        ""
    };
    format!(
        "CREATE VIRTUAL TABLE {} {} USING {}{}",
        vtab.tbl_name.name.0,
        if_not_exists,
        vtab.module_name.0,
        if args.is_empty() {
            String::new()
        } else {
            format!("({})", args)
        },
    )
}

pub fn translate_create_virtual_table(
    vtab: CreateVirtualTable,
    schema: &Schema,
    query_mode: QueryMode,
    syms: &SymbolTable,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let ast::CreateVirtualTable {
        if_not_exists,
        tbl_name,
        module_name,
        args,
    } = &vtab;

    let table_name = tbl_name.name.0.clone();
    let module_name_str = module_name.0.clone();
    let args_vec = args.clone().unwrap_or_default();
    let Some(vtab_module) = syms.vtab_modules.get(&module_name_str) else {
        bail_parse_error!("no such module: {}", module_name_str);
    };
    if !vtab_module.module_kind.eq(&VTabKind::VirtualTable) {
        bail_parse_error!("module {} is not a virtual table", module_name_str);
    };
    if schema.get_table(&table_name).is_some() {
        if *if_not_exists {
            program.epilogue(crate::translate::emitter::TransactionMode::Write);
            return Ok(program);
        }
        bail_parse_error!("Table {} already exists", tbl_name);
    }

    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 2,
        approx_num_insns: 40,
        approx_num_labels: 2,
    };
    program.extend(&opts);
    let module_name_reg = program.emit_string8_new_reg(module_name_str.clone());
    let table_name_reg = program.emit_string8_new_reg(table_name.clone());
    let args_reg = if !args_vec.is_empty() {
        let args_start = program.alloc_register();

        // Emit string8 instructions for each arg
        for (i, arg) in args_vec.iter().enumerate() {
            program.emit_string8(arg.clone(), args_start + i);
        }
        let args_record_reg = program.alloc_register();

        // VCreate expects an array of args as a record
        program.emit_insn(Insn::MakeRecord {
            start_reg: args_start,
            count: args_vec.len(),
            dest_reg: args_record_reg,
            index_name: None,
        });
        Some(args_record_reg)
    } else {
        None
    };

    program.emit_insn(Insn::VCreate {
        module_name: module_name_reg,
        table_name: table_name_reg,
        args_reg,
    });
    let table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(table.clone()));
    program.emit_insn(Insn::OpenWrite {
        db: 0,
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: table_name.clone(),
    });

    let sql = create_vtable_body_to_str(&vtab);
    emit_schema_entry(
        &mut program,
        sqlite_schema_cursor_id,
        SchemaEntryType::Table,
        &tbl_name.name.0,
        &tbl_name.name.0,
        0, // virtual tables dont have a root page
        Some(sql),
    );

    program.emit_schema_change();
    let parse_schema_where_clause = format!("tbl_name = '{}' AND type != 'trigger'", table_name);
    // Virtual tables are only ever created in `main` in this engine (the module
    // registry is connection-global, not per-database), so the re-parse targets
    // `main`'s catalog.
    program.emit_insn(Insn::ParseSchema {
        db: crate::multidb::DB_MAIN,
        where_clause: Some(parse_schema_where_clause),
    });

    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}

pub fn translate_drop_table(
    query_mode: QueryMode,
    tbl_name: ast::QualifiedName,
    if_exists: bool,
    schema: &Schema,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    #[cfg(not(feature = "index_experimental"))]
    {
        if schema.table_has_indexes(&tbl_name.name.to_string()) {
            bail_parse_error!(
                "DROP Table with indexes on the table enabled only with index_experimental feature"
            );
        }
    }
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 3,
        approx_num_insns: 40,
        approx_num_labels: 4,
    };
    program.extend(&opts);
    let qualifier = tbl_name.db_name.as_ref().map(|name| name.0.clone());
    // Validate the qualifier before looking anything up, so `DROP TABLE
    // nosuchdb.t` reports the database rather than the table.
    let _ = program.resolve_db_index(qualifier.as_deref())?;
    let table = schema.get_table_qualified(qualifier.as_deref(), tbl_name.name.0.as_str());
    if table.is_none() {
        if if_exists {
            program.epilogue(crate::translate::emitter::TransactionMode::Write);

            return Ok(program);
        }
        bail_parse_error!("No such table: {}", tbl_name.name.0.as_str());
    }

    let table = table.ok_or_else(|| LimboError::InternalError("table not found".to_string()))?;

    // Refuse to drop a view via DROP TABLE, matching SQLite's wording.
    if matches!(table.as_ref(), Table::View(_)) {
        bail_parse_error!("use DROP VIEW to delete view {}", tbl_name.name.0);
    }

    // Everything below writes to the catalog of the database that actually owns
    // the table: its `sqlite_schema`, its b-trees, its in-memory catalog.
    let db_index = table.db_index();

    let null_reg = program.alloc_register(); //  r1
    program.emit_null(null_reg, None);
    let table_name_and_root_page_register = program.alloc_register(); //  r2, this register is special because it's first used to track table name and then moved root page
    let table_reg = program.emit_string8_new_reg(tbl_name.name.0.clone()); //  r3
    program.mark_last_insn_constant();
    let table_type = program.emit_string8_new_reg("trigger".to_string()); //  r4
    program.mark_last_insn_constant();
    let row_id_reg = program.alloc_register(); //  r5

    let schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id_0 = program.alloc_cursor_id(
        //  cursor 0
        CursorType::BTreeTable(schema_table.clone()),
    );
    program.emit_insn(Insn::OpenWrite {
        db: db_index,
        cursor_id: sqlite_schema_cursor_id_0,
        root_page: 1usize.into(),
        name: SQLITE_TABLEID.to_string(),
    });

    //  1. Remove all entries from the schema table related to the table we are dropping, except for triggers
    //  loop to beginning of schema table
    let end_metadata_label = program.allocate_label();
    let metadata_loop = program.allocate_label();
    program.emit_insn(Insn::Rewind {
        cursor_id: sqlite_schema_cursor_id_0,
        pc_if_empty: end_metadata_label,
    });
    program.preassign_label_to_next_insn(metadata_loop);

    //  start loop on schema table
    program.emit_column(
        sqlite_schema_cursor_id_0,
        2,
        table_name_and_root_page_register,
    );
    let next_label = program.allocate_label();
    program.emit_insn(Insn::Ne {
        lhs: table_name_and_root_page_register,
        rhs: table_reg,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
    });
    program.emit_column(
        sqlite_schema_cursor_id_0,
        0,
        table_name_and_root_page_register,
    );
    program.emit_insn(Insn::Eq {
        lhs: table_name_and_root_page_register,
        rhs: table_type,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
    });
    program.emit_insn(Insn::RowId {
        cursor_id: sqlite_schema_cursor_id_0,
        dest: row_id_reg,
    });
    program.emit_insn(Insn::Delete {
        cursor_id: sqlite_schema_cursor_id_0,
    });

    program.resolve_label(next_label, program.offset());
    program.emit_insn(Insn::Next {
        cursor_id: sqlite_schema_cursor_id_0,
        pc_if_next: metadata_loop,
    });
    program.preassign_label_to_next_insn(end_metadata_label);
    //  end of loop on schema table

    //  1b. Drop the table's triggers with it, matching SQLite ("DROP TABLE also
    //  deletes triggers associated with the table"). The loop above deliberately
    //  skips type='trigger' rows, so they are removed here in their own pass.
    crate::translate::trigger::emit_delete_trigger_rows(
        &mut program,
        sqlite_schema_cursor_id_0,
        tbl_name.name.0.as_str(),
        crate::translate::trigger::MatchColumn::TblName,
    );

    //  2. Destroy the indices within a loop
    let indices = schema.get_indices(&tbl_name.name.0);
    for index in indices {
        program.emit_insn(Insn::Destroy {
            root: index.root_page,
            former_root_reg: 0, //  no autovacuum (https://www.sqlite.org/opcode.html#Destroy)
            is_temp: db_index,
        });

        //  3. TODO: Open an ephemeral table, and read over triggers from schema table into ephemeral table
        //  Requires support via https://github.com/tursodatabase/limbo/pull/768

        //  4. TODO: Open a write cursor to the schema table and re-insert all triggers into the sqlite schema table from the ephemeral table and delete old trigger
        //  Requires support via https://github.com/tursodatabase/limbo/pull/768
    }

    //  3. Destroy the table structure
    match table.as_ref() {
        Table::BTree(table) => {
            program.emit_insn(Insn::Destroy {
                root: table.root_page,
                former_root_reg: table_name_and_root_page_register,
                is_temp: db_index,
            });
        }
        Table::Virtual(vtab) => {
            // From what I see, TableValuedFunction is not stored in the schema as a table.
            // But this line here below is a safeguard in case this behavior changes in the future
            // And mirrors what SQLite does.
            if matches!(vtab.kind, limbo_ext::VTabKind::TableValuedFunction) {
                return Err(crate::LimboError::ParseError(format!(
                    "table {} may not be dropped",
                    vtab.name
                )));
            }
            program.emit_insn(Insn::VDestroy {
                table_name: vtab.name.clone(),
                db: 0, // TODO change this for multiple databases
            });
        }
        Table::Pseudo(..) => {
            return Err(crate::LimboError::InternalError(
                "Pseudo table cannot be dropped".to_string(),
            ));
        }
        Table::FromClauseSubquery(..) => {
            return Err(crate::LimboError::InternalError(
                "FromClauseSubquery cannot be dropped".to_string(),
            ));
        }
        // A view has no B-tree to destroy. Reaching here with a view means the
        // early guard in `translate_drop_table` was bypassed; do nothing rather
        // than emit a bogus `Destroy`.
        Table::View(..) => {}
    };

    let schema_data_register = program.alloc_register();
    let schema_row_id_register = program.alloc_register();
    program.emit_null(schema_data_register, Some(schema_row_id_register));

    //  All of the following processing needs to be done only if the table is not a virtual table
    if table.btree().is_some() {
        //  4. Open an ephemeral table, and read over the entry from the schema table whose root page was moved in the destroy operation

        //  cursor id 1
        let sqlite_schema_cursor_id_1 =
            program.alloc_cursor_id(CursorType::BTreeTable(schema_table.clone()));
        let simple_table_rc = Rc::new(BTreeTable {
            root_page: 0, // Not relevant for ephemeral table definition
            db_index: 0,  // ephemeral b-trees always live in the main pager
            name: "ephemeral_scratch".to_string(),
            has_rowid: true,
            primary_key_columns: vec![],
            columns: vec![Column {
                name: Some("rowid".to_string()),
                ty: Type::Integer,
                ty_str: "INTEGER".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                unique_conflict: ast::ResolveType::Abort,
                collation: None,
                is_generated: false,
            }],
            is_strict: false,
            unique_sets: None,
            primary_key_conflict: ast::ResolveType::Abort,
            foreign_keys: vec![],
        });
        //  cursor id 2
        let ephemeral_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(simple_table_rc));
        program.emit_insn(Insn::OpenEphemeral {
            cursor_id: ephemeral_cursor_id,
            is_table: true,
        });
        let if_not_label = program.allocate_label();
        program.emit_insn(Insn::IfNot {
            reg: table_name_and_root_page_register,
            target_pc: if_not_label,
            jump_if_null: true, //  jump anyway
        });
        program.emit_insn(Insn::OpenRead {
            db: db_index,
            cursor_id: sqlite_schema_cursor_id_1,
            root_page: 1usize.into(),
        });

        let schema_column_0_register = program.alloc_register();
        let schema_column_1_register = program.alloc_register();
        let schema_column_2_register = program.alloc_register();
        let moved_to_root_page_register = program.alloc_register(); //  the register that will contain the root page number the last root page is moved to
        let schema_column_4_register = program.alloc_register();
        let prev_root_page_register = program.alloc_register(); //  the register that will contain the root page number that the last root page was on before VACUUM
        let _r14 = program.alloc_register(); //  Unsure why this register is allocated but putting it in here to make comparison with SQLite easier
        let new_record_register = program.alloc_register();

        //  Loop to copy over row id's from the schema table for rows that have the same root page as the one that was moved
        let copy_schema_to_temp_table_loop_end_label = program.allocate_label();
        let copy_schema_to_temp_table_loop = program.allocate_label();
        program.emit_insn(Insn::Rewind {
            cursor_id: sqlite_schema_cursor_id_1,
            pc_if_empty: copy_schema_to_temp_table_loop_end_label,
        });
        program.preassign_label_to_next_insn(copy_schema_to_temp_table_loop);
        //  start loop on schema table
        program.emit_column(sqlite_schema_cursor_id_1, 3, prev_root_page_register);
        //  The label and Insn::Ne are used to skip over any rows in the schema table that don't have the root page that was moved
        let next_label = program.allocate_label();
        program.emit_insn(Insn::Ne {
            lhs: prev_root_page_register,
            rhs: table_name_and_root_page_register,
            target_pc: next_label,
            flags: CmpInsFlags::default(),
            collation: program.curr_collation(),
        });
        program.emit_insn(Insn::RowId {
            cursor_id: sqlite_schema_cursor_id_1,
            dest: schema_row_id_register,
        });
        program.emit_insn(Insn::Insert {
            cursor: ephemeral_cursor_id,
            key_reg: schema_row_id_register,
            record_reg: schema_data_register,
            flag: InsertFlags::new(),
            table_name: "scratch_table".to_string(),
        });

        program.resolve_label(next_label, program.offset());
        program.emit_insn(Insn::Next {
            cursor_id: sqlite_schema_cursor_id_1,
            pc_if_next: copy_schema_to_temp_table_loop,
        });
        program.preassign_label_to_next_insn(copy_schema_to_temp_table_loop_end_label);
        //  End loop to copy over row id's from the schema table for rows that have the same root page as the one that was moved

        program.resolve_label(if_not_label, program.offset());

        //  5. Open a write cursor to the schema table and re-insert the records placed in the ephemeral table but insert the correct root page now
        program.emit_insn(Insn::OpenWrite {
            db: db_index,
            cursor_id: sqlite_schema_cursor_id_1,
            root_page: 1usize.into(),
            name: SQLITE_TABLEID.to_string(),
        });

        //  Loop to copy over row id's from the ephemeral table and then re-insert into the schema table with the correct root page
        let copy_temp_table_to_schema_loop_end_label = program.allocate_label();
        let copy_temp_table_to_schema_loop = program.allocate_label();
        program.emit_insn(Insn::Rewind {
            cursor_id: ephemeral_cursor_id,
            pc_if_empty: copy_temp_table_to_schema_loop_end_label,
        });
        program.preassign_label_to_next_insn(copy_temp_table_to_schema_loop);
        //  start loop on schema table
        program.emit_insn(Insn::RowId {
            cursor_id: ephemeral_cursor_id,
            dest: schema_row_id_register,
        });
        //  the next_label and Insn::NotExists are used to skip patching any rows in the schema table that don't have the row id that was written to the ephemeral table
        let next_label = program.allocate_label();
        program.emit_insn(Insn::NotExists {
            cursor: sqlite_schema_cursor_id_1,
            rowid_reg: schema_row_id_register,
            target_pc: next_label,
        });
        program.emit_column(sqlite_schema_cursor_id_1, 0, schema_column_0_register);
        program.emit_column(sqlite_schema_cursor_id_1, 1, schema_column_1_register);
        program.emit_column(sqlite_schema_cursor_id_1, 2, schema_column_2_register);
        let root_page: i64 = table.get_root_page()?.try_into().map_err(|_| {
            crate::LimboError::InternalError("root page does not fit in i64".to_string())
        })?;
        program.emit_insn(Insn::Integer {
            value: root_page,
            dest: moved_to_root_page_register,
        });
        program.emit_column(sqlite_schema_cursor_id_1, 4, schema_column_4_register);
        program.emit_insn(Insn::MakeRecord {
            start_reg: schema_column_0_register,
            count: 5,
            dest_reg: new_record_register,
            index_name: None,
        });
        program.emit_insn(Insn::Delete {
            cursor_id: sqlite_schema_cursor_id_1,
        });
        program.emit_insn(Insn::Insert {
            cursor: sqlite_schema_cursor_id_1,
            key_reg: schema_row_id_register,
            record_reg: new_record_register,
            flag: InsertFlags::new(),
            table_name: SQLITE_TABLEID.to_string(),
        });

        program.resolve_label(next_label, program.offset());
        program.emit_insn(Insn::Next {
            cursor_id: ephemeral_cursor_id,
            pc_if_next: copy_temp_table_to_schema_loop,
        });
        program.preassign_label_to_next_insn(copy_temp_table_to_schema_loop_end_label);
        //  End loop to copy over row id's from the ephemeral table and then re-insert into the schema table with the correct root page
    }

    program.emit_schema_change_for(db_index);
    //  Drop the in-memory structures for the table
    program.emit_insn(Insn::DropTriggersForTable {
        table_name: crate::util::normalize_ident(tbl_name.name.0.as_str()),
    });
    program.emit_insn(Insn::DropTable {
        db: db_index,
        _p2: 0,
        _p3: 0,
        table_name: tbl_name.name.0,
    });

    //  end of the program
    program.epilogue(super::emitter::TransactionMode::Write);

    Ok(program)
}
