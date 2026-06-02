//! [`Value`] enum and its implementations.

use std::cmp::Ordering;
use std::fmt;

/// A single SQL value returned from or passed to a query.
///
/// Covers both the basic scalar types common to all databases and extended
/// types for dates, timestamps, UUIDs, JSON, exact decimals, and arrays.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// SQL `NULL`.
    Null,
    /// Boolean value.
    Bool(bool),
    /// 64-bit signed integer.
    I64(i64),
    /// 64-bit floating-point number.
    F64(f64),
    /// UTF-8 text string.
    Text(String),
    /// Raw binary data.
    Blob(Vec<u8>),

    // ── Extended types ──────────────────────────────────────────────────
    /// Unix timestamp with microsecond precision (microseconds since epoch,
    /// UTC).  Maps to SQL `TIMESTAMP` / `TIMESTAMPTZ`.
    Timestamp(i64),
    /// Date-only value as days since Unix epoch (1970-01-01).
    /// Maps to SQL `DATE`.
    Date(i32),
    /// Time-of-day value as microseconds since midnight.
    /// Maps to SQL `TIME`.
    Time(i64),
    /// UUID stored as a 128-bit unsigned integer.
    /// Maps to SQL `UUID`.
    Uuid(u128),
    /// JSON or JSONB data stored as a UTF-8 string.
    /// Maps to SQL `JSON` / `JSONB`.
    Json(String),
    /// Exact decimal stored as a string representation (e.g. `"123.456"`).
    /// Using a string avoids introducing a big-decimal dependency at the
    /// core level while preserving exact precision.
    /// Maps to SQL `NUMERIC` / `DECIMAL`.
    Decimal(String),
    /// Ordered array of values (e.g. Postgres `INTEGER[]`, `TEXT[]`).
    Array(Vec<Value>),
}

impl Value {
    /// Returns the human-readable type name of this value variant.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "Null",
            Value::Bool(_) => "Bool",
            Value::I64(_) => "I64",
            Value::F64(_) => "F64",
            Value::Text(_) => "Text",
            Value::Blob(_) => "Blob",
            Value::Timestamp(_) => "Timestamp",
            Value::Date(_) => "Date",
            Value::Time(_) => "Time",
            Value::Uuid(_) => "Uuid",
            Value::Json(_) => "Json",
            Value::Decimal(_) => "Decimal",
            Value::Array(_) => "Array",
        }
    }

    /// Returns `true` if this value is [`Value::Null`].
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::I64(n) => write!(f, "{n}"),
            Value::F64(n) => write!(f, "{n}"),
            Value::Text(s) => write!(f, "{s}"),
            Value::Blob(b) => write!(f, "<blob:{} bytes>", b.len()),
            Value::Timestamp(us) => {
                // Format as seconds.microseconds from epoch
                let secs = us / 1_000_000;
                let frac = (us % 1_000_000).unsigned_abs();
                write!(f, "{secs}.{frac:06}")
            }
            Value::Date(days) => write!(f, "{days}d"),
            Value::Time(us) => {
                let total_secs = us / 1_000_000;
                let hours = total_secs / 3600;
                let mins = (total_secs % 3600) / 60;
                let secs = total_secs % 60;
                let frac = (us % 1_000_000).unsigned_abs();
                if frac == 0 {
                    write!(f, "{hours:02}:{mins:02}:{secs:02}")
                } else {
                    write!(f, "{hours:02}:{mins:02}:{secs:02}.{frac:06}")
                }
            }
            Value::Uuid(u) => {
                // Format as standard UUID: 8-4-4-4-12
                let bytes = u.to_be_bytes();
                write!(
                    f,
                    "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                    bytes[0], bytes[1], bytes[2], bytes[3],
                    bytes[4], bytes[5],
                    bytes[6], bytes[7],
                    bytes[8], bytes[9],
                    bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
                )
            }
            Value::Json(s) => write!(f, "{s}"),
            Value::Decimal(s) => write!(f, "{s}"),
            Value::Array(vals) => {
                write!(f, "[")?;
                for (i, v) in vals.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{v}")?;
                }
                write!(f, "]")
            }
        }
    }
}

impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self, other) {
            (Value::Null, Value::Null) => Some(Ordering::Equal),
            (Value::Null, _) => Some(Ordering::Less),
            (_, Value::Null) => Some(Ordering::Greater),
            (Value::Bool(a), Value::Bool(b)) => a.partial_cmp(b),
            (Value::I64(a), Value::I64(b)) => a.partial_cmp(b),
            (Value::F64(a), Value::F64(b)) => a.partial_cmp(b),
            (Value::Text(a), Value::Text(b)) => a.partial_cmp(b),
            (Value::Blob(a), Value::Blob(b)) => a.partial_cmp(b),
            (Value::Timestamp(a), Value::Timestamp(b)) => a.partial_cmp(b),
            (Value::Date(a), Value::Date(b)) => a.partial_cmp(b),
            (Value::Time(a), Value::Time(b)) => a.partial_cmp(b),
            (Value::Uuid(a), Value::Uuid(b)) => a.partial_cmp(b),
            (Value::Json(a), Value::Json(b)) => a.partial_cmp(b),
            (Value::Decimal(a), Value::Decimal(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

// ── From impls for ergonomic Value construction ─────────────────────────────

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::I64(i64::from(v))
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::I64(v)
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

impl From<Vec<u8>> for Value {
    fn from(v: Vec<u8>) -> Self {
        Value::Blob(v)
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}
