//! VDBE bytecode generation for pragma statements.
//! More info: <https://www.sqlite.org/pragma.html>

use limbo_sqlite3_parser::ast::PragmaName;
use limbo_sqlite3_parser::ast::{self, Expr};
use std::rc::Rc;
use std::sync::Arc;

use crate::fast_lock::SpinLock;
use crate::schema::Schema;
use crate::storage::pager::{AutoVacuumMode, SynchronousMode};
use crate::storage::sqlite3_ondisk::{
    DatabaseHeader, MAX_PAGE_SIZE, MIN_PAGE_CACHE_SIZE, MIN_PAGE_SIZE,
};
use crate::storage::wal::CheckpointMode;
use crate::util::{normalize_ident, parse_signed_number};
use crate::vdbe::builder::{ProgramBuilder, ProgramBuilderOpts, QueryMode};
use crate::vdbe::insn::{Cookie, Insn};
use crate::{bail_parse_error, LimboError, Pager, Value};
use std::str::FromStr;
use strum::IntoEnumIterator;

use super::integrity_check::translate_integrity_check;

fn list_pragmas(program: &mut ProgramBuilder) {
    for x in PragmaName::iter() {
        let register = program.emit_string8_new_reg(x.to_string());
        program.emit_result_row(register, 1);
    }
    program.add_pragma_result_column("pragma_list".into());
    program.epilogue(crate::translate::emitter::TransactionMode::None);
}

pub fn translate_pragma(
    query_mode: QueryMode,
    schema: &Schema,
    name: &ast::QualifiedName,
    body: Option<ast::PragmaBody>,
    database_header: Arc<SpinLock<DatabaseHeader>>,
    pager: Rc<Pager>,
    connection: Arc<crate::Connection>,
    mut program: ProgramBuilder,
) -> crate::Result<ProgramBuilder> {
    let opts = ProgramBuilderOpts {
        query_mode,
        num_cursors: 0,
        approx_num_insns: 20,
        approx_num_labels: 0,
    };
    program.extend(&opts);
    let mut write = false;

    if name.name.0.eq_ignore_ascii_case("pragma_list") {
        list_pragmas(&mut program);
        return Ok(program);
    }

    let pragma = match PragmaName::from_str(&name.name.0) {
        Ok(pragma) => pragma,
        Err(_) => bail_parse_error!("Not a valid pragma name"),
    };

    // `PRAGMA <schema>.<name>` addresses one specific database. The header-cookie
    // family (`user_version`, `application_id`, `schema_version`, `page_count`)
    // is routed to it through the `db` operand of `ReadCookie`/`SetCookie`/
    // `PageCount`. Every other pragma either reads `main`'s header at
    // compile time or changes connection-global state, so a non-`main`
    // qualifier is refused rather than silently answered from `main` -- an
    // answer about the wrong database is worse than an error.
    let db_index = program.resolve_db_index(name.db_name.as_ref().map(|db| db.0.as_str()))?;
    if db_index == crate::multidb::DB_TEMP {
        // `temp` is addressable before it is materialized; a read-only
        // `PRAGMA temp.user_version` must create it rather than fail, exactly as
        // a `CREATE TEMP TABLE` would.
        connection.ensure_temp_db()?;
    }
    if db_index != crate::multidb::DB_MAIN && !pragma_is_per_database(&pragma) {
        bail_parse_error!(
            "PRAGMA {}.{} is not supported: this pragma applies to the main database only",
            name.db_name
                .as_ref()
                .map(|db| db.0.as_str())
                .unwrap_or("main"),
            name.name.0
        );
    }

    match body {
        None => {
            query_pragma(
                pragma,
                schema,
                None,
                database_header.clone(),
                pager,
                connection,
                db_index,
                &mut program,
            )?;
        }
        Some(ast::PragmaBody::Equals(value) | ast::PragmaBody::Call(value)) => match pragma {
            PragmaName::TableInfo
            | PragmaName::ForeignKeyList
            | PragmaName::IndexList
            | PragmaName::IndexInfo => {
                query_pragma(
                    pragma,
                    schema,
                    Some(value),
                    database_header.clone(),
                    pager,
                    connection,
                    db_index,
                    &mut program,
                )?;
            }
            _ => {
                write = true;
                update_pragma(
                    pragma,
                    schema,
                    value,
                    database_header.clone(),
                    pager,
                    connection,
                    db_index,
                    &mut program,
                )?;
            }
        },
    };
    program.epilogue(match write {
        false => super::emitter::TransactionMode::Read,
        true => super::emitter::TransactionMode::Write,
    });

    Ok(program)
}

