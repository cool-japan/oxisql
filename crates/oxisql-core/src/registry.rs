//! [`TypeRegistry`] — bidirectional mapping between SQL type-name strings and
//! [`SqlType`] descriptors, together with default [`Value`] construction.
//!
//! All type-name keys are stored and looked up **case-insensitively** (they
//! are normalised to UPPER CASE internally).  Register custom names with
//! [`TypeRegistry::register`]; look them up with [`TypeRegistry::lookup`].

use std::collections::HashMap;

use crate::Value;

// ── SqlType ──────────────────────────────────────────────────────────────────

/// A SQL column type descriptor.
///
/// This enum covers the standard SQL type vocabulary plus a catch-all
/// [`SqlType::Unknown`] for non-standard type names that have been registered
/// by a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SqlType {
    /// SQL `INTEGER` / `INT4`.
    Integer,
    /// SQL `BIGINT` / `INT8`.
    BigInt,
    /// SQL `SMALLINT` / `INT2`.
    SmallInt,
    /// SQL `REAL` / `FLOAT4`.
    Float,
    /// SQL `DOUBLE PRECISION` / `FLOAT8`.
    Double,
    /// SQL `DECIMAL` / `NUMERIC`.
    Decimal,
    /// SQL `TEXT` / `CLOB`.
    Text,
    /// SQL `VARCHAR(n)` / `CHARACTER VARYING`.  The `Option<u32>` carries the
    /// optional maximum length (`None` = unbounded).
    VarChar(Option<u32>),
    /// SQL `BYTEA` / `BLOB` / `BINARY`.
    Blob,
    /// SQL `BOOLEAN` / `BOOL`.
    Boolean,
    /// SQL `TIMESTAMP` / `TIMESTAMPTZ`.
    Timestamp,
    /// SQL `DATE`.
    Date,
    /// SQL `TIME` / `TIMETZ`.
    Time,
    /// SQL `UUID`.
    Uuid,
    /// SQL `JSON` / `JSONB`.
    Json,
    /// SQL array type (e.g. `INTEGER[]`).  The inner [`SqlType`] describes the
    /// element type.
    Array(Box<SqlType>),
    /// A non-standard type name that is not covered by the variants above.
    Unknown(String),
}

impl SqlType {
    /// Return the canonical SQL name for this type.
    ///
    /// For parameterised types (e.g. `VARCHAR(255)`) the length is included in
    /// the output when present.
    pub fn as_sql_name(&self) -> String {
        match self {
            SqlType::Integer => "INTEGER".into(),
            SqlType::BigInt => "BIGINT".into(),
            SqlType::SmallInt => "SMALLINT".into(),
            SqlType::Float => "REAL".into(),
            SqlType::Double => "DOUBLE PRECISION".into(),
            SqlType::Decimal => "DECIMAL".into(),
            SqlType::Text => "TEXT".into(),
            SqlType::VarChar(None) => "VARCHAR".into(),
            SqlType::VarChar(Some(n)) => format!("VARCHAR({n})"),
            SqlType::Blob => "BYTEA".into(),
            SqlType::Boolean => "BOOLEAN".into(),
            SqlType::Timestamp => "TIMESTAMP".into(),
            SqlType::Date => "DATE".into(),
            SqlType::Time => "TIME".into(),
            SqlType::Uuid => "UUID".into(),
            SqlType::Json => "JSON".into(),
            SqlType::Array(inner) => format!("{}[]", inner.as_sql_name()),
            SqlType::Unknown(s) => s.clone(),
        }
    }

    /// Return the zero/empty default [`Value`] for this type.
    ///
    /// The returned value is the conventional "empty" representation used when
    /// a column default is not otherwise specified:
    ///
    /// | `SqlType`              | `Value`               |
    /// |------------------------|-----------------------|
    /// | Integer / BigInt / … | `Value::I64(0)`       |
    /// | Float / Double        | `Value::F64(0.0)`     |
    /// | Decimal               | `Value::Decimal("0")` |
    /// | Text / VarChar        | `Value::Text("")`     |
    /// | Blob                  | `Value::Blob([])`     |
    /// | Boolean               | `Value::Bool(false)`  |
    /// | Timestamp             | `Value::Timestamp(0)` |
    /// | Date                  | `Value::Date(0)`      |
    /// | Time                  | `Value::Time(0)`      |
    /// | Uuid                  | `Value::Uuid(0)`      |
    /// | Json                  | `Value::Json("{}")`   |
    /// | Array(_)              | `Value::Array([])`    |
    /// | Unknown               | `Value::Null`         |
    pub fn default_value(&self) -> Value {
        match self {
            SqlType::Integer | SqlType::BigInt | SqlType::SmallInt => Value::I64(0),
            SqlType::Float | SqlType::Double => Value::F64(0.0),
            SqlType::Decimal => Value::Decimal("0".into()),
            SqlType::Text | SqlType::VarChar(_) => Value::Text(String::new()),
            SqlType::Blob => Value::Blob(Vec::new()),
            SqlType::Boolean => Value::Bool(false),
            SqlType::Timestamp => Value::Timestamp(0),
            SqlType::Date => Value::Date(0),
            SqlType::Time => Value::Time(0),
            SqlType::Uuid => Value::Uuid(0),
            SqlType::Json => Value::Json("{}".into()),
            SqlType::Array(_) => Value::Array(Vec::new()),
            SqlType::Unknown(_) => Value::Null,
        }
    }
}

