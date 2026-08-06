//! Translation of `ANALYZE` into bytecode.
//!
//! `ANALYZE` gathers cardinality statistics for tables and indexes and stores
//! them in the `sqlite_stat1` table (`CREATE TABLE sqlite_stat1(tbl,idx,stat)`).
//!
//! The generated program:
//! 1. creates `sqlite_stat1` if it does not yet exist,
//! 2. clears the prior rows relevant to the requested target(s),
//! 3. walks each target b-tree (via the [`Insn::IdxStat`] opcode) and inserts a
//!    fresh `(tbl, idx, stat)` row — skipping empty tables/indexes,
//! 4. bumps the schema cookie and re-parses the schema so the issuing
//!    connection picks up the new table and statistics.

use std::rc::Rc;
use std::sync::Arc;

use crate::schema::{BTreeTable, Index, Schema};
use crate::storage::pager::CreateBTreeFlags;
use crate::translate::emitter::TransactionMode;
use crate::translate::schema::{emit_schema_entry, SchemaEntryType, SQLITE_TABLEID};
use crate::vdbe::builder::{CursorType, ProgramBuilder, ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::{CmpInsFlags, InsertFlags, Insn, RegisterOrLiteral};
use crate::{bail_parse_error, LimboError, Result};

use limbo_sqlite3_parser::ast::QualifiedName;

/// Name of the statistics table.
const STAT1_TABLE: &str = "sqlite_stat1";
/// Canonical schema for the statistics table.
const STAT1_SQL: &str = "CREATE TABLE sqlite_stat1(tbl,idx,stat)";

/// A single object to analyze.
enum AnalyzeTarget {
    /// A table b-tree (produces a `(tbl, NULL, "N")` row).
    Table(Rc<BTreeTable>),
    /// An index b-tree (produces a `(tbl, idx, "N a1 … ak")` row).
    Index(Arc<Index>),
}

/// Which prior `sqlite_stat1` rows to remove before inserting fresh ones.
enum ClearMode {
    /// Remove every row (bare `ANALYZE` / `ANALYZE main`).
    All,
    /// Remove rows whose `tbl` column matches (named-table `ANALYZE`).
    Table(String),
    /// Remove rows whose `idx` column matches (named-index `ANALYZE`).
    Index(String),
}

/// Translate an `ANALYZE [name]` statement.
pub fn translate_analyze(
    query_mode: QueryMode,
    schema: &Schema,
    name: Option<QualifiedName>,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    program.extend(&ProgramBuilderOpts {
        query_mode,
        num_cursors: 8,
        approx_num_insns: 64,
        approx_num_labels: 8,
    });

    let (targets, clear_mode) = resolve_targets(schema, name.as_ref())?;

    // Ensure sqlite_stat1 exists. If it does, remember its root page so we can
    // open it by literal; otherwise create it and remember the register that
    // will hold its freshly-allocated root page.
    let stat1_root_page = schema.get_btree_table(STAT1_TABLE).map(|t| t.root_page);
    let mut stat1_root_reg: Option<usize> = None;

    if stat1_root_page.is_none() {
        let root_reg = program.alloc_register();
        program.emit_insn(Insn::CreateBtree {
            db: 0,
            root: root_reg,
            flags: CreateBTreeFlags::new_table(),
        });

        let schema_table = schema.get_btree_table(SQLITE_TABLEID).ok_or_else(|| {
            LimboError::InternalError("sqlite_schema table missing from schema".to_string())
        })?;
        let schema_cursor = program.alloc_cursor_id(CursorType::BTreeTable(schema_table));
        program.emit_insn(Insn::OpenWrite {
            db: 0,
            cursor_id: schema_cursor,
            root_page: 1usize.into(),
            name: SQLITE_TABLEID.to_string(),
        });
        emit_schema_entry(
            &mut program,
            schema_cursor,
            SchemaEntryType::Table,
            STAT1_TABLE,
            STAT1_TABLE,
            root_reg,
            Some(STAT1_SQL.to_string()),
        );
        stat1_root_reg = Some(root_reg);
    }

    // Open sqlite_stat1 for writing. When it already existed we have its real
    // BTreeTable definition; when freshly created we synthesize one (its root
    // page field is irrelevant — OpenWrite takes the page from the register).
    let stat1_btree = match schema.get_btree_table(STAT1_TABLE) {
        Some(table) => table,
        None => Rc::new(BTreeTable::from_sql(STAT1_SQL, 0)?),
    };
    let stat1_cursor = program.alloc_cursor_id(CursorType::BTreeTable(stat1_btree));
    let stat1_root: RegisterOrLiteral<usize> = match (stat1_root_page, stat1_root_reg) {
        (Some(page), _) => RegisterOrLiteral::Literal(page),
        (None, Some(reg)) => RegisterOrLiteral::Register(reg),
        (None, None) => {
            return Err(LimboError::InternalError(
                "sqlite_stat1 root page could not be resolved".to_string(),
            ))
        }
    };
    program.emit_insn(Insn::OpenWrite {
        db: 0,
        cursor_id: stat1_cursor,
        root_page: stat1_root,
        name: STAT1_TABLE.to_string(),
    });

    emit_clear_loop(&mut program, stat1_cursor, &clear_mode);

    for target in &targets {
        emit_target_row(&mut program, stat1_cursor, target);
    }

    // Bump the schema cookie and reload so the connection sees sqlite_stat1.
    program.emit_schema_change();
    program.emit_insn(Insn::ParseSchema {
        db: 0,
        where_clause: None,
    });
    program.epilogue(TransactionMode::Write);

    Ok(program)
}

/// Resolve the requested target name into the concrete list of objects to
/// analyze and the corresponding [`ClearMode`].
fn resolve_targets(
    schema: &Schema,
    name: Option<&QualifiedName>,
) -> Result<(Vec<AnalyzeTarget>, ClearMode)> {
    match name {
        None => Ok((all_user_tables(schema), ClearMode::All)),
        Some(qn) => {
            let obj = qn.name.0.as_str();
            match &qn.db_name {
                Some(db) => {
                    // `ANALYZE <db>.<obj>` — the database qualifier must name an
                    // attached database (only main/temp exist here).
                    let db_lower = db.0.to_ascii_lowercase();
                    if db_lower != "main" && db_lower != "temp" {
                        bail_parse_error!("no such database: {}", db.0);
                    }
                    resolve_named(schema, obj)
                }
                None => {
                    // A bare name may be a whole-database request (main/temp) or
                    // a table/index name.
                    if obj.eq_ignore_ascii_case("main") || obj.eq_ignore_ascii_case("temp") {
                        Ok((all_user_tables(schema), ClearMode::All))
                    } else {
                        resolve_named(schema, obj)
                    }
                }
            }
        }
    }
}

/// All analyzable user tables (and, when enabled, their indexes), skipping
/// internal `sqlite_*` tables, views, and virtual tables.
fn all_user_tables(schema: &Schema) -> Vec<AnalyzeTarget> {
    let mut keys: Vec<&String> = schema.tables.keys().collect();
    keys.sort();

    let mut targets = Vec::new();
    for key in keys {
        if key.starts_with("sqlite_") {
            continue;
        }
        let Some(table) = schema.tables.get(key) else {
            continue;
        };
        let Some(btree) = table.btree() else {
            // Views / virtual tables have no countable b-tree.
            continue;
        };
        targets.push(AnalyzeTarget::Table(btree.clone()));
        for index in schema.get_indices(&btree.name) {
            targets.push(AnalyzeTarget::Index(index.clone()));
        }
    }
    targets
}

/// Resolve a named object (table or index) into targets + clear mode.
fn resolve_named(schema: &Schema, obj: &str) -> Result<(Vec<AnalyzeTarget>, ClearMode)> {
    if let Some(table) = schema.get_table(obj) {
        if let Some(btree) = table.btree() {
            let mut targets = vec![AnalyzeTarget::Table(btree.clone())];
            for index in schema.get_indices(&btree.name) {
                targets.push(AnalyzeTarget::Index(index.clone()));
            }
            return Ok((targets, ClearMode::Table(btree.name.clone())));
        }
        // Non-btree object (e.g. virtual table): nothing to count, but still
        // drop any stale rows recorded under its name.
        return Ok((Vec::new(), ClearMode::Table(table.get_name().to_string())));
    }

    if let Some(index) = find_index_by_name(schema, obj) {
        let clear = ClearMode::Index(index.name.clone());
        return Ok((vec![AnalyzeTarget::Index(index)], clear));
    }

    bail_parse_error!("no such table: {}", obj);
}

/// Find a registered index by (case-insensitive) name across all tables.
fn find_index_by_name(schema: &Schema, name: &str) -> Option<Arc<Index>> {
    schema
        .indexes
        .values()
        .flatten()
        .find(|index| index.name.eq_ignore_ascii_case(name))
        .cloned()
}

/// Emit a `Rewind → [filter] → Delete → Next` loop that clears the relevant
/// prior rows of `sqlite_stat1`.
fn emit_clear_loop(program: &mut ProgramBuilder, stat1_cursor: usize, clear_mode: &ClearMode) {
    // (column to test, register holding the match value, jump-if-null?)
    let filter = match clear_mode {
        ClearMode::All => None,
        ClearMode::Table(tbl) => Some((0usize, program.emit_string8_new_reg(tbl.clone()), false)),
        ClearMode::Index(idx) => Some((1usize, program.emit_string8_new_reg(idx.clone()), true)),
    };

    let end_label = program.allocate_label();
    let loop_label = program.allocate_label();
    program.emit_insn(Insn::Rewind {
        cursor_id: stat1_cursor,
        pc_if_empty: end_label,
    });
    program.preassign_label_to_next_insn(loop_label);

    let skip_label = match filter {
        None => None,
        Some((column, filter_reg, jump_if_null)) => {
            let cmp_reg = program.alloc_register();
            let skip_label = program.allocate_label();
            program.emit_column(stat1_cursor, column, cmp_reg);
            let mut flags = CmpInsFlags::default();
            if jump_if_null {
                // Table rows have a NULL `idx`; treat NULL as "does not match"
                // so they are preserved when clearing a single index.
                flags = flags.jump_if_null();
            }
            program.emit_insn(Insn::Ne {
                lhs: cmp_reg,
                rhs: filter_reg,
                target_pc: skip_label,
                flags,
                collation: program.curr_collation(),
            });
            Some(skip_label)
        }
    };

    program.emit_insn(Insn::Delete {
        cursor_id: stat1_cursor,
    });
    if let Some(skip_label) = skip_label {
        program.resolve_label(skip_label, program.offset());
    }
    program.emit_insn(Insn::Next {
        cursor_id: stat1_cursor,
        pc_if_next: loop_label,
    });
    program.preassign_label_to_next_insn(end_label);
}

/// Open a target b-tree, compute its `sqlite_stat1` row, and insert it (unless
/// the table/index is empty, in which case the row is skipped).
fn emit_target_row(program: &mut ProgramBuilder, stat1_cursor: usize, target: &AnalyzeTarget) {
    let (tbl_name, idx_name, root_page, num_cols, cursor_type) = match target {
        AnalyzeTarget::Table(btree) => (
            btree.name.clone(),
            None,
            btree.root_page,
            0usize,
            CursorType::BTreeTable(btree.clone()),
        ),
        AnalyzeTarget::Index(index) => (
            index.table_name.clone(),
            Some(index.name.clone()),
            index.root_page,
            index.columns.len(),
            CursorType::BTreeIndex(index.clone()),
        ),
    };

    let read_cursor = program.alloc_cursor_id(cursor_type);
    program.emit_insn(Insn::OpenRead {
        db: 0,
        cursor_id: read_cursor,
        root_page,
    });

    // sqlite_stat1 column order is (tbl, idx, stat); allocate the three holding
    // registers contiguously so MakeRecord can pack them in one shot.
    let tbl_reg = program.alloc_register();
    let idx_reg = program.alloc_register();
    let stat_reg = program.alloc_register();

    program.emit_insn(Insn::IdxStat {
        cursor_id: read_cursor,
        num_cols,
        dest: stat_reg,
    });
    program.emit_string8(tbl_name, tbl_reg);
    match idx_name {
        Some(idx) => program.emit_string8(idx, idx_reg),
        None => program.emit_null(idx_reg, None),
    }

    // IdxStat writes NULL for an empty table/index — skip the insert in that case.
    let skip_label = program.allocate_label();
    program.emit_insn(Insn::IsNull {
        reg: stat_reg,
        target_pc: skip_label,
    });

    let rowid_reg = program.alloc_register();
    program.emit_insn(Insn::NewRowid {
        cursor: stat1_cursor,
        rowid_reg,
        prev_largest_reg: 0,
    });
    let record_reg = program.alloc_register();
    program.emit_insn(Insn::MakeRecord {
        start_reg: tbl_reg,
        count: 3,
        dest_reg: record_reg,
        index_name: None,
    });
    program.emit_insn(Insn::Insert {
        cursor: stat1_cursor,
        key_reg: rowid_reg,
        record_reg,
        flag: InsertFlags::new(),
        table_name: STAT1_TABLE.to_string(),
    });
    program.resolve_label(skip_label, program.offset());
}