#[allow(clippy::too_many_arguments)]
/// Whether a pragma is meaningfully per-database, i.e. reads or writes a value
/// that lives in one database's header and is routed through an opcode carrying
/// a `db` operand.
fn pragma_is_per_database(pragma: &PragmaName) -> bool {
    matches!(
        pragma,
        PragmaName::UserVersion
            | PragmaName::ApplicationId
            | PragmaName::SchemaVersion
            | PragmaName::PageCount
    )
}

#[allow(clippy::too_many_arguments)]
fn update_pragma(
    pragma: PragmaName,
    schema: &Schema,
    value: ast::Expr,
    header: Arc<SpinLock<DatabaseHeader>>,
    pager: Rc<Pager>,
    connection: Arc<crate::Connection>,
    db_index: usize,
    program: &mut ProgramBuilder,
) -> crate::Result<()> {
    match pragma {
        PragmaName::CacheSize => {
            let cache_size = match parse_signed_number(&value)? {
                Value::Integer(size) => size,
                Value::Float(size) => size as i64,
                _ => bail_parse_error!("Invalid value for cache size pragma"),
            };
            update_cache_size(cache_size, header, pager, connection)?;
            Ok(())
        }
        PragmaName::JournalMode => {
            query_pragma(
                PragmaName::JournalMode,
                schema,
                None,
                header,
                pager,
                connection,
                db_index,
                program,
            )?;
            Ok(())
        }
        PragmaName::LegacyFileFormat => Ok(()),
        PragmaName::WalCheckpoint => {
            query_pragma(
                PragmaName::WalCheckpoint,
                schema,
                Some(value),
                header,
                pager,
                connection,
                db_index,
                program,
            )?;
            Ok(())
        }
        PragmaName::PageCount => {
            query_pragma(
                PragmaName::PageCount,
                schema,
                None,
                header,
                pager,
                connection,
                db_index,
                program,
            )?;
            Ok(())
        }
        PragmaName::UserVersion => {
            let data = parse_signed_number(&value)?;
            let version_value = match data {
                Value::Integer(i) => i as i32,
                Value::Float(f) => f as i32,
                _ => unreachable!(),
            };

            program.emit_insn(Insn::SetCookie {
                db: db_index,
                cookie: Cookie::UserVersion,
                value: version_value,
                p5: 1,
            });
            Ok(())
        }
        PragmaName::ApplicationId => {
            // SQLite presents application_id as a SIGNED 32-bit integer.
            // Parse the user-supplied signed-number and narrow to i32 so that
            // values such as -1 round-trip faithfully through the header.
            let data = parse_signed_number(&value)?;
            let application_id_value = match data {
                Value::Integer(i) => i as i32,
                Value::Float(f) => f as i32,
                other => {
                    return Err(crate::LimboError::InvalidArgument(format!(
                        "application_id must be an integer, got {other:?}"
                    )));
                }
            };

            program.emit_insn(Insn::SetCookie {
                db: db_index,
                cookie: Cookie::ApplicationId,
                value: application_id_value,
                p5: 1,
            });
            Ok(())
        }
        PragmaName::SchemaVersion => {
            let data = parse_signed_number(&value)?;
            let schema_version_value = match data {
                Value::Integer(i) => i as i32,
                Value::Float(f) => f as i32,
                _ => unreachable!(),
            };

            program.emit_insn(Insn::SetCookie {
                db: db_index,
                cookie: Cookie::SchemaVersion,
                value: schema_version_value,
                p5: 1,
            });
            Ok(())
        }
        PragmaName::TableInfo
        | PragmaName::ForeignKeyList
        | PragmaName::IndexList
        | PragmaName::IndexInfo => {
            // because we need control over the write parameter for the transaction,
            // this should be unreachable. We have to force-call query_pragma before
            // getting here
            unreachable!();
        }
        PragmaName::PageSize => {
            // SQLite semantics (`setPageSize` in pragma.c / btree.c): the requested
            // size must be a power of two in [MIN_PAGE_SIZE, MAX_PAGE_SIZE]; any
            // other value -- including a non-numeric argument -- is *silently
            // ignored*. Even a legal value only takes effect while the database
            // file is still completely empty; otherwise it is merely recorded and
            // applied by the next `VACUUM`. Crucially, `PRAGMA page_size = N`
            // never raises an error: drivers and ORMs emit it unconditionally
            // during connection setup, so turning it into an `Err` would break the
            // handshake just as badly as the `todo!()` that used to abort the
            // process here.
            //
            // Changing the live page size would require rebuilding the buffer
            // pool, the page cache and the WAL around the new size (and, for a
            // non-empty file, a full VACUUM-style rewrite), which is not
            // implemented. So we validate, log, and defer -- observationally
            // identical to SQLite's behaviour on a database that already holds
            // data, which is the overwhelmingly common case.
            let requested = match parse_signed_number(&value) {
                Ok(Value::Integer(size)) => Some(size),
                Ok(Value::Float(size)) => Some(size as i64),
                // Non-numeric argument: ignored, exactly like SQLite.
                _ => None,
            };
            if let Some(size) = requested {
                let is_legal = u32::try_from(size).is_ok_and(|size| {
                    (MIN_PAGE_SIZE..=MAX_PAGE_SIZE).contains(&size) && size.is_power_of_two()
                });
                let current = header.lock().get_page_size();
                if !is_legal {
                    tracing::debug!(
                        "PRAGMA page_size = {size}: not a power of two in \
                         [{MIN_PAGE_SIZE}, {MAX_PAGE_SIZE}], ignored"
                    );
                } else if size as u32 != current {
                    tracing::warn!(
                        "PRAGMA page_size = {size}: deferred (current page size {current} \
                         retained); changing the page size of an existing database \
                         requires VACUUM, which is not implemented yet"
                    );
                }
            }
            Ok(())
        }
        PragmaName::AutoVacuum => {
            let auto_vacuum_mode = match value {
                Expr::Name(name) => {
                    let name = name.0.to_lowercase();
                    match name.as_str() {
                        "none" => 0,
                        "full" => 1,
                        "incremental" => 2,
                        _ => {
                            return Err(LimboError::InvalidArgument(
                                "invalid auto vacuum mode".to_string(),
                            ));
                        }
                    }
                }
                _ => {
                    return Err(LimboError::InvalidArgument(
                        "invalid auto vacuum mode".to_string(),
                    ))
                }
            };
            match auto_vacuum_mode {
                0 => update_auto_vacuum_mode(AutoVacuumMode::None, 0, header, pager)?,
                1 => update_auto_vacuum_mode(AutoVacuumMode::Full, 1, header, pager)?,
                2 => {
                    // Incremental auto-vacuum has no implementation: the freelist
                    // trunk walk that `PRAGMA incremental_vacuum` would need does
                    // not exist, and `Pager::btree_create` has no
                    // `AutoVacuumMode::Incremental` root-page allocation strategy.
                    //
                    // Accepting mode 2 here used to arm that mode on the pager AND
                    // persist it into the database header, so the very next
                    // `CREATE TABLE`/`CREATE INDEX` aborted the host process on an
                    // `unimplemented!()` -- and, because the header had already
                    // been written, kept aborting on every reopen of the file.
                    // Reject *before* mutating any state so nothing is armed and
                    // nothing is persisted.
                    return Err(LimboError::InvalidArgument(
                        "incremental auto_vacuum mode (auto_vacuum = 2) is not supported yet; \
                         use 'none' or 'full'"
                            .to_string(),
                    ));
                }
                _ => {
                    return Err(LimboError::InvalidArgument(
                        "invalid auto vacuum mode".to_string(),
                    ))
                }
            }
            let largest_root_page_number_reg = program.alloc_register();
            program.emit_insn(Insn::ReadCookie {
                db: 0,
                dest: largest_root_page_number_reg,
                cookie: Cookie::LargestRootPageNumber,
            });
            let set_cookie_label = program.allocate_label();
            program.emit_insn(Insn::If {
                reg: largest_root_page_number_reg,
                target_pc: set_cookie_label,
                jump_if_null: false,
            });
            program.emit_insn(Insn::Halt {
                err_code: 0,
                description: "Early halt because auto vacuum mode is not enabled".to_string(),
            });
            program.resolve_label(set_cookie_label, program.offset());
            program.emit_insn(Insn::SetCookie {
                db: 0,
                cookie: Cookie::IncrementalVacuum,
                value: auto_vacuum_mode - 1,
                p5: 0,
            });
            Ok(())
        }
        PragmaName::Synchronous => {
            let mode = match &value {
                Expr::Name(name) => {
                    let n = name.0.to_ascii_lowercase();
                    match n.as_str() {
                        "off" => SynchronousMode::Off,
                        "normal" => SynchronousMode::Normal,
                        "full" => SynchronousMode::Full,
                        "extra" => SynchronousMode::Extra,
                        _ => {
                            return Err(LimboError::InvalidArgument(format!(
                                "invalid synchronous mode: {}",
                                name.0
                            )));
                        }
                    }
                }
                _ => {
                    let n = match parse_signed_number(&value)? {
                        Value::Integer(i) => i,
                        Value::Float(f) => f as i64,
                        other => {
                            return Err(LimboError::InvalidArgument(format!(
                                "invalid synchronous mode: {other:?}"
                            )));
                        }
                    };
                    match n {
                        0 => SynchronousMode::Off,
                        1 => SynchronousMode::Normal,
                        2 => SynchronousMode::Full,
                        3 => SynchronousMode::Extra,
                        _ => {
                            return Err(LimboError::InvalidArgument(format!(
                                "invalid synchronous mode value: {n}"
                            )));
                        }
                    }
                }
            };
            pager.set_synchronous_mode(mode);
            Ok(())
        }
        PragmaName::IntegrityCheck => unreachable!("integrity_check cannot be set"),
    }
}

