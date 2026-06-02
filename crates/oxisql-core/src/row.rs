//! [`Row`], [`RowSet`], and the [`FromValue`] extraction trait.

use std::fmt;

use crate::{OxiSqlError, Value};

// ── FromValue trait ─────────────────────────────────────────────────────────

/// Extract a typed value from a [`Value`] enum variant.
///
/// Provides type-safe conversions from `Value` to concrete Rust types.
/// Returns [`OxiSqlError::TypeMismatch`] when the variant does not match
/// the requested type.
///
/// # Null handling
///
/// - `FromValue` for `Option<T>` returns `Ok(None)` for `Value::Null`.
/// - `FromValue` for non-`Option` types returns `TypeMismatch` for `Null`.
pub trait FromValue: Sized {
    /// Try to extract `Self` from a [`Value`].
    fn from_value(v: &Value) -> Result<Self, OxiSqlError>;
}

impl FromValue for bool {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::Bool(b) => Ok(*b),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "Bool",
                got: other.type_name(),
            }),
        }
    }
}

impl FromValue for i32 {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::I64(n) => i32::try_from(*n).map_err(|_| OxiSqlError::TypeMismatch {
                expected: "I64 (within i32 range)",
                got: "I64 (out of i32 range)",
            }),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "I64",
                got: other.type_name(),
            }),
        }
    }
}

impl FromValue for i64 {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::I64(n) => Ok(*n),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "I64",
                got: other.type_name(),
            }),
        }
    }
}

impl FromValue for f64 {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::F64(n) => Ok(*n),
            // Allow I64 -> f64 coercion for convenience
            Value::I64(n) => Ok(*n as f64),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "F64",
                got: other.type_name(),
            }),
        }
    }
}

impl FromValue for String {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::Text(s) => Ok(s.clone()),
            Value::Json(s) => Ok(s.clone()),
            Value::Decimal(s) => Ok(s.clone()),
            // Format UUID as the standard hyphenated string representation.
            Value::Uuid(_) => Ok(format!("{v}")),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "Text",
                got: other.type_name(),
            }),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::Blob(b) => Ok(b.clone()),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "Blob",
                got: other.type_name(),
            }),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        if v.is_null() {
            Ok(None)
        } else {
            T::from_value(v).map(Some)
        }
    }
}

impl FromValue for u128 {
    fn from_value(v: &Value) -> Result<Self, OxiSqlError> {
        match v {
            Value::Uuid(u) => Ok(*u),
            other => Err(OxiSqlError::TypeMismatch {
                expected: "Uuid",
                got: other.type_name(),
            }),
        }
    }
}

// ── Row ─────────────────────────────────────────────────────────────────────

/// A single row returned from a query, with named columns.
#[derive(Debug, Clone)]
pub struct Row {
    columns: Vec<String>,
    values: Vec<Value>,
    index: std::collections::HashMap<String, usize>,
}

impl Row {
    /// Construct a [`Row`] from parallel column-name and value vectors.
    ///
    /// # Panics
    ///
    /// Does not panic; callers are responsible for ensuring `columns` and
    /// `values` have the same length.
    pub fn new(columns: Vec<String>, values: Vec<Value>) -> Self {
        let index = columns
            .iter()
            .enumerate()
            .map(|(i, c)| (c.clone(), i))
            .collect();
        Self {
            columns,
            values,
            index,
        }
    }

    /// Look up a value by column name.  Returns `None` if the column does not
    /// exist in this row.
    pub fn get(&self, col: &str) -> Option<&Value> {
        self.index.get(col).and_then(|&i| self.values.get(i))
    }

    /// Look up a value by zero-based column index.
    pub fn get_by_index(&self, i: usize) -> Option<&Value> {
        self.values.get(i)
    }

    /// Type-safe extraction by column name.
    ///
    /// Looks up the column and converts via [`FromValue`].  Returns
    /// [`OxiSqlError::Other`] if the column does not exist, or
    /// [`OxiSqlError::TypeMismatch`] if the value cannot be converted.
    ///
    /// # Example
    ///
    /// ```rust
    /// # use oxisql_core::{Row, Value};
    /// let row = Row::new(vec!["id".into()], vec![Value::I64(42)]);
    /// let id: i64 = row.try_get("id").unwrap();
    /// assert_eq!(id, 42);
    /// ```
    pub fn try_get<T: FromValue>(&self, col: &str) -> Result<T, OxiSqlError> {
        let val = self
            .get(col)
            .ok_or_else(|| OxiSqlError::Other(format!("column '{col}' not found")))?;
        T::from_value(val)
    }

    /// Type-safe extraction by column index.
    ///
    /// Like [`try_get`](Row::try_get) but uses a zero-based index.
    pub fn try_get_by_index<T: FromValue>(&self, i: usize) -> Result<T, OxiSqlError> {
        let val = self
            .get_by_index(i)
            .ok_or_else(|| OxiSqlError::Other(format!("column index {i} out of range")))?;
        T::from_value(val)
    }

    /// Return the ordered column names for this row.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Return the ordered values for this row.
    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Return the number of columns in this row.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Check whether a column value is `NULL`.
    ///
    /// Returns `true` if the column exists and its value is `Value::Null`.
    /// Returns `false` if the column does not exist or has a non-null value.
    pub fn is_null(&self, col: &str) -> bool {
        self.get(col).is_some_and(|v| v.is_null())
    }

    /// Consume the row and return its values, discarding column names.
    pub fn into_values(self) -> Vec<Value> {
        self.values
    }
}

impl fmt::Display for Row {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        for (i, (col, val)) in self.columns.iter().zip(self.values.iter()).enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            write!(f, "{col}: {val}")?;
        }
        write!(f, "}}")
    }
}

// ── RowSet ──────────────────────────────────────────────────────────────────

/// A collection of [`Row`]s with schema metadata.
///
/// Wraps `Vec<Row>` and provides convenience methods for accessing results.
#[derive(Debug, Clone)]
pub struct RowSet {
    /// The column names (shared across all rows).
    columns: Vec<String>,
    /// The rows in this result set.
    rows: Vec<Row>,
}

impl RowSet {
    /// Create a [`RowSet`] from rows.
    ///
    /// Column names are extracted from the first row.  If `rows` is empty,
    /// columns will be empty.
    pub fn from_rows(rows: Vec<Row>) -> Self {
        let columns = rows
            .first()
            .map(|r| r.columns().to_vec())
            .unwrap_or_default();
        Self { columns, rows }
    }

    /// Create a [`RowSet`] with explicit column names.
    pub fn new(columns: Vec<String>, rows: Vec<Row>) -> Self {
        Self { columns, rows }
    }

    /// Return the column names for this result set.
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// Return the rows.
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Return the number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Returns `true` if there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Return the number of columns.
    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// Consume the result set and return the inner rows.
    pub fn into_rows(self) -> Vec<Row> {
        self.rows
    }
}
