//! `CREATE TRIGGER` / `DROP TRIGGER` translation, and row-trigger firing.
//!
//! A trigger owns no B-tree. Creating one writes a single `type='trigger'` row
//! into `sqlite_schema` (`rootpage = 0`, `tbl_name` = the watched table) and
//! re-parses just that row into the live catalog via `Insn::ParseSchema`,
//! exactly like `CREATE VIEW`. Dropping one removes that row and the catalog
//! entry. `DROP TABLE` drops the table's triggers with it.
//!
//! Firing lives in [`fire`]; the `OLD`/`NEW` register rewrite lives in
//! [`rewrite`]; `RAISE()` lives in [`raise`]. Read [`fire`]'s module docs for
//! the inlining strategy and the deliberate deviations from upstream SQLite.

pub mod fire;
pub mod raise;
pub mod rewrite;

use limbo_sqlite3_parser::ast::{self, fmt::ToTokens, QualifiedName, TriggerTime};

use crate::schema::{Schema, Table, Trigger};
use crate::translate::emitter::TransactionMode;
use crate::translate::schema::SQLITE_TABLEID;
use crate::util::normalize_ident;
use crate::vdbe::builder::{CursorType, ProgramBuilder, ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::{CmpInsFlags, Insn};
use crate::{bail_parse_error, LimboError, Result};

pub use fire::{emit_triggers, table_has_triggers, TriggerEventKind, TriggerFireArgs};
pub use rewrite::RowImage;

/// `sqlite_schema.type` value for a trigger row.
const TRIGGER_SCHEMA_TYPE: &str = "trigger";

/// Translate `CREATE TRIGGER [IF NOT EXISTS] name ... BEGIN ... END`.
pub fn translate_create_trigger(
    query_mode: QueryMode,
    create: ast::CreateTrigger,
    schema: &Schema,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 1,
        approx_num_insns: 30,
        approx_num_labels: 1,
    };
    program.extend(&opts);

    // A TEMP trigger belongs to the per-connection `temp` catalog: its
    // `sqlite_schema` row is written to `temp`, it is invisible to other
    // connections, and it disappears when the connection closes. It may still
    // fire on -- and write to -- `main` tables, which is why the target table is
    // resolved through the merged catalog below rather than within `temp`.
    let db_index = program.resolve_ddl_db_index(
        create.trigger_name.db_name.as_ref().map(|n| n.0.as_str()),
        create.temporary,
    )?;

    let trigger_name = normalize_ident(create.trigger_name.name.0.as_str());
    let table_name = normalize_ident(create.tbl_name.name.0.as_str());

    if schema.trigger_exists(&trigger_name) {
        if create.if_not_exists {
            program.epilogue(TransactionMode::Write);
            return Ok(program);
        }
        bail_parse_error!("trigger {} already exists", trigger_name);
    }

    let Some(target) = schema.get_table(&table_name) else {
        bail_parse_error!("no such table: {}", table_name);
    };
    let time = create.time.unwrap_or(TriggerTime::Before);
    match (target.as_ref(), time) {
        (Table::View(_), TriggerTime::InsteadOf) => {}
        (Table::View(_), _) => {
            bail_parse_error!(
                "cannot create {} trigger on view: {}",
                trigger_time_name(time),
                table_name
            )
        }
        (_, TriggerTime::InsteadOf) => {
            bail_parse_error!("cannot create INSTEAD OF trigger on table: {}", table_name)
        }
        (Table::BTree(_), _) => {}
        (_, _) => bail_parse_error!(
            "cannot create trigger on {}: only ordinary tables and views can have triggers",
            table_name
        ),
    }

    // Canonical text persisted to `sqlite_schema.sql`, with `IF NOT EXISTS`
    // stripped, matching SQLite and matching `CREATE VIEW` in this engine.
    let stored = ast::CreateTrigger {
        // The `TEMP` keyword is not persisted: which catalog the row lives in is
        // already decided by *which* `sqlite_schema` it is written to, exactly
        // as upstream does for `sqlite_temp_schema`.
        temporary: false,
        if_not_exists: false,
        ..create.clone()
    };
    let sql = ast::Stmt::CreateTrigger(Box::new(stored))
        .format()
        .map_err(|e| LimboError::InternalError(e.to_string()))?;

    // Reject a body that could never run *now*, rather than at first fire:
    // this is where SQLite reports `no such column: NEW.x` too.
    let parsed = Trigger::from_ast(&create, &sql)?;
    validate_trigger_body(&parsed, target.as_ref())?;

    let schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(schema_table));
    program.emit_insn(Insn::OpenWrite {
        db: db_index,
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: SQLITE_TABLEID.to_string(),
    });

    crate::translate::schema::emit_schema_entry_raw(
        &mut program,
        sqlite_schema_cursor_id,
        TRIGGER_SCHEMA_TYPE,
        &trigger_name,
        &table_name,
        0,
        Some(sql),
    );

    program.emit_schema_change_for(db_index);
    program.emit_insn(Insn::ParseSchema {
        db: db_index,
        where_clause: Some(format!(
            "type = 'trigger' AND name = '{}'",
            escape_sql_literal(&trigger_name)
        )),
    });

    program.epilogue(TransactionMode::Write);
    Ok(program)
}