#[allow(clippy::too_many_arguments)]
fn query_pragma(
    pragma: PragmaName,
    schema: &Schema,
    value: Option<ast::Expr>,
    database_header: Arc<SpinLock<DatabaseHeader>>,
    pager: Rc<Pager>,
    connection: Arc<crate::Connection>,
    db_index: usize,
    program: &mut ProgramBuilder,
) -> crate::Result<()> {
    let register = program.alloc_register();
    match pragma {
        PragmaName::CacheSize => {
            program.emit_int(connection.get_cache_size() as i64, register);
            program.emit_result_row(register, 1);
            program.add_pragma_result_column(pragma.to_string());
        }
        PragmaName::JournalMode => {
            program.emit_string8("wal".into(), register);
            program.emit_result_row(register, 1);
            program.add_pragma_result_column(pragma.to_string());
        }
        PragmaName::LegacyFileFormat => {}
        PragmaName::WalCheckpoint => {
            // Checkpoint uses 3 registers: P1, P2, P3. Ref Insn::Checkpoint for more info.
            // Allocate two more here as one was allocated at the top.
            let mode = match value {
                Some(ast::Expr::Name(name)) => {
                    let mode_name = normalize_ident(&name.0);
                    CheckpointMode::from_str(&mode_name).map_err(|e| {
                        LimboError::ParseError(format!("Unknown Checkpoint Mode: {}", e))
                    })?
                }
                _ => CheckpointMode::Passive,
            };

            if !matches!(mode, CheckpointMode::Passive) {
                return Err(LimboError::ParseError(
                    "only Passive mode supported".to_string(),
                ));
            }

            program.alloc_registers(2);
            program.emit_insn(Insn::Checkpoint {
                database: 0,
                checkpoint_mode: mode,
                dest: register,
            });
            program.emit_result_row(register, 3);
        }
        PragmaName::PageCount => {
            program.emit_insn(Insn::PageCount {
                db: db_index,
                dest: register,
            });
            program.emit_result_row(register, 1);
            program.add_pragma_result_column(pragma.to_string());
        }
        PragmaName::TableInfo => {
            let table = match value {
                Some(ast::Expr::Name(name)) => {
                    let tbl = normalize_ident(&name.0);
                    schema.get_table(&tbl)
                }
                _ => None,
            };

            let base_reg = register;
            program.alloc_registers(5);
            if let Some(table) = table {
                // A view's output columns are resolved lazily (schema load leaves
                // them empty to keep connection opens cheap; see
                // `util::parse_schema_rows`). Infer them here, the sole consumer.
                // A malformed/cyclic view resolves to no columns, exactly as the
                // former eager pass left it.
                let resolved_view_columns: Vec<crate::schema::Column> = match table.as_ref() {
                    crate::schema::Table::View(view) if view.columns.is_empty() => {
                        let syms = connection.syms.borrow();
                        crate::util::resolve_view_columns(schema, &syms, view).unwrap_or_default()
                    }
                    _ => Vec::new(),
                };
                let columns: &[crate::schema::Column] = if resolved_view_columns.is_empty() {
                    table.columns()
                } else {
                    &resolved_view_columns
                };
                for (i, column) in columns.iter().enumerate() {
                    // cid
                    program.emit_int(i as i64, base_reg);
                    // name
                    program.emit_string8(column.name.clone().unwrap_or_default(), base_reg + 1);

                    // type
                    program.emit_string8(column.ty_str.clone(), base_reg + 2);

                    // notnull
                    program.emit_bool(column.notnull, base_reg + 3);

                    // dflt_value
                    match &column.default {
                        None => {
                            program.emit_null(base_reg + 4, None);
                        }
                        Some(expr) => {
                            program.emit_string8(expr.to_string(), base_reg + 4);
                        }
                    }

                    // pk
                    program.emit_bool(column.primary_key, base_reg + 5);

                    program.emit_result_row(base_reg, 6);
                }
            }
            let col_names = ["cid", "name", "type", "notnull", "dflt_value", "pk"];
            for name in col_names {
                program.add_pragma_result_column(name.into());
            }
        }
        PragmaName::ForeignKeyList => {
            // PRAGMA foreign_key_list(table) — 8-column SQLite shape:
            // id, seq, table, from, to, on_update, on_delete, match
            let btree_table = match value {
                Some(ast::Expr::Name(name)) => {
                    let tbl = normalize_ident(&name.0);
                    schema.get_btree_table(&tbl)
                }
                _ => None,
            };

            // 8 columns total; base_reg is already allocated above.
            let base_reg = register;
            program.alloc_registers(7);

            if let Some(tbl) = btree_table {
                for fk in &tbl.foreign_keys {
                    let max_seq = fk.from_cols.len().max(1);
                    for seq in 0..max_seq {
                        let from_col = fk.from_cols.get(seq).cloned().unwrap_or_default();
                        let to_col = fk.to_cols.get(seq).cloned();

                        // id
                        program.emit_int(fk.id as i64, base_reg);
                        // seq
                        program.emit_int(seq as i64, base_reg + 1);
                        // table (parent table name)
                        program.emit_string8(fk.to_table.clone(), base_reg + 2);
                        // from (child column)
                        program.emit_string8(from_col, base_reg + 3);
                        // to (parent column, or NULL if implicit PK reference)
                        match to_col {
                            Some(col) if !col.is_empty() => {
                                program.emit_string8(col, base_reg + 4);
                            }
                            _ => {
                                program.emit_null(base_reg + 4, None);
                            }
                        }
                        // on_update
                        program.emit_string8(fk.on_update.clone(), base_reg + 5);
                        // on_delete
                        program.emit_string8(fk.on_delete.clone(), base_reg + 6);
                        // match
                        program.emit_string8(fk.match_clause.clone(), base_reg + 7);

                        program.emit_result_row(base_reg, 8);
                    }
                }
            }

            let col_names = [
                "id",
                "seq",
                "table",
                "from",
                "to",
                "on_update",
                "on_delete",
                "match",
            ];
            for name in col_names {
                program.add_pragma_result_column(name.into());
            }
        }
        PragmaName::IndexList => {
            // PRAGMA index_list(table) — 5-column SQLite shape:
            //   seq, name, unique, origin, partial
            // Emitted straight from the in-memory schema, which is more reliable
            // than re-parsing the CREATE INDEX text from sqlite_schema. The parsed
            // index list is only retained when the `index_experimental` feature is
            // on; without it the schema keeps a per-table "has indexes" bit only.
            let tbl = match value {
                Some(ast::Expr::Name(name)) => Some(normalize_ident(&name.0)),
                _ => None,
            };

            // Without the feature we cannot enumerate index rows. Returning zero
            // rows for a table that actually has indexes would be a silent lie, so
            // raise a typed error in that case (a table with no indexes, or an
            // unknown table, still legitimately produces an empty result set).
            #[cfg(not(feature = "index_experimental"))]
            if let Some(t) = &tbl {
                if schema.table_has_indexes(t) {
                    bail_parse_error!(
                        "PRAGMA index_list requires the `index_experimental` \
                         feature to enumerate index metadata"
                    );
                }
            }

            #[cfg(feature = "index_experimental")]
            {
                let indices: Vec<Arc<crate::schema::Index>> = match &tbl {
                    Some(t) => schema.get_indices(t).to_vec(),
                    None => Vec::new(),
                };

                // 5 columns total; base_reg is already allocated above.
                let base_reg = register;
                program.alloc_registers(4);

                for (seq, index) in indices.iter().enumerate() {
                    // seq — position of the index within the table
                    program.emit_int(seq as i64, base_reg);
                    // name
                    program.emit_string8(index.name.clone(), base_reg + 1);
                    // unique
                    program.emit_bool(index.unique, base_reg + 2);
                    // origin — "c" for an explicit CREATE INDEX, "u" for an index
                    // that backs a UNIQUE / PRIMARY KEY constraint (auto-index). We
                    // cannot tell PRIMARY KEY from UNIQUE apart here, so we do not
                    // fabricate SQLite's "pk"; "u" is the honest classification.
                    let origin = if index.name.starts_with("sqlite_autoindex") {
                        "u"
                    } else {
                        "c"
                    };
                    program.emit_string8(origin.into(), base_reg + 3);
                    // partial — the schema does not retain a partial index's WHERE
                    // predicate (`Index::from_sql` drops `where_clause`), so this is
                    // always 0. Correct for non-partial indexes, conservative otherwise.
                    program.emit_int(0, base_reg + 4);

                    program.emit_result_row(base_reg, 5);
                }
            }

            let col_names = ["seq", "name", "unique", "origin", "partial"];
            for name in col_names {
                program.add_pragma_result_column(name.into());
            }
        }
        PragmaName::IndexInfo => {
            // PRAGMA index_info(index) — 3-column SQLite shape:
            //   seqno, cid, name
            // Without the `index_experimental` feature the schema retains no parsed
            // index definitions, so no index can be resolved; emitting an empty
            // result would be indistinguishable from "no such index", which is
            // false for every real index. Raise a typed error instead.
            #[cfg(not(feature = "index_experimental"))]
            {
                let _ = (&value, register);
                bail_parse_error!(
                    "PRAGMA index_info requires the `index_experimental` feature \
                     to enumerate index columns"
                );
            }

            #[cfg(feature = "index_experimental")]
            {
                // Index names are globally unique, so a scan across every table's
                // index list resolves the argument unambiguously.
                let index: Option<Arc<crate::schema::Index>> = match value {
                    Some(ast::Expr::Name(name)) => {
                        let idx_name = normalize_ident(&name.0);
                        schema
                            .indexes
                            .values()
                            .flatten()
                            .find(|idx| idx.name == idx_name)
                            .cloned()
                    }
                    _ => None,
                };

                // 3 columns total; base_reg is already allocated above.
                let base_reg = register;
                program.alloc_registers(2);

                if let Some(index) = index {
                    for (seqno, column) in index.columns.iter().enumerate() {
                        // seqno — rank of the column within the index
                        program.emit_int(seqno as i64, base_reg);
                        // cid — rank of the column within the source table
                        program.emit_int(column.pos_in_table as i64, base_reg + 1);
                        // name — the indexed column name
                        program.emit_string8(column.name.clone(), base_reg + 2);

                        program.emit_result_row(base_reg, 3);
                    }
                }

                let col_names = ["seqno", "cid", "name"];
                for name in col_names {
                    program.add_pragma_result_column(name.into());
                }
            }
        }
        PragmaName::UserVersion => {
            program.emit_insn(Insn::ReadCookie {
                db: db_index,
                dest: register,
                cookie: Cookie::UserVersion,
            });
            program.add_pragma_result_column(pragma.to_string());
            program.emit_result_row(register, 1);
        }
        PragmaName::ApplicationId => {
            program.emit_insn(Insn::ReadCookie {
                db: db_index,
                dest: register,
                cookie: Cookie::ApplicationId,
            });
            program.add_pragma_result_column(pragma.to_string());
            program.emit_result_row(register, 1);
        }
        PragmaName::SchemaVersion => {
            program.emit_insn(Insn::ReadCookie {
                db: db_index,
                dest: register,
                cookie: Cookie::SchemaVersion,
            });
            program.add_pragma_result_column(pragma.to_string());
            program.emit_result_row(register, 1);
        }
        PragmaName::PageSize => {
            program.emit_int(database_header.lock().get_page_size().into(), register);
            program.emit_result_row(register, 1);
            program.add_pragma_result_column(pragma.to_string());
        }
        PragmaName::AutoVacuum => {
            let auto_vacuum_mode = pager.get_auto_vacuum_mode();
            let auto_vacuum_mode_i64: i64 = match auto_vacuum_mode {
                AutoVacuumMode::None => 0,
                AutoVacuumMode::Full => 1,
                AutoVacuumMode::Incremental => 2,
            };
            let register = program.alloc_register();
            program.emit_insn(Insn::Int64 {
                _p1: 0,
                out_reg: register,
                _p3: 0,
                value: auto_vacuum_mode_i64,
            });
            program.emit_result_row(register, 1);
        }
        PragmaName::Synchronous => {
            let mode = pager.get_synchronous_mode();
            program.emit_int(mode.as_i64(), register);
            program.emit_result_row(register, 1);
            program.add_pragma_result_column(pragma.to_string());
        }
        PragmaName::IntegrityCheck => {
            translate_integrity_check(schema, program)?;
        }
    }

    Ok(())
}

