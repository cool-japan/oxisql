//! Translation of `CREATE VIEW` and `DROP VIEW` statements.
//!
//! A view has no B-tree of its own: creating one writes a single `type='view'`
//! row into `sqlite_schema` (root page 0, exactly like a virtual table) and
//! re-parses just that row into the live schema via `Insn::ParseSchema`; dropping
//! one removes its `sqlite_schema` row (leaving any `INSTEAD OF` trigger rows
//! dangling, matching `DROP TABLE`'s existing behavior) and its in-memory entry.
//! View *expansion* at query time lives in `translate::planner`; view *column
//! resolution* on schema load lives in `util::parse_schema_rows`.

use limbo_sqlite3_parser::ast::{self, fmt::ToTokens, IndexedColumn, QualifiedName, Select};

use crate::schema::{Schema, Table};
use crate::translate::emitter::TransactionMode;
use crate::translate::schema::{emit_schema_entry, SchemaEntryType, SQLITE_TABLEID};
use crate::vdbe::builder::{CursorType, ProgramBuilder, ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::{CmpInsFlags, Insn};
use crate::{bail_parse_error, LimboError, Result};

/// Translate `CREATE VIEW [IF NOT EXISTS] name [(cols)] AS <select>`.
///
/// Mirrors [`crate::translate::schema::translate_create_virtual_table`]: an
/// existence/`IF NOT EXISTS` check against the shared table namespace, a single
/// `sqlite_schema` row with `rootpage = 0`, and an incremental `ParseSchema`
/// re-parse of just that row (which registers the view and infers its columns).
#[allow(clippy::too_many_arguments)]
pub fn translate_create_view(
    query_mode: QueryMode,
    temporary: bool,
    if_not_exists: bool,
    view_name: QualifiedName,
    columns: Option<Vec<IndexedColumn>>,
    select: Box<Select>,
    schema: &Schema,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    if temporary {
        bail_parse_error!("TEMPORARY view not supported yet");
    }
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: 30,
        approx_num_labels: 1,
    };
    program.extend(&opts);

    let name = view_name.name.0.clone();
    // A view shares the table namespace, so an existing table/view/vtab of the
    // same name is a conflict (SQLite: "table X already exists").
    if schema.get_table(&name).is_some() {
        if if_not_exists {
            program.epilogue(TransactionMode::Write);
            return Ok(program);
        }
        bail_parse_error!("Table {} already exists", name);
    }

    // Canonical SQL text persisted to `sqlite_schema.sql`. `IF NOT EXISTS` is
    // stripped from the stored text (matching SQLite), and the `select` body is
    // moved into the statement purely to render it.
    let stmt = ast::Stmt::CreateView {
        temporary: false,
        if_not_exists: false,
        view_name,
        columns,
        select,
    };
    let sql = stmt
        .format()
        .map_err(|e| LimboError::InternalError(e.to_string()))?;

    let schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id =
        program.alloc_cursor_id(CursorType::BTreeTable(schema_table.clone()));
    program.emit_insn(Insn::OpenWrite {
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: name.clone(),
    });

    emit_schema_entry(
        &mut program,
        sqlite_schema_cursor_id,
        SchemaEntryType::View,
        &name,
        &name,
        0, // views, like virtual tables, have no root page
        Some(sql),
    );

    program.emit_schema_change();
    let parse_schema_where_clause = format!("tbl_name = '{}' AND type != 'trigger'", name);
    program.emit_insn(Insn::ParseSchema {
        db: sqlite_schema_cursor_id,
        where_clause: Some(parse_schema_where_clause),
    });

    program.epilogue(TransactionMode::Write);
    Ok(program)
}

/// Translate `DROP VIEW [IF EXISTS] name`.
///
/// Removes the view's `sqlite_schema` row(s) (every non-`trigger` row whose
/// `tbl_name` matches) and its in-memory catalog entry. A view has no B-tree and
/// no dependent indexes, so none of `DROP TABLE`'s `Destroy`/VACUUM-remap work is
/// needed. Any `INSTEAD OF` trigger rows are deliberately left dangling, matching
/// `DROP TABLE`'s existing trigger handling.
pub fn translate_drop_view(
    query_mode: QueryMode,
    view_name: QualifiedName,
    if_exists: bool,
    schema: &Schema,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: 30,
        approx_num_labels: 4,
    };
    program.extend(&opts);

    let name = view_name.name.0.clone();
    let table = schema.get_table(&name);
    match table.as_ref().map(|t| t.as_ref()) {
        None => {
            if if_exists {
                program.epilogue(TransactionMode::Write);
                return Ok(program);
            }
            bail_parse_error!("no such view: {}", name);
        }
        Some(Table::View(_)) => {}
        // Refuse to drop a real table (or vtab) via DROP VIEW, matching SQLite.
        Some(_) => {
            bail_parse_error!("use DROP TABLE to delete table {}", name);
        }
    }

    let schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id =
        program.alloc_cursor_id(CursorType::BTreeTable(schema_table.clone()));
    program.emit_insn(Insn::OpenWrite {
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: SQLITE_TABLEID.to_string(),
    });

    // Scratch registers: the current row's tbl_name / type, the view name to
    // match, the literal "trigger" to exclude, and the rowid to delete.
    let name_and_type_reg = program.alloc_register();
    let view_name_reg = program.emit_string8_new_reg(name.clone());
    program.mark_last_insn_constant();
    let trigger_type_reg = program.emit_string8_new_reg("trigger".to_string());
    program.mark_last_insn_constant();
    let row_id_reg = program.alloc_register();

    // Delete every sqlite_schema row for this view except type='trigger' rows.
    let end_label = program.allocate_label();
    let loop_label = program.allocate_label();
    program.emit_insn(Insn::Rewind {
        cursor_id: sqlite_schema_cursor_id,
        pc_if_empty: end_label,
    });
    program.preassign_label_to_next_insn(loop_label);

    let next_label = program.allocate_label();
    // Skip rows whose tbl_name (column 2) differs from the view name.
    program.emit_column(sqlite_schema_cursor_id, 2, name_and_type_reg);
    program.emit_insn(Insn::Ne {
        lhs: name_and_type_reg,
        rhs: view_name_reg,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
    });
    // Skip trigger rows (column 0 == 'trigger').
    program.emit_column(sqlite_schema_cursor_id, 0, name_and_type_reg);
    program.emit_insn(Insn::Eq {
        lhs: name_and_type_reg,
        rhs: trigger_type_reg,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
    });
    program.emit_insn(Insn::RowId {
        cursor_id: sqlite_schema_cursor_id,
        dest: row_id_reg,
    });
    program.emit_insn(Insn::Delete {
        cursor_id: sqlite_schema_cursor_id,
    });

    program.resolve_label(next_label, program.offset());
    program.emit_insn(Insn::Next {
        cursor_id: sqlite_schema_cursor_id,
        pc_if_next: loop_label,
    });
    program.preassign_label_to_next_insn(end_label);

    program.emit_schema_change();
    // Remove the in-memory catalog entry (type-agnostic HashMap removal).
    program.emit_insn(Insn::DropTable {
        db: 0,
        _p2: 0,
        _p3: 0,
        table_name: name,
    });

    program.epilogue(TransactionMode::Write);
    Ok(program)
}
