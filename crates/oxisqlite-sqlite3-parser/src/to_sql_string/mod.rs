//! ToSqlString trait definition and implementations

mod expr;
mod stmt;

use crate::ast::TableReferenceId;

/// Context to be used in ToSqlString
pub trait ToSqlContext {
    /// Given an id, get the table name
    ///
    /// Currently not considering aliases
    fn get_table_name(&self, id: TableReferenceId) -> &str;
    /// Given a table id and a column index, get the column name
    fn get_column_name(&self, table_id: TableReferenceId, col_idx: usize) -> &str;
}

/// Trait to convert an ast to a string
pub trait ToSqlString {
    /// Convert the given value to String
    fn to_sql_string<C: ToSqlContext>(&self, context: &C) -> String;
}

impl<T: ToSqlString> ToSqlString for Box<T> {
    fn to_sql_string<C: ToSqlContext>(&self, context: &C) -> String {
        T::to_sql_string(&self, context)
    }
}

#[cfg(test)]
mod tests {
    use super::ToSqlContext;

    struct TestContext;

    impl ToSqlContext for TestContext {
        fn get_column_name(
            &self,
            _table_id: crate::ast::TableReferenceId,
            _col_idx: usize,
        ) -> &str {
            "placeholder_column"
        }

        fn get_table_name(&self, _id: crate::ast::TableReferenceId) -> &str {
            "placeholder_table"
        }
    }
}
