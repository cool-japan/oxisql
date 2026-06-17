//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use super::column::{Column, Type};
use super::table::BTreeTable;

pub(super) const SCHEMA_TABLE_NAME: &str = "sqlite_schema";
pub(super) const SCHEMA_TABLE_NAME_ALT: &str = "sqlite_master";
pub fn sqlite_schema_table() -> BTreeTable {
    BTreeTable {
        root_page: 1,
        name: "sqlite_schema".to_string(),
        has_rowid: true,
        is_strict: false,
        primary_key_columns: vec![],
        columns: vec![
            Column {
                name: Some("type".to_string()),
                ty: Type::Text,
                ty_str: "TEXT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            },
            Column {
                name: Some("name".to_string()),
                ty: Type::Text,
                ty_str: "TEXT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            },
            Column {
                name: Some("tbl_name".to_string()),
                ty: Type::Text,
                ty_str: "TEXT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            },
            Column {
                name: Some("rootpage".to_string()),
                ty: Type::Integer,
                ty_str: "INT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            },
            Column {
                name: Some("sql".to_string()),
                ty: Type::Text,
                ty_str: "TEXT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            },
        ],
        unique_sets: None,
        foreign_keys: vec![],
    }
}