// ── TypeRegistry ─────────────────────────────────────────────────────────────

/// Bidirectional registry mapping SQL type-name strings to [`SqlType`]
/// descriptors.
///
/// The registry is pre-populated with all standard SQL type names (and common
/// aliases) via [`TypeRegistry::new`].  Backends can extend it at runtime with
/// [`TypeRegistry::register`].
///
/// # Case handling
///
/// All keys are stored in UPPER CASE.  [`lookup`](TypeRegistry::lookup) and
/// [`register`](TypeRegistry::register) normalise their inputs, so
/// `lookup("integer")` and `lookup("INTEGER")` both work.
pub struct TypeRegistry {
    name_to_type: HashMap<String, SqlType>,
}

impl TypeRegistry {
    /// Create a new registry pre-populated with standard SQL type names.
    pub fn new() -> Self {
        let mut reg = Self {
            name_to_type: HashMap::new(),
        };

        // Integer family
        reg.insert("INTEGER", SqlType::Integer);
        reg.insert("INT", SqlType::Integer);
        reg.insert("INT4", SqlType::Integer);
        reg.insert("BIGINT", SqlType::BigInt);
        reg.insert("INT8", SqlType::BigInt);
        reg.insert("SMALLINT", SqlType::SmallInt);
        reg.insert("INT2", SqlType::SmallInt);

        // Floating-point family
        reg.insert("REAL", SqlType::Float);
        reg.insert("FLOAT4", SqlType::Float);
        reg.insert("FLOAT", SqlType::Float);
        reg.insert("DOUBLE PRECISION", SqlType::Double);
        reg.insert("FLOAT8", SqlType::Double);
        reg.insert("DOUBLE", SqlType::Double);

        // Exact numeric
        reg.insert("DECIMAL", SqlType::Decimal);
        reg.insert("NUMERIC", SqlType::Decimal);

        // Character types
        reg.insert("TEXT", SqlType::Text);
        reg.insert("CLOB", SqlType::Text);
        reg.insert("VARCHAR", SqlType::VarChar(None));
        reg.insert("CHARACTER VARYING", SqlType::VarChar(None));

        // Binary
        reg.insert("BYTEA", SqlType::Blob);
        reg.insert("BLOB", SqlType::Blob);
        reg.insert("BINARY", SqlType::Blob);

        // Boolean
        reg.insert("BOOLEAN", SqlType::Boolean);
        reg.insert("BOOL", SqlType::Boolean);

        // Temporal
        reg.insert("TIMESTAMP", SqlType::Timestamp);
        reg.insert("TIMESTAMPTZ", SqlType::Timestamp);
        reg.insert("TIMESTAMP WITH TIME ZONE", SqlType::Timestamp);
        reg.insert("DATE", SqlType::Date);
        reg.insert("TIME", SqlType::Time);
        reg.insert("TIMETZ", SqlType::Time);

        // UUID
        reg.insert("UUID", SqlType::Uuid);

        // JSON
        reg.insert("JSON", SqlType::Json);
        reg.insert("JSONB", SqlType::Json);

        reg
    }

    /// Internal helper: insert without normalising (caller already upper-cases).
    fn insert(&mut self, name: &str, sql_type: SqlType) {
        self.name_to_type
            .insert(name.to_ascii_uppercase(), sql_type);
    }

    /// Look up a SQL type by name.
    ///
    /// The lookup is case-insensitive.  Returns `None` if the type name is not
    /// registered.
    pub fn lookup(&self, type_name: &str) -> Option<&SqlType> {
        self.name_to_type.get(&type_name.to_ascii_uppercase())
    }

    /// Return the default [`Value`] for the named SQL type.
    ///
    /// If the type name is not registered, returns [`Value::Null`].
    pub fn default_value_for(&self, type_name: &str) -> Value {
        self.lookup(type_name)
            .map(|t| t.default_value())
            .unwrap_or(Value::Null)
    }

    /// Register a custom SQL type name, overwriting any existing mapping.
    ///
    /// The name is normalised to UPPER CASE before storage.
    pub fn register(&mut self, name: &str, sql_type: SqlType) {
        self.name_to_type
            .insert(name.to_ascii_uppercase(), sql_type);
    }

    /// Return `true` if `value` is a valid instance of `sql_type`.
    ///
    /// `Value::Null` is considered valid for any type (SQL NULL semantics).
    ///
    /// Array element types are **not** recursively validated — only the
    /// top-level variant is checked.
    pub fn value_matches_type(value: &Value, sql_type: &SqlType) -> bool {
        if value.is_null() {
            return true;
        }
        matches!(
            (value, sql_type),
            (
                Value::I64(_),
                SqlType::Integer | SqlType::BigInt | SqlType::SmallInt
            ) | (Value::F64(_), SqlType::Float | SqlType::Double)
                | (Value::Decimal(_), SqlType::Decimal)
                | (Value::Text(_), SqlType::Text | SqlType::VarChar(_))
                | (Value::Blob(_), SqlType::Blob)
                | (Value::Bool(_), SqlType::Boolean)
                | (Value::Timestamp(_), SqlType::Timestamp)
                | (Value::Date(_), SqlType::Date)
                | (Value::Time(_), SqlType::Time)
                | (Value::Uuid(_), SqlType::Uuid)
                | (Value::Json(_), SqlType::Json)
                | (Value::Array(_), SqlType::Array(_))
        )
    }
}

impl Default for TypeRegistry {
    fn default() -> Self {
        Self::new()
    }
}
