//! Auto-generated test module (consolidated from inline `#[cfg(test)] mod` blocks)

use crate::Result;
use limbo_sqlite3_parser::ast::SortOrder;

use super::*;

#[cfg(test)]
mod tests_2 {
    use super::*;
    use crate::LimboError;
    #[test]
    pub fn test_has_rowid_true() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        assert!(table.has_rowid, "has_rowid should be set to true");
        Ok(())
    }
    #[test]
    pub fn test_has_rowid_false() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT) WITHOUT ROWID;"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        assert!(!table.has_rowid, "has_rowid should be set to false");
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_single_text() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a TEXT PRIMARY KEY, b TEXT);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            !table.column_is_rowid_alias(column),
            "column 'a´ has type different than INTEGER so can't be a rowid alias"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_single_integer() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            table.column_is_rowid_alias(column),
            "column 'a´ should be a rowid alias"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_single_integer_separate_primary_key_definition() -> Result<()>
    {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, PRIMARY KEY(a));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            table.column_is_rowid_alias(column),
            "column 'a´ should be a rowid alias"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_single_integer_separate_primary_key_definition_without_rowid(
    ) -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, PRIMARY KEY(a)) WITHOUT ROWID;"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            !table.column_is_rowid_alias(column),
            "column 'a´ shouldn't be a rowid alias because table has no rowid"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_single_integer_without_rowid() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT) WITHOUT ROWID;"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            !table.column_is_rowid_alias(column),
            "column 'a´ shouldn't be a rowid alias because table has no rowid"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_inline_composite_primary_key() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT PRIMARY KEY);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            !table.column_is_rowid_alias(column),
            "column 'a´ shouldn't be a rowid alias because table has composite primary key"
        );
        Ok(())
    }
    #[test]
    pub fn test_column_is_rowid_alias_separate_composite_primary_key_definition() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, PRIMARY KEY(a, b));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            !table.column_is_rowid_alias(column),
            "column 'a´ shouldn't be a rowid alias because table has composite primary key"
        );
        Ok(())
    }
    #[test]
    pub fn uppercase_table_level_pk_is_rowid_alias() -> Result<()> {
        let table =
            BTreeTable::from_sql("CREATE TABLE t (A INTEGER NOT NULL, PRIMARY KEY(A));", 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            column.primary_key,
            "uppercase table-level PK -> primary_key"
        );
        assert!(column.is_rowid_alias, "single INTEGER PK -> rowid alias");
        assert!(table.column_is_rowid_alias(column));
        Ok(())
    }
    #[test]
    pub fn quoted_table_level_pk_is_rowid_alias() -> Result<()> {
        let table = BTreeTable::from_sql(
            r#"CREATE TABLE t ("A" INTEGER NOT NULL, PRIMARY KEY("A"));"#,
            0,
        )?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "quoted table-level PK -> primary_key");
        assert!(column.is_rowid_alias, "single INTEGER PK -> rowid alias");
        Ok(())
    }
    #[test]
    pub fn lowercase_table_level_pk_still_works() -> Result<()> {
        let table =
            BTreeTable::from_sql("CREATE TABLE t (a INTEGER NOT NULL, PRIMARY KEY(a));", 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(
            column.primary_key,
            "lowercase table-level PK -> primary_key"
        );
        assert!(column.is_rowid_alias, "single INTEGER PK -> rowid alias");
        Ok(())
    }
    #[test]
    pub fn composite_table_level_pk_not_rowid_alias() -> Result<()> {
        let table =
            BTreeTable::from_sql("CREATE TABLE t (a INTEGER, b TEXT, PRIMARY KEY(a, b));", 0)?;
        let col_a = table.get_column("a").unwrap().1;
        let col_b = table.get_column("b").unwrap().1;
        assert!(
            col_a.primary_key && col_b.primary_key,
            "both columns are PK"
        );
        assert!(
            !col_a.is_rowid_alias,
            "composite PK 'a' is not a rowid alias"
        );
        assert!(
            !col_b.is_rowid_alias,
            "composite PK 'b' is not a rowid alias"
        );
        Ok(())
    }
    #[test]
    pub fn column_level_int_pk_unchanged() -> Result<()> {
        let table = BTreeTable::from_sql("CREATE TABLE t (a INTEGER PRIMARY KEY, b TEXT);", 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column-level INTEGER PK -> primary_key");
        assert!(
            column.is_rowid_alias,
            "column-level INTEGER PK -> rowid alias"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_inline_single() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT, c REAL);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(!column.primary_key, "column 'b' shouldn't be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![("a".to_string(), SortOrder::Asc)],
            table.primary_key_columns,
            "primary key column names should be ['a']"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_inline_multiple() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT PRIMARY KEY, c REAL);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(column.primary_key, "column 'b' shouldn be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![
                ("a".to_string(), SortOrder::Asc),
                ("b".to_string(), SortOrder::Asc)
            ],
            table.primary_key_columns,
            "primary key column names should be ['a', 'b']"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_separate_single() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, c REAL, PRIMARY KEY(a desc));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(!column.primary_key, "column 'b' shouldn't be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![("a".to_string(), SortOrder::Desc)],
            table.primary_key_columns,
            "primary key column names should be ['a']"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_separate_multiple() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, c REAL, PRIMARY KEY(a, b desc));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(column.primary_key, "column 'b' shouldn be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![
                ("a".to_string(), SortOrder::Asc),
                ("b".to_string(), SortOrder::Desc)
            ],
            table.primary_key_columns,
            "primary key column names should be ['a', 'b']"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_separate_single_quoted() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, c REAL, PRIMARY KEY('a'));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(!column.primary_key, "column 'b' shouldn't be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![("a".to_string(), SortOrder::Asc)],
            table.primary_key_columns,
            "primary key column names should be ['a']"
        );
        Ok(())
    }
    #[test]
    pub fn test_primary_key_separate_single_doubly_quoted() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, c REAL, PRIMARY KEY("a"));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert!(column.primary_key, "column 'a' should be a primary key");
        let column = table.get_column("b").unwrap().1;
        assert!(!column.primary_key, "column 'b' shouldn't be a primary key");
        let column = table.get_column("c").unwrap().1;
        assert!(!column.primary_key, "column 'c' shouldn't be a primary key");
        assert_eq!(
            vec![("a".to_string(), SortOrder::Asc)],
            table.primary_key_columns,
            "primary key column names should be ['a']"
        );
        Ok(())
    }
    #[test]
    pub fn test_default_value() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER DEFAULT 23);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        let default = column.default.clone().unwrap();
        assert_eq!(default.to_string(), "23");
        Ok(())
    }
    #[test]
    pub fn test_col_notnull() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER NOT NULL);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.notnull, true);
        Ok(())
    }
    #[test]
    pub fn test_col_notnull_negative() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.notnull, false);
        Ok(())
    }
    #[test]
    pub fn test_col_type_string_integer() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a InTeGeR);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.ty_str, "INTEGER");
        Ok(())
    }
    #[test]
    pub fn test_col_type_string_int() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a InT);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.ty_str, "INT");
        Ok(())
    }
    #[test]
    pub fn test_col_type_string_blob() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a bLoB);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.ty_str, "BLOB");
        Ok(())
    }
    #[test]
    pub fn test_col_type_string_empty() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.ty_str, "");
        Ok(())
    }
    #[test]
    pub fn test_col_type_string_some_nonsense() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a someNonsenseName);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let column = table.get_column("a").unwrap().1;
        assert_eq!(column.ty_str, "someNonsenseName");
        Ok(())
    }
    #[test]
    pub fn test_sqlite_schema() {
        let expected = r#"CREATE TABLE sqlite_schema ( type TEXT, name TEXT, tbl_name TEXT, rootpage INTEGER, sql TEXT )"#;
        let actual = sqlite_schema_table().to_sql();
        assert_eq!(expected, actual);
    }
    #[test]
    #[should_panic]
    fn test_automatic_index_single_column() {
        let sql = r#"CREATE TABLE t1 (a INTEGER PRIMARY KEY, b TEXT);"#;
        let table = BTreeTable::from_sql(sql, 0).unwrap();
        let _index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )
        .unwrap();
    }
    #[test]
    fn test_automatic_index_composite_key() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT, PRIMARY KEY(a, b));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )?;
        assert!(index.len() == 1);
        let index = index.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 2);
        assert_eq!(index.columns[0].name, "a");
        assert_eq!(index.columns[1].name, "b");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        assert!(matches!(index.columns[1].order, SortOrder::Asc));
        Ok(())
    }
    #[test]
    fn test_automatic_index_no_primary_key() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a INTEGER, b TEXT);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let result = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        );
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), LimboError::InternalError(msg) if msg
            .contains("without primary key"))
        );
        Ok(())
    }
    #[test]
    #[should_panic]
    fn test_automatic_index_nonexistent_column() {
        let table = BTreeTable {
            root_page: 0,
            name: "t1".to_string(),
            has_rowid: true,
            is_strict: false,
            primary_key_columns: vec![("nonexistent".to_string(), SortOrder::Asc)],
            columns: vec![Column {
                name: Some("a".to_string()),
                ty: Type::Integer,
                ty_str: "INT".to_string(),
                primary_key: false,
                is_rowid_alias: false,
                notnull: false,
                default: None,
                unique: false,
                collation: None,
            }],
            unique_sets: None,
            foreign_keys: vec![],
        };
        let _result = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        );
    }
    #[test]
    fn test_automatic_index_unique_column() -> Result<()> {
        let sql = r#"CREATE table t1 (x INTEGER, y INTEGER UNIQUE);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )?;
        assert!(index.len() == 1);
        let index = index.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 1);
        assert_eq!(index.columns[0].name, "y");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        Ok(())
    }
    #[test]
    fn test_automatic_index_pkey_unique_column() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (x PRIMARY KEY, y UNIQUE);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let auto_indices = vec![
            ("sqlite_autoindex_t1_1".to_string(), 2),
            ("sqlite_autoindex_t1_2".to_string(), 3),
        ];
        let indices = Index::automatic_from_primary_key_and_unique(&table, auto_indices.clone())?;
        assert!(indices.len() == auto_indices.len());
        for (pos, index) in indices.iter().enumerate() {
            let (index_name, root_page) = &auto_indices[pos];
            assert_eq!(index.name, *index_name);
            assert_eq!(index.table_name, "t1");
            assert_eq!(index.root_page, *root_page);
            assert!(index.unique);
            assert_eq!(index.columns.len(), 1);
            if pos == 0 {
                assert_eq!(index.columns[0].name, "x");
            } else if pos == 1 {
                assert_eq!(index.columns[0].name, "y");
            }
            assert!(matches!(index.columns[0].order, SortOrder::Asc));
        }
        Ok(())
    }
    #[test]
    fn test_automatic_index_pkey_many_unique_columns() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a PRIMARY KEY, b UNIQUE, c, d, UNIQUE(c, d));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let auto_indices = vec![
            ("sqlite_autoindex_t1_1".to_string(), 2),
            ("sqlite_autoindex_t1_2".to_string(), 3),
            ("sqlite_autoindex_t1_2".to_string(), 4),
        ];
        let indices = Index::automatic_from_primary_key_and_unique(&table, auto_indices.clone())?;
        assert!(indices.len() == auto_indices.len());
        for (pos, index) in indices.iter().enumerate() {
            let (index_name, root_page) = &auto_indices[pos];
            assert_eq!(index.name, *index_name);
            assert_eq!(index.table_name, "t1");
            assert_eq!(index.root_page, *root_page);
            assert!(index.unique);
            if pos == 0 {
                assert_eq!(index.columns.len(), 1);
                assert_eq!(index.columns[0].name, "a");
            } else if pos == 1 {
                assert_eq!(index.columns.len(), 1);
                assert_eq!(index.columns[0].name, "b");
            } else if pos == 2 {
                assert_eq!(index.columns.len(), 2);
                assert_eq!(index.columns[0].name, "c");
                assert_eq!(index.columns[1].name, "d");
            }
            assert!(matches!(index.columns[0].order, SortOrder::Asc));
        }
        Ok(())
    }
    #[test]
    fn test_automatic_index_unique_set_dedup() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a, b, UNIQUE(a, b), UNIQUE(a, b));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )?;
        assert!(index.len() == 1);
        let index = index.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 2);
        assert_eq!(index.columns[0].name, "a");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        assert_eq!(index.columns[1].name, "b");
        assert!(matches!(index.columns[1].order, SortOrder::Asc));
        Ok(())
    }
    #[test]
    fn test_automatic_index_primary_key_is_unique() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a primary key unique);"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )?;
        assert!(index.len() == 1);
        let index = index.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 1);
        assert_eq!(index.columns[0].name, "a");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        Ok(())
    }
    #[test]
    fn test_automatic_index_primary_key_is_unique_and_composite() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a, b, PRIMARY KEY(a, b), UNIQUE(a, b));"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut index = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![("sqlite_autoindex_t1_1".to_string(), 2)],
        )?;
        assert!(index.len() == 1);
        let index = index.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 2);
        assert_eq!(index.columns[0].name, "a");
        assert_eq!(index.columns[1].name, "b");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        Ok(())
    }
    #[test]
    fn test_automatic_index_unique_and_a_pk() -> Result<()> {
        let sql = r#"CREATE TABLE t1 (a NUMERIC UNIQUE UNIQUE,  b TEXT PRIMARY KEY)"#;
        let table = BTreeTable::from_sql(sql, 0)?;
        let mut indexes = Index::automatic_from_primary_key_and_unique(
            &table,
            vec![
                ("sqlite_autoindex_t1_1".to_string(), 2),
                ("sqlite_autoindex_t1_2".to_string(), 3),
            ],
        )?;
        assert!(indexes.len() == 2);
        let index = indexes.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_2");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 3);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 1);
        assert_eq!(index.columns[0].name, "a");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        let index = indexes.pop().unwrap();
        assert_eq!(index.name, "sqlite_autoindex_t1_1");
        assert_eq!(index.table_name, "t1");
        assert_eq!(index.root_page, 2);
        assert!(index.unique);
        assert_eq!(index.columns.len(), 1);
        assert_eq!(index.columns[0].name, "b");
        assert!(matches!(index.columns[0].order, SortOrder::Asc));
        Ok(())
    }
}