/// Translate `DROP TRIGGER [IF EXISTS] name`.
pub fn translate_drop_trigger(
    query_mode: QueryMode,
    trigger_name: QualifiedName,
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

    let name = normalize_ident(trigger_name.name.0.as_str());
    let qualifier = trigger_name.db_name.as_ref().map(|db| db.0.clone());
    let _ = program.resolve_db_index(qualifier.as_deref())?;
    // The trigger's `sqlite_schema` row lives in whichever catalog owns it.
    let db_index = schema.db_index_for_object(qualifier.as_deref(), &name);
    if !schema.trigger_exists(&name) {
        if if_exists {
            program.epilogue(TransactionMode::Write);
            return Ok(program);
        }
        bail_parse_error!("no such trigger: {}", name);
    }

    let schema_table = schema
        .get_btree_table(SQLITE_TABLEID)
        .ok_or_else(|| LimboError::InternalError("sqlite_schema table not found".to_string()))?;
    let sqlite_schema_cursor_id = program.alloc_cursor_id(CursorType::BTreeTable(schema_table));
    program.emit_insn(Insn::OpenWrite {
        db: db_index,
        cursor_id: sqlite_schema_cursor_id,
        root_page: 1usize.into(),
        name: SQLITE_TABLEID.to_string(),
    });

    emit_delete_trigger_rows(
        &mut program,
        sqlite_schema_cursor_id,
        &name,
        MatchColumn::Name,
    );

    program.emit_schema_change_for(db_index);
    program.emit_insn(Insn::DropTrigger { trigger_name: name });

    program.epilogue(TransactionMode::Write);
    Ok(program)
}

/// Which `sqlite_schema` column to match trigger rows on.
#[derive(Clone, Copy)]
pub enum MatchColumn {
    /// `name` — one specific trigger (`DROP TRIGGER`).
    Name,
    /// `tbl_name` — every trigger of a table (`DROP TABLE`).
    TblName,
}

/// Emit a scan of `sqlite_schema` deleting every `type='trigger'` row whose
/// `name` (or `tbl_name`) equals `value`.
pub fn emit_delete_trigger_rows(
    program: &mut ProgramBuilder,
    sqlite_schema_cursor_id: usize,
    value: &str,
    match_column: MatchColumn,
) {
    let column_index = match match_column {
        MatchColumn::Name => 1usize,
        MatchColumn::TblName => 2usize,
    };
    let scratch_reg = program.alloc_register();
    let wanted_reg = program.emit_string8_new_reg(value.to_string());
    program.mark_last_insn_constant();
    let trigger_type_reg = program.emit_string8_new_reg(TRIGGER_SCHEMA_TYPE.to_string());
    program.mark_last_insn_constant();

    let end_label = program.allocate_label();
    let loop_label = program.allocate_label();
    program.emit_insn(Insn::Rewind {
        cursor_id: sqlite_schema_cursor_id,
        pc_if_empty: end_label,
    });
    program.preassign_label_to_next_insn(loop_label);

    let next_label = program.allocate_label();
    // Only trigger rows.
    program.emit_column(sqlite_schema_cursor_id, 0, scratch_reg);
    program.emit_insn(Insn::Ne {
        lhs: scratch_reg,
        rhs: trigger_type_reg,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
    });
    // ...whose name / tbl_name matches.
    program.emit_column(sqlite_schema_cursor_id, column_index, scratch_reg);
    program.emit_insn(Insn::Ne {
        lhs: scratch_reg,
        rhs: wanted_reg,
        target_pc: next_label,
        flags: CmpInsFlags::default(),
        collation: program.curr_collation(),
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
}

fn trigger_time_name(time: TriggerTime) -> &'static str {
    match time {
        TriggerTime::Before => "BEFORE",
        TriggerTime::After => "AFTER",
        TriggerTime::InsteadOf => "INSTEAD OF",
    }
}

/// Escape a value for embedding in the `ParseSchema` WHERE clause, which is
/// re-parsed as SQL text.
fn escape_sql_literal(value: &str) -> String {
    value.replace('\'', "''")
}

/// Reject a trigger whose body can never be generated, at `CREATE TRIGGER` time.
///
/// Runs the same `OLD`/`NEW` rewrite the firing path runs, against placeholder
/// registers, so a bad column reference (`NEW.nope`, or `NEW.x` in a `DELETE`
/// trigger) is reported when the trigger is created rather than the first time
/// somebody writes to the table.
fn validate_trigger_body(trigger: &Trigger, target: &Table) -> Result<()> {
    let Table::BTree(btree) = target else {
        // INSTEAD OF triggers on views: the body cannot be validated against a
        // row image because views have no stable column registers here, and the
        // trigger can never fire (writes to views are rejected). Store it.
        return Ok(());
    };
    let placeholder = RowImage {
        rowid_reg: 0,
        cols_start_reg: 0,
    };
    let (old, new) = match &trigger.op {
        crate::schema::TriggerOp::Insert => (None, Some(placeholder)),
        crate::schema::TriggerOp::Delete => (Some(placeholder), None),
        crate::schema::TriggerOp::Update(_) => (Some(placeholder), Some(placeholder)),
    };
    let bindings = rewrite::TriggerRowBindings {
        table: btree.as_ref(),
        old,
        new,
    };
    if let Some(when_clause) = trigger.when_clause.as_ref() {
        rewrite::rewrite_when_clause(when_clause, &bindings)?;
    }
    for cmd in trigger.commands.iter() {
        // `SELECT RAISE(...)` bodies are shape-checked when they are emitted;
        // here only the OLD/NEW references inside them are validated.
        match cmd {
            ast::TriggerCmd::Select(select) => {
                let mut select = select.as_ref().clone();
                rewrite::rewrite_select(&mut select, &bindings)?;
            }
            _ => {
                rewrite::trigger_cmd_to_stmt(cmd, &bindings)?;
            }
        }
    }
    Ok(())
}
