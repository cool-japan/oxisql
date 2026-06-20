//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::translate::collate::CollationSeq;
use crate::{util::normalize_ident, Result};
use fallible_iterator::FallibleIterator;
use limbo_sqlite3_parser::ast::{Expr, SortOrder};
use limbo_sqlite3_parser::{
    ast::{Cmd, Stmt},
    lexer::sql::Parser,
};

use super::table::BTreeTable;

#[allow(dead_code)]
#[derive(Debug)]
pub struct Index {
    pub name: String,
    pub table_name: String,
    pub root_page: usize,
    pub columns: Vec<IndexColumn>,
    pub unique: bool,
    pub ephemeral: bool,
    /// Does the index have a rowid as the last column?
    /// This is the case for btree indexes (persistent or ephemeral) that
    /// have been created based on a table with a rowid.
    /// For example, WITHOUT ROWID tables (not supported in Limbo yet),
    /// and  SELECT DISTINCT ephemeral indexes will not have a rowid.
    pub has_rowid: bool,
}
impl Index {
    pub fn from_sql(sql: &str, root_page: usize, table: &BTreeTable) -> Result<Index> {
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser.next()?;
        match cmd {
            Some(Cmd::Stmt(Stmt::CreateIndex {
                idx_name,
                tbl_name,
                columns,
                unique,
                ..
            })) => {
                let index_name = normalize_ident(&idx_name.name.0);
                let mut index_columns = Vec::with_capacity(columns.len());
                for col in columns.into_iter() {
                    let name = normalize_ident(&col.expr.to_string());
                    let Some((pos_in_table, _)) = table.get_column(&name) else {
                        return Err(crate::LimboError::InternalError(format!(
                            "Column {} is in index {} but not found in table {}",
                            name, index_name, table.name
                        )));
                    };
                    let (_, column) = table.get_column(&name).unwrap();
                    index_columns.push(IndexColumn {
                        name,
                        order: col.order.unwrap_or(SortOrder::Asc),
                        pos_in_table,
                        collation: column.collation,
                        default: column.default.clone(),
                    });
                }
                Ok(Index {
                    name: index_name,
                    table_name: normalize_ident(&tbl_name.0),
                    root_page,
                    columns: index_columns,
                    unique,
                    ephemeral: false,
                    has_rowid: table.has_rowid,
                })
            }
            _ => todo!("Expected create index statement"),
        }
    }
    /// The order of index returned should be kept the same
    ///
    /// If the order of the index returned changes, this is a breaking change
    ///
    /// In the future when we support Alter Column, we should revisit a way to make this less dependent on ordering
    pub fn automatic_from_primary_key_and_unique(
        table: &BTreeTable,
        auto_indices: Vec<(String, usize)>,
    ) -> Result<Vec<Index>> {
        assert!(!auto_indices.is_empty());
        let mut indices = Vec::with_capacity(auto_indices.len());
        let mut auto_indices = auto_indices.into_iter();
        let has_primary_key_index =
            table.get_rowid_alias_column().is_none() && !table.primary_key_columns.is_empty();
        if has_primary_key_index {
            let (index_name, root_page) = auto_indices.next().expect(
                "number of auto_indices in schema should be same number of indices calculated",
            );
            let primary_keys = table
                .primary_key_columns
                .iter()
                .map(|(col_name, order)| {
                    let Some((pos_in_table, _)) = table.get_column(col_name) else {
                        panic!(
                            "Column {} is in index {} but not found in table {}",
                            col_name, index_name, table.name
                        );
                    };
                    let (_, column) = table.get_column(col_name).unwrap();
                    IndexColumn {
                        name: normalize_ident(col_name),
                        order: *order,
                        pos_in_table,
                        collation: column.collation,
                        default: column.default.clone(),
                    }
                })
                .collect::<Vec<_>>();
            indices.push(Index {
                name: normalize_ident(index_name.as_str()),
                table_name: table.name.clone(),
                root_page,
                columns: primary_keys,
                unique: true,
                ephemeral: false,
                has_rowid: table.has_rowid,
            });
        }
        let unique_indices = table
            .columns
            .iter()
            .enumerate()
            .filter_map(|(pos_in_table, col)| {
                if col.unique {
                    let col_name = col.name.as_ref().unwrap();
                    if has_primary_key_index && table.primary_key_columns.len() == 1
                        && &table.primary_key_columns.first().as_ref().unwrap().0
                            == col_name
                    {
                        return None;
                    }
                    let (index_name, root_page) = auto_indices
                        .next()
                        .expect(
                            "number of auto_indices in schema should be same number of indices calculated",
                        );
                    let (_, column) = table.get_column(col_name).unwrap();
                    Some(Index {
                        name: normalize_ident(index_name.as_str()),
                        table_name: table.name.clone(),
                        root_page,
                        columns: vec![
                            IndexColumn { name : normalize_ident(col_name), order :
                            SortOrder::Asc, pos_in_table, collation : column.collation,
                            default : column.default.clone(), }
                        ],
                        unique: true,
                        ephemeral: false,
                        has_rowid: table.has_rowid,
                    })
                } else {
                    None
                }
            });
        indices.extend(unique_indices);
        if table.primary_key_columns.is_empty() && indices.is_empty() && table.unique_sets.is_none()
        {
            return Err(crate::LimboError::InternalError(
                "Cannot create automatic index for table without primary key or unique constraint"
                    .to_string(),
            ));
        }
        if table.get_rowid_alias_column().is_some()
            && indices.is_empty()
            && table.unique_sets.is_none()
        {
            panic!(
                "should not create an automatic index on table with a single column as rowid_alias and no UNIQUE columns"
            );
        }
        if let Some(unique_sets) = table.unique_sets.as_ref() {
            let unique_set_indices = unique_sets
                .iter()
                .filter(|set| {
                    if has_primary_key_index
                        && table.primary_key_columns.len() == set.len()
                        && table.primary_key_columns.iter().all(|col| set.contains(col))
                    {
                        return false;
                    } else {
                        true
                    }
                })
                .map(|set| {
                    let (index_name, root_page) = auto_indices
                        .next()
                        .expect(
                            "number of auto_indices in schema should be same number of indices calculated",
                        );
                    let index_cols = set
                        .iter()
                        .map(|(col_name, order)| {
                            let Some((pos_in_table, _)) = table.get_column(col_name)
                            else {
                                panic!(
                                    "Column {} is in index {} but not found in table {}",
                                    col_name, index_name, table.name
                                );
                            };
                            let (_, column) = table.get_column(col_name).unwrap();
                            IndexColumn {
                                name: normalize_ident(col_name),
                                order: *order,
                                pos_in_table,
                                collation: column.collation,
                                default: column.default.clone(),
                            }
                        });
                    Index {
                        name: normalize_ident(index_name.as_str()),
                        table_name: table.name.clone(),
                        root_page,
                        columns: index_cols.collect(),
                        unique: true,
                        ephemeral: false,
                        has_rowid: table.has_rowid,
                    }
                });
            indices.extend(unique_set_indices);
        }
        if auto_indices.next().is_some() {
            panic!("number of auto_indices in schema should be same number of indices calculated");
        }
        Ok(indices)
    }
    /// Given a column position in the table, return the position in the index.
    /// Returns None if the column is not found in the index.
    /// For example, given:
    /// CREATE TABLE t(a, b, c)
    /// CREATE INDEX idx ON t(b)
    /// then column_table_pos_to_index_pos(1) returns Some(0)
    pub fn column_table_pos_to_index_pos(&self, table_pos: usize) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.pos_in_table == table_pos)
    }

    /// Create a synthetic `Index` representing the implicit PK index of a WITHOUT ROWID table.
    ///
    /// WITHOUT ROWID tables store their data in an index-format B-Tree where the primary-key
    /// columns are the key and the full row (all columns) is the record payload.  No separate
    /// sqlite_schema entry is written for this implicit index.  The synthetic object is used
    /// to open cursors with `CursorType::BTreeIndex` so the execution layer uses index-format
    /// B-Tree access (seek by PK, read all columns from the stored record).
    ///
    /// Constraint: PK column(s) must appear first in the table's column list (position 0, 1, …
    /// in declaration order equals physical position in the stored record).  This matches the
    /// SQLite recommendation for WITHOUT ROWID tables and is the only layout we support.
    ///
    /// **All table columns are included** in the synthetic index's `columns` list (in
    /// declaration order) so that helpers like `emit_column` can look up default values
    /// for every column by its position in the table.  The first `pk_count` columns are
    /// the primary-key columns; the B-Tree cursor's key comparison uses only the first
    /// `num_cols` values of the stored record (where `num_cols = index.columns.len()` —
    /// i.e. all columns — when `has_rowid = false`).
    ///
    /// For **seek operations** (e.g. `NoConflict`), the caller supplies `num_regs =
    /// pk_count` so that `indexbtree_move_to` compares only the first `pk_count` values,
    /// which is the PK key.  Full-scan reads (Rewind / Next / Column) naturally access
    /// all values because the record payload contains the full row.
    pub fn synthetic_for_without_rowid(table: &BTreeTable) -> std::sync::Arc<Index> {
        // All columns in declaration order, using the declaration index as `pos_in_table`.
        // PK columns must already be at the front (validated at CREATE TABLE time).
        let all_columns: Vec<IndexColumn> = table
            .columns
            .iter()
            .enumerate()
            .map(|(pos_in_table, column)| {
                // Determine sort order: use the order from primary_key_columns if this
                // column is part of the PK, otherwise default to Asc.
                let order = table
                    .primary_key_columns
                    .iter()
                    .find(|(pk_name, _)| {
                        column
                            .name
                            .as_ref()
                            .is_some_and(|n| normalize_ident(n) == normalize_ident(pk_name))
                    })
                    .map(|(_, ord)| *ord)
                    .unwrap_or(limbo_sqlite3_parser::ast::SortOrder::Asc);
                IndexColumn {
                    name: column
                        .name
                        .as_deref()
                        .map(normalize_ident)
                        .unwrap_or_default(),
                    order,
                    pos_in_table,
                    collation: column.collation,
                    default: column.default.clone(),
                }
            })
            .collect();
        std::sync::Arc::new(Index {
            // Use a synthetic name that cannot collide with user-created objects.
            name: format!("__without_rowid_pk_{}", table.name),
            table_name: table.name.clone(),
            // The synthetic index shares the table's root page — it IS the table.
            root_page: table.root_page,
            columns: all_columns,
            unique: true,
            ephemeral: false,
            has_rowid: false,
        })
    }
}
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct IndexColumn {
    pub name: String,
    pub order: SortOrder,
    /// the position of the column in the source table.
    /// for example:
    /// CREATE TABLE t(a,b,c)
    /// CREATE INDEX idx ON t(b)
    /// b.pos_in_table == 1
    pub pos_in_table: usize,
    pub collation: Option<CollationSeq>,
    pub default: Option<Expr>,
}