fn update_auto_vacuum_mode(
    auto_vacuum_mode: AutoVacuumMode,
    largest_root_page_number: u32,
    header: Arc<SpinLock<DatabaseHeader>>,
    pager: Rc<Pager>,
) -> crate::Result<()> {
    let mut header_guard = header.lock();
    header_guard.vacuum_mode_largest_root_page = largest_root_page_number;
    pager.set_auto_vacuum_mode(auto_vacuum_mode);
    pager.write_database_header(&header_guard)?;
    Ok(())
}

fn update_cache_size(
    value: i64,
    header: Arc<SpinLock<DatabaseHeader>>,
    pager: Rc<Pager>,
    connection: Arc<crate::Connection>,
) -> crate::Result<()> {
    let mut cache_size_unformatted: i64 = value;
    let mut cache_size = if cache_size_unformatted < 0 {
        let kb = cache_size_unformatted.abs() * 1024;
        let page_size = header.lock().get_page_size();
        kb / page_size as i64
    } else {
        value
    } as usize;

    if cache_size < MIN_PAGE_CACHE_SIZE {
        cache_size = MIN_PAGE_CACHE_SIZE;
        cache_size_unformatted = MIN_PAGE_CACHE_SIZE as i64;
    }
    connection.set_cache_size(cache_size_unformatted as i32);

    // update cache size
    pager
        .change_page_cache_size(cache_size)
        .expect("couldn't update page cache size");

    Ok(())
}
