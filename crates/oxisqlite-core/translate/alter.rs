use fallible_iterator::FallibleIterator as _;
use limbo_sqlite3_parser::{ast, lexer::sql::Parser};

use crate::{
    function::{AlterTableFunc, Func},
    schema::{Column, Schema},
    util::normalize_ident,
    vdbe::{
        builder::{ProgramBuilder, QueryMode},
        insn::{Insn, RegisterOrLiteral},
    },
    LimboError, Result, SymbolTable,
};

use super::{
    emitter::TransactionMode, schema::SQLITE_TABLEID, update::translate_update_with_after,
};

pub fn translate_alter_table(
    alter: (ast::QualifiedName, ast::AlterTableBody),
    syms: &SymbolTable,
    schema: &Schema,
    mut program: ProgramBuilder,
) -> Result<ProgramBuilder> {
    let (table_name, alter_table) = alter;
    let ast::Name(table_name) = table_name.name;
    #[cfg(not(feature = "index_experimental"))]
    {
        if schema.table_has_indexes(&table_name) && cfg!(not(feature = "index_experimental")) {
            // Let's disable altering a table with indices altogether instead of checking column by
            // column to be extra safe.
            crate::bail_parse_error!(
                "Alter table disabled for table with indexes without index_experimental feature flag"
            );
        }
    }

    let Some(original_btree) = schema
        .get_table(&table_name)
        .and_then(|table| table.btree())
    else {
        return Err(LimboError::ParseError(format!(
            "no such table: {table_name}"
        )));
    };

    let mut btree = (*original_btree).clone();

    Ok(match alter_table {
        ast::AlterTableBody::DropColumn(column_name) => {
            let ast::Name(column_name) = column_name;

            // Tables always have at least one column.
            assert_ne!(btree.columns.len(), 0);

            if btree.columns.len() == 1 {
                return Err(LimboError::ParseError(format!(
                    "cannot drop column \"{column_name}\": no other columns exist"
                )));
            }

            let (dropped_index, column) = btree.get_column(&column_name).ok_or_else(|| {
                LimboError::ParseError(format!("no such column: \"{column_name}\""))
            })?;

            if column.primary_key {
                return Err(LimboError::ParseError(format!(
                    "cannot drop column \"{column_name}\": PRIMARY KEY"
                )));
            }

            if column.unique
                || btree.unique_sets.as_ref().is_some_and(|sets| {
                    sets.iter().any(|set| {
                        set.columns
                            .iter()
                            .any(|(name, _)| name == &normalize_ident(&column_name))
                    })
                })
            {
                return Err(LimboError::ParseError(format!(
                    "cannot drop column \"{column_name}\": UNIQUE"
                )));
            }

            btree.columns.remove(dropped_index);

            let sql = btree.to_sql();

            let stmt = format!(
                r#"
                    UPDATE {SQLITE_TABLEID}
                    SET sql = '{sql}'
                    WHERE name = '{table_name}' COLLATE NOCASE AND type = 'table'
                "#,
            );

            let mut parser = Parser::new(stmt.as_bytes());
            let Some(ast::Cmd::Stmt(ast::Stmt::Update(mut update))) = parser
                .next()
                .expect("parser should produce Update statement")
            else {
                unreachable!();
            };

            translate_update_with_after(
                QueryMode::Normal,
                schema,
                &mut update,
                syms,
                program,
                |program| {
                    let column_count = btree.columns.len();
                    let root_page = btree.root_page;
                    let table_name = btree.name.clone();

                    let cursor_id = program.alloc_cursor_id(
                        crate::vdbe::builder::CursorType::BTreeTable(original_btree),
                    );

                    program.emit_insn(Insn::OpenWrite {
                        cursor_id,
                        root_page: RegisterOrLiteral::Literal(root_page),
                        name: table_name.clone(),
                    });

                    program.cursor_loop(cursor_id, |program, rowid| {
                        let first_column = program.alloc_registers(column_count);

                        let mut iter = first_column;

                        for i in 0..(column_count + 1) {
                            if i == dropped_index {
                                continue;
                            }

                            program.emit_column(cursor_id, i, iter);

                            iter += 1;
                        }

                        let record = program.alloc_register();

                        program.emit_insn(Insn::MakeRecord {
                            start_reg: first_column,
                            count: column_count,
                            dest_reg: record,
                            index_name: None,
                        });

                        program.emit_insn(Insn::Insert {
                            cursor: cursor_id,
                            key_reg: rowid,
                            record_reg: record,
                            flag: crate::vdbe::insn::InsertFlags(0),
                            table_name: table_name.clone(),
                        });
                    });

                    program.emit_schema_change();
                    program.emit_insn(Insn::ParseSchema {
                        db: usize::MAX, // TODO: This value is unused, change when we do something with it
                        where_clause: None,
                    })
                },
            )?
        }
        ast::AlterTableBody::AddColumn(col_def) => {
            let column = Column::from(col_def);

            if let Some(default) = &column.default {
                if !matches!(
                    default,
                    ast::Expr::Literal(
                        ast::Literal::Null
                            | ast::Literal::Blob(_)
                            | ast::Literal::Numeric(_)
                            | ast::Literal::String(_)
                    )
                ) {
                    // Real SQLite classifies this as a runtime error, not a parse/syntax
                    // error -- even though whether the DEFAULT expression is a literal is
                    // fully knowable at translate time (it's syntactic, not
                    // data-dependent), so there's no genuine need to defer the check into
                    // a bytecode program. `LimboError::Constraint` is this crate's
                    // "Runtime error: {0}" variant (see its #[error(...)] attribute in
                    // error.rs), matching SQLite's own classification here.
                    return Err(LimboError::Constraint(
                        "Cannot add a column with non-constant default".to_string(),
                    ));
                }
            }

            btree.columns.push(column);

            let sql = btree.to_sql();
            let mut escaped = String::with_capacity(sql.len());

            for ch in sql.chars() {
                match ch {
                    '\'' => escaped.push_str("''"),
                    ch => escaped.push(ch),
                }
            }

            let stmt = format!(
                r#"
                    UPDATE {SQLITE_TABLEID}
                    SET sql = '{escaped}'
                    WHERE name = '{table_name}' COLLATE NOCASE AND type = 'table'
                "#,
            );

            let mut parser = Parser::new(stmt.as_bytes());
            let Some(ast::Cmd::Stmt(ast::Stmt::Update(mut update))) = parser
                .next()
                .expect("parser should produce Update statement")
            else {
                unreachable!();
            };

            translate_update_with_after(
                QueryMode::Normal,
                schema,
                &mut update,
                syms,
                program,
                |program| {
                    program.emit_schema_change();
                    program.emit_insn(Insn::ParseSchema {
                        db: usize::MAX, // TODO: This value is unused, change when we do something with it
                        where_clause: None,
                    });
                },
            )?
        }
        ast::AlterTableBody::RenameColumn { old, new } => {
            let ast::Name(rename_from) = old;
            let ast::Name(rename_to) = new;

            if btree.get_column(&rename_from).is_none() {
                return Err(LimboError::ParseError(format!(
                    "no such column: \"{rename_from}\""
                )));
            };

            if btree.get_column(&rename_to).is_some() {
                return Err(LimboError::ParseError(format!(
                    "duplicate column name: \"{rename_from}\""
                )));
            };

            let sqlite_schema = schema
                .get_btree_table(SQLITE_TABLEID)
                .expect("sqlite_schema should be on schema");

            let cursor_id = program.alloc_cursor_id(crate::vdbe::builder::CursorType::BTreeTable(
                sqlite_schema.clone(),
            ));

            program.emit_insn(Insn::OpenWrite {
                cursor_id,
                root_page: RegisterOrLiteral::Literal(sqlite_schema.root_page),
                name: sqlite_schema.name.clone(),
            });

            program.cursor_loop(cursor_id, |program, rowid| {
                let sqlite_schema_column_len = sqlite_schema.columns.len();
                assert_eq!(sqlite_schema_column_len, 5);

                let first_column = program.alloc_registers(sqlite_schema_column_len);

                for i in 0..sqlite_schema_column_len {
                    program.emit_column(cursor_id, i, first_column + i);
                }

                program.emit_string8_new_reg(table_name.clone());
                program.mark_last_insn_constant();

                program.emit_string8_new_reg(rename_from.clone());
                program.mark_last_insn_constant();

                program.emit_string8_new_reg(rename_to.clone());
                program.mark_last_insn_constant();

                let out = program.alloc_registers(sqlite_schema_column_len);

                program.emit_insn(Insn::Function {
                    constant_mask: 0,
                    start_reg: first_column,
                    dest: out,
                    func: crate::function::FuncCtx {
                        func: Func::AlterTable(AlterTableFunc::RenameColumn),
                        arg_count: 8,
                    },
                });

                let record = program.alloc_register();

                program.emit_insn(Insn::MakeRecord {
                    start_reg: out,
                    count: sqlite_schema_column_len,
                    dest_reg: record,
                    index_name: None,
                });

                program.emit_insn(Insn::Insert {
                    cursor: cursor_id,
                    key_reg: rowid,
                    record_reg: record,
                    flag: crate::vdbe::insn::InsertFlags(0),
                    table_name: table_name.clone(),
                });
            });

            program.emit_schema_change();
            program.emit_insn(Insn::ParseSchema {
                db: usize::MAX, // TODO: This value is unused, change when we do something with it
                where_clause: None,
            });

            program.epilogue(TransactionMode::Write);

            program
        }
        ast::AlterTableBody::RenameTo(new_name) => {
            let ast::Name(new_name) = new_name;

            if schema.get_table(&new_name).is_some() {
                return Err(LimboError::ParseError(format!(
                    "there is already another table or index with this name: {new_name}"
                )));
            };

            let sqlite_schema = schema
                .get_btree_table(SQLITE_TABLEID)
                .expect("sqlite_schema should be on schema");

            let cursor_id = program.alloc_cursor_id(crate::vdbe::builder::CursorType::BTreeTable(
                sqlite_schema.clone(),
            ));

            program.emit_insn(Insn::OpenWrite {
                cursor_id,
                root_page: RegisterOrLiteral::Literal(sqlite_schema.root_page),
                name: sqlite_schema.name.clone(),
            });

            program.cursor_loop(cursor_id, |program, rowid| {
                let sqlite_schema_column_len = sqlite_schema.columns.len();
                assert_eq!(sqlite_schema_column_len, 5);

                let first_column = program.alloc_registers(sqlite_schema_column_len);

                for i in 0..sqlite_schema_column_len {
                    program.emit_column(cursor_id, i, first_column + i);
                }

                program.emit_string8_new_reg(table_name.clone());
                program.mark_last_insn_constant();

                program.emit_string8_new_reg(new_name.clone());
                program.mark_last_insn_constant();

                let out = program.alloc_registers(5);

                program.emit_insn(Insn::Function {
                    constant_mask: 0,
                    start_reg: first_column,
                    dest: out,
                    func: crate::function::FuncCtx {
                        func: Func::AlterTable(AlterTableFunc::RenameTable),
                        arg_count: 7,
                    },
                });

                let record = program.alloc_register();

                program.emit_insn(Insn::MakeRecord {
                    start_reg: out,
                    count: sqlite_schema_column_len,
                    dest_reg: record,
                    index_name: None,
                });

                program.emit_insn(Insn::Insert {
                    cursor: cursor_id,
                    key_reg: rowid,
                    record_reg: record,
                    flag: crate::vdbe::insn::InsertFlags(0),
                    table_name: table_name.clone(),
                });
            });

            program.emit_schema_change();
            program.emit_insn(Insn::ParseSchema {
                db: usize::MAX, // TODO: This value is unused, change when we do something with it
                where_clause: None,
            });

            program.epilogue(TransactionMode::Write);

            program
        }
    })
}

