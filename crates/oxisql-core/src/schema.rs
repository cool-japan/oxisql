//! Types returned by [`Connection`][crate::Connection] schema-inspection methods.

/// A table visible to the current connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableInfo {
    /// The table name.
    pub name: String,
    /// The schema (namespace) the table belongs to, if the backend supports it.
    pub schema: Option<String>,
    /// The kind of table object.
    pub table_type: TableType,
}

/// The kind of table object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableType {
    /// A regular base table.
    Base,
    /// A view.
    View,
    /// Something else (foreign table, materialised view, etc.).
    Other(String),
}

impl From<&str> for TableType {
    fn from(s: &str) -> Self {
        match s.to_ascii_uppercase().as_str() {
            "BASE TABLE" | "TABLE" => TableType::Base,
            "VIEW" => TableType::View,
            other => TableType::Other(other.to_string()),
        }
    }
}

/// A column in a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    /// The column name.
    pub name: String,
    /// The 1-based position of the column in the table definition.
    pub ordinal_position: u32,
    /// The SQL data type name as reported by the backend.
    pub data_type: String,
    /// Whether the column accepts NULL values.
    pub nullable: bool,
    /// The column default expression, if any.
    pub default: Option<String>,
    /// Maximum character/byte length, if applicable.
    pub max_length: Option<u64>,
    /// Numeric precision, if applicable.
    pub numeric_precision: Option<u32>,
    /// Numeric scale, if applicable.
    pub numeric_scale: Option<u32>,
}

/// An index on a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    /// The index name.
    pub name: String,
    /// Ordered list of column names that form the index key.
    pub columns: Vec<String>,
    /// Whether the index enforces uniqueness.
    pub unique: bool,
    /// Whether this index is the table's primary key.
    pub primary: bool,
}

/// A foreign key constraint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    /// The constraint name.
    pub constraint_name: String,
    /// The column on the referencing (child) table.
    pub column: String,
    /// The referenced (parent) table name.
    pub foreign_table: String,
    /// The referenced column in the parent table.
    pub foreign_column: String,
}
