//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::translate::collate::CollationSeq;
use crate::LimboError;
use crate::Result;
use core::fmt;
use limbo_sqlite3_parser::ast::{self, ColumnDefinition, Expr, ResolveType};

/// # SQLite Column Type Affinities
///
/// Each column in an SQLite 3 database is assigned one of the following type affinities:
///
/// - **TEXT**
/// - **NUMERIC**
/// - **INTEGER**
/// - **REAL**
/// - **BLOB**
///
/// > **Note:** Historically, the "BLOB" type affinity was called "NONE". However, this term was renamed to avoid confusion with "no affinity".
///
/// ## Affinity Descriptions
///
/// ### **TEXT**
/// - Stores data using the NULL, TEXT, or BLOB storage classes.
/// - Numerical data inserted into a column with TEXT affinity is converted into text form before being stored.
/// - **Example:**
///   ```sql
///   CREATE TABLE example (col TEXT);
///   INSERT INTO example (col) VALUES (123); -- Stored as '123' (text)
///   SELECT typeof(col) FROM example; -- Returns 'text'
///   ```
///
/// ### **NUMERIC**
/// - Can store values using all five storage classes.
/// - Text data is converted to INTEGER or REAL (in that order of preference) if it is a well-formed integer or real literal.
/// - If the text represents an integer too large for a 64-bit signed integer, it is converted to REAL.
/// - If the text is not a well-formed literal, it is stored as TEXT.
/// - Hexadecimal integer literals are stored as TEXT for historical compatibility.
/// - Floating-point values that can be exactly represented as integers are converted to integers.
/// - **Example:**
///   ```sql
///   CREATE TABLE example (col NUMERIC);
///   INSERT INTO example (col) VALUES ('3.0e+5'); -- Stored as 300000 (integer)
///   SELECT typeof(col) FROM example; -- Returns 'integer'
///   ```
///
/// ### **INTEGER**
/// - Behaves like NUMERIC affinity but differs in `CAST` expressions.
/// - **Example:**
///   ```sql
///   CREATE TABLE example (col INTEGER);
///   INSERT INTO example (col) VALUES (4.0); -- Stored as 4 (integer)
///   SELECT typeof(col) FROM example; -- Returns 'integer'
///   ```
///
/// ### **REAL**
/// - Similar to NUMERIC affinity but forces integer values into floating-point representation.
/// - **Optimization:** Small floating-point values with no fractional component may be stored as integers on disk to save space. This is invisible at the SQL level.
/// - **Example:**
///   ```sql
///   CREATE TABLE example (col REAL);
///   INSERT INTO example (col) VALUES (4); -- Stored as 4.0 (real)
///   SELECT typeof(col) FROM example; -- Returns 'real'
///   ```
///
/// ### **BLOB**
/// - Does not prefer any storage class.
/// - No coercion is performed between storage classes.
/// - **Example:**
///   ```sql
///   CREATE TABLE example (col BLOB);
///   INSERT INTO example (col) VALUES (x'1234'); -- Stored as a binary blob
///   SELECT typeof(col) FROM example; -- Returns 'blob'
///   ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Affinity {
    Integer,
    Text,
    Blob,
    Real,
    Numeric,
}
impl Affinity {
    /// This is meant to be used in opcodes like Eq, which state:
    ///
    /// "The SQLITE_AFF_MASK portion of P5 must be an affinity character - SQLITE_AFF_TEXT, SQLITE_AFF_INTEGER, and so forth.
    /// An attempt is made to coerce both inputs according to this affinity before the comparison is made.
    /// If the SQLITE_AFF_MASK is 0x00, then numeric affinity is used.
    /// Note that the affinity conversions are stored back into the input registers P1 and P3.
    /// So this opcode can cause persistent changes to registers P1 and P3.""
    pub fn aff_mask(&self) -> char {
        match self {
            Affinity::Integer => SQLITE_AFF_INTEGER,
            Affinity::Text => SQLITE_AFF_TEXT,
            Affinity::Blob => SQLITE_AFF_NONE,
            Affinity::Real => SQLITE_AFF_REAL,
            Affinity::Numeric => SQLITE_AFF_NUMERIC,
        }
    }
    pub fn from_char(char: char) -> Result<Self> {
        match char {
            SQLITE_AFF_INTEGER => Ok(Affinity::Integer),
            SQLITE_AFF_TEXT => Ok(Affinity::Text),
            SQLITE_AFF_NONE => Ok(Affinity::Blob),
            SQLITE_AFF_REAL => Ok(Affinity::Real),
            SQLITE_AFF_NUMERIC => Ok(Affinity::Numeric),
            _ => Err(LimboError::InternalError(format!(
                "Invalid affinity character: {}",
                char
            ))),
        }
    }
    pub fn to_char_code(&self) -> u8 {
        self.aff_mask() as u8
    }
    pub fn from_char_code(code: u8) -> Result<Self, LimboError> {
        Self::from_char(code as char)
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Affinity::Integer | Affinity::Real | Affinity::Numeric)
    }
    pub fn has_affinity(&self) -> bool {
        !matches!(self, Affinity::Blob)
    }
}
#[derive(Debug, Clone)]
pub struct Column {
    pub name: Option<String>,
    pub ty: Type,
    pub ty_str: String,
    pub primary_key: bool,
    pub is_rowid_alias: bool,
    pub notnull: bool,
    pub default: Option<Expr>,
    pub unique: bool,
    /// The `ON CONFLICT <action>` resolution declared on this column's own
    /// `UNIQUE` constraint (e.g. `x INTEGER UNIQUE ON CONFLICT REPLACE`).
    /// Meaningless when `unique` is `false`. Defaults to [`ResolveType::Abort`]
    /// when the constraint didn't specify a clause, matching SQLite's own
    /// default. This is independent from a statement-level
    /// `INSERT/UPDATE OR <action>`, which takes precedence over this value
    /// when present (see `translate::insert`/`translate::emitter`).
    pub unique_conflict: ResolveType,
    pub collation: Option<CollationSeq>,
    /// Whether this column is declared `GENERATED ALWAYS AS (...)`, either
    /// `STORED` or `VIRTUAL` (see [`ast::ColumnConstraint::Generated`]).
    /// SQLite rejects a `SET`/`DO UPDATE SET` target on a generated column
    /// identically regardless of the `STORED`/`VIRTUAL` sub-kind, so a single
    /// flag is sufficient for that check; the sub-kind itself isn't tracked
    /// because nothing yet needs to distinguish them.
    pub is_generated: bool,
}
impl Column {
    pub fn affinity(&self) -> Affinity {
        affinity(&self.ty_str)
    }
}
impl From<ColumnDefinition> for Column {
    fn from(value: ColumnDefinition) -> Self {
        let ast::Name(name) = value.col_name;
        let mut default = None;
        let mut notnull = false;
        let mut primary_key = false;
        let mut unique = false;
        let mut unique_conflict = ResolveType::Abort;
        let mut collation = None;
        let mut is_generated = false;
        for ast::NamedColumnConstraint { constraint, .. } in value.constraints {
            match constraint {
                ast::ColumnConstraint::PrimaryKey { .. } => primary_key = true,
                ast::ColumnConstraint::NotNull { .. } => notnull = true,
                ast::ColumnConstraint::Unique(on_conflict) => {
                    unique = true;
                    if let Some(resolve) = on_conflict {
                        unique_conflict = resolve;
                    }
                }
                ast::ColumnConstraint::Default(expr) => {
                    default.replace(expr);
                }
                ast::ColumnConstraint::Collate { collation_name } => {
                    collation.replace(
                        CollationSeq::new(&collation_name.0)
                            .expect("collation should have been set correctly in create table"),
                    );
                }
                ast::ColumnConstraint::Generated { .. } => {
                    is_generated = true;
                }
                _ => {}
            };
        }
        let ty = match value.col_type {
            Some(ref data_type) => {
                let type_name = data_type.name.clone().to_uppercase();
                if type_name.contains("INT") {
                    Type::Integer
                } else if type_name.contains("CHAR")
                    || type_name.contains("CLOB")
                    || type_name.contains("TEXT")
                {
                    Type::Text
                } else if type_name.contains("BLOB") || type_name.is_empty() {
                    Type::Blob
                } else if type_name.contains("REAL")
                    || type_name.contains("FLOA")
                    || type_name.contains("DOUB")
                {
                    Type::Real
                } else {
                    Type::Numeric
                }
            }
            None => Type::Null,
        };
        let ty_str = value
            .col_type
            .map(|t| t.name.to_string())
            .unwrap_or_default();
        Column {
            name: Some(name),
            ty,
            default,
            notnull,
            ty_str,
            primary_key,
            is_rowid_alias: primary_key && matches!(ty, Type::Integer),
            unique,
            unique_conflict,
            collation,
            is_generated,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Type {
    Null,
    Text,
    Numeric,
    Integer,
    Real,
    Blob,
}
impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Null => "",
            Self::Text => "TEXT",
            Self::Numeric => "NUMERIC",
            Self::Integer => "INTEGER",
            Self::Real => "REAL",
            Self::Blob => "BLOB",
        };
        write!(f, "{}", s)
    }
}
/// 3.1. Determination Of Column Affinity
/// For tables not declared as STRICT, the affinity of a column is determined by the declared type of the column, according to the following rules in the order shown:
///
/// If the declared type contains the string "INT" then it is assigned INTEGER affinity.
///
/// If the declared type of the column contains any of the strings "CHAR", "CLOB", or "TEXT" then that column has TEXT affinity. Notice that the type VARCHAR contains the string "CHAR" and is thus assigned TEXT affinity.
///
/// If the declared type for a column contains the string "BLOB" or if no type is specified then the column has affinity BLOB.
///
/// If the declared type for a column contains any of the strings "REAL", "FLOA", or "DOUB" then the column has REAL affinity.
///
/// Otherwise, the affinity is NUMERIC.
///
/// Note that the order of the rules for determining column affinity is important. A column whose declared type is "CHARINT" will match both rules 1 and 2 but the first rule takes precedence and so the column affinity will be INTEGER.
pub fn affinity(datatype: &str) -> Affinity {
    if datatype.contains("INT") {
        return Affinity::Integer;
    }
    if datatype.contains("CHAR") || datatype.contains("CLOB") || datatype.contains("TEXT") {
        return Affinity::Text;
    }
    if datatype.contains("BLOB") || datatype.is_empty() || datatype.contains("ANY") {
        return Affinity::Blob;
    }
    if datatype.contains("REAL") || datatype.contains("FLOA") || datatype.contains("DOUB") {
        return Affinity::Real;
    }
    Affinity::Numeric
}
pub const SQLITE_AFF_NONE: char = 'A';
pub const SQLITE_AFF_TEXT: char = 'B';
pub const SQLITE_AFF_NUMERIC: char = 'C';
pub const SQLITE_AFF_INTEGER: char = 'D';
pub const SQLITE_AFF_REAL: char = 'E';