#[cfg(test)]
mod tests {
    //! Regression tests for `ALTER TABLE ... ADD COLUMN ... DEFAULT <non-constant>`.
    //! The statement must still be rejected (this crate does not support
    //! evaluating a non-constant `DEFAULT` for `ADD COLUMN`, matching real
    //! SQLite), but its `LimboError` classification changed from `ParseError`
    //! to `Constraint` ("Runtime error: ...") to match SQLite's own
    //! classification of this failure. See the comment at the `Err(...)` site
    //! above for details; this is purely a diagnostic-classification change,
    //! not a behavior change.

    use std::sync::Arc;

    use crate::{Database, LimboError};

    fn open_conn() -> (Arc<dyn crate::IO>, Arc<crate::Connection>) {
        let io: Arc<dyn crate::IO> = Arc::new(crate::MemoryIO::new());
        let db =
            Database::open_file(io.clone(), ":memory:", false).expect("open in-memory database");
        let conn = db.connect().expect("connect");
        (io, conn)
    }

    #[test]
    fn add_column_with_non_constant_default_is_rejected_as_constraint_error() {
        let (_io, conn) = open_conn();
        conn.execute("CREATE TABLE t(a INTEGER)")
            .expect("create table");

        // `random()` is a function call, not a literal, so it is rejected as a
        // non-constant DEFAULT -- but now classified as LimboError::Constraint
        // ("Runtime error: ...") instead of LimboError::ParseError, matching
        // real SQLite's classification.
        let err = conn
            .execute("ALTER TABLE t ADD COLUMN b INTEGER DEFAULT (random())")
            .expect_err("non-constant DEFAULT must still be rejected");
        assert!(
            matches!(err, LimboError::Constraint(_)),
            "expected LimboError::Constraint (SQLite's \"Runtime error\" classification), got {err:?}"
        );
        assert!(
            err.to_string().starts_with("Runtime error:"),
            "expected the \"Runtime error:\" prefix from LimboError::Constraint's Display, got: {err}"
        );
    }

    #[test]
    fn add_column_with_constant_default_still_succeeds() {
        // Unaffected control case: a literal DEFAULT must keep working exactly
        // as before this classification-only change.
        let (_io, conn) = open_conn();
        conn.execute("CREATE TABLE t(a INTEGER)")
            .expect("create table");
        conn.execute("ALTER TABLE t ADD COLUMN b INTEGER DEFAULT 42")
            .expect("constant DEFAULT must still be accepted");
    }
}
