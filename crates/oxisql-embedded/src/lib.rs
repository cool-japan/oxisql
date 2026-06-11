#![forbid(unsafe_code)]
#![warn(missing_docs)]

//! `oxisql-embedded` — GlueSQL-backed in-memory SQL engine for OxiSQL.
//!
//! Provides [`EmbeddedConnection`], which implements [`Connection`] over a
//! GlueSQL [`MemoryStorage`] instance protected by a `tokio` async mutex.
//! The mutex ensures exclusive access during transactions, mapping GlueSQL's
//! `&mut self` requirement onto OxiSQL's `&self` trait surface.
//!
//! # Parameter binding
//!
//! GlueSQL's `execute` API does not accept `$1`-style positional parameters
//! in a type-safe way that round-trips through our [`Value`] enum.  This
//! crate implements SQL-injection-safe client-side parameter binding via
//! [`bind_params`] / [`escape_sql_value`] in the `params` module.
//!
//! [`bind_params`] uses AST-level substitution by default: it parses the SQL
//! into a sqlparser AST, replaces `Expr::Value(Placeholder("$N"))` nodes
//! with properly-typed literal expressions, then re-serialises.  This means
//! `$1` inside a string literal (a `SingleQuotedString` node) is never
//! mistakenly substituted.  When parsing fails (e.g. GlueSQL-specific syntax),
//! it falls back to [`bind_params_string`], which uses a forward-scan approach
//! that correctly handles `$10` vs `$1` boundary cases and `$$` escapes.
//!
//! # User-defined functions (UDFs)
//!
//! [`EmbeddedConnection`] maintains a runtime registry of scalar functions
//! that can be registered via [`EmbeddedConnection::register_udf`] and
//! invoked programmatically via [`EmbeddedConnection::call_udf`].  These are
//! not SQL-level UDFs executed inside queries; they are host-side functions
//! accessible through the Rust API.
//!
//! # Aggregate UDFs
//!
//! Custom aggregate functions follow the `init → step* → finalize` pattern
//! and are registered via [`EmbeddedConnection::register_aggregate`] and
//! applied programmatically via [`EmbeddedConnection::apply_aggregate`].
//!
//! # Savepoints
//!
//! [`EmbeddedConnection::savepoint`], [`EmbeddedConnection::rollback_to_savepoint`],
//! and [`EmbeddedConnection::release_savepoint`] are provided for API
//! compatibility but are **no-ops** on GlueSQL `MemoryStorage`, which does not
//! support nested transactions.
//!
//! # SQL import / export
//!
//! [`EmbeddedConnection::import_from_sql`] loads a SQL dump string by
//! executing all statements via `execute_batch`.
//! [`EmbeddedConnection::export_as_sql`] enumerates all tables via
//! `gluesql::core::store::Store::fetch_all_schemas`, emits `CREATE TABLE`
//! DDL for each table, then emits one `INSERT INTO … VALUES (…)` per row.
//!
//! # Persistent storage
//!
//! [`EmbeddedConnection::open_file`] provides a path for future file-backed
//! persistence.  Without the `sled-storage` feature the call returns
//! [`OxiSqlError::UnsupportedUri`].
//!
//! # GlueSQL SQL Dialect Notes
//!
//! GlueSQL's `MemoryStorage` supports a subset of SQL. Known differences from
//! standard SQL:
//!
//! ## Unsupported features
//! - `ALTER TABLE ADD COLUMN` — not supported; recreate the table instead
//! - `ALTER TABLE DROP COLUMN` — not supported
//! - Multi-row `VALUES` in a single `INSERT` — use individual `INSERT` statements
//! - Window functions (`ROW_NUMBER()`, `RANK()`) — not supported in MemoryStorage
//! - `ATTACH DATABASE` — intercepted and returns `UnsupportedUri`
//! - `INFORMATION_SCHEMA` tables — not available in MemoryStorage
//!
//! ## Transaction support
//! - `BEGIN` / `COMMIT` / `ROLLBACK` — supported syntactically but MemoryStorage
//!   does not implement MVCC; all changes are immediately visible
//! - Savepoints — accepted via `SAVEPOINT name` / `ROLLBACK TO name` but are no-ops
//!
//! ## Type notes
//! - All numeric columns accept `INT`, `INTEGER`, `BIGINT`, `FLOAT`, `DOUBLE`
//! - `TEXT`, `VARCHAR(n)`, `CHAR(n)` are all stored as UTF-8 strings
//! - `BLOB` is supported via `X'hex'` literals
//! - `DECIMAL` columns are stored as text; use `Value::Decimal` for precision
//!
//! ## Parameter binding
//! - `$1`-style positional parameters are supported via `execute_with_params`
//! - Named parameters are not supported
//! - Parameters inside string literals (`'has $1'`) are preserved correctly
//!   (AST-level binding avoids the injection risk of string replacement)

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use chrono::{NaiveDate, NaiveTime};
use gluesql::core::ast::ToSql as GlueToSql;
use gluesql::core::store::Store as GlueStore;
use gluesql::prelude::{Glue, MemoryStorage, Payload};
use oxisql_core::{
    ColumnInfo, Connection, ForeignKeyInfo, IndexInfo, OxiSqlError, PreparedStatement, Row,
    TableInfo, TableType, ToSqlValue, Transaction, Value,
};
use tokio::sync::{Mutex, OwnedMutexGuard};

mod params;
pub use params::{bind_params, bind_params_string, escape_sql_value};

pub mod csv;
pub use csv::{build_csv_output, parse_csv, value_to_csv_field};

#[cfg(feature = "fjall-storage")]
mod fjall_storage;
#[cfg(feature = "fjall-storage")]
pub use fjall_storage::FjallGlueStorage;

#[cfg(feature = "redb-storage")]
mod redb_storage;
#[cfg(feature = "redb-storage")]
pub use redb_storage::RedbGlueStorage;

pub(crate) mod fts;
use fts::FtsIndex;

pub mod vtable;
pub use vtable::{VirtualTableFn, VirtualTableRegistry};

pub mod btree_index;
pub use btree_index::{BTreeIndex, IndexKey, IndexRegistry};

// ── UdfRegistry ──────────────────────────────────────────────────────────────

/// Type alias for a boxed scalar UDF closure.
///
/// Encapsulates the `Arc<dyn Fn(Vec<Value>) -> Value + Send + Sync>` type
/// behind a short alias to avoid the `type_complexity` clippy lint.
type UdfFn = Arc<dyn Fn(Vec<Value>) -> Value + Send + Sync>;

/// A runtime registry of scalar user-defined functions.
///
/// Functions are stored as `UdfFn` (an `Arc<dyn Fn>` alias) so they are cheaply cloneable and
/// can be shared across clones of [`EmbeddedConnection`].  The registry is
/// protected by a [`RwLock`] so that [`EmbeddedConnection::register_udf`]
/// (write) and [`EmbeddedConnection::call_udf`] (read) can both operate on a
/// `&self` receiver.
#[derive(Default)]
pub struct UdfRegistry {
    funcs: HashMap<String, UdfFn>,
}

impl fmt::Debug for UdfRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "UdfRegistry {{ funcs: <{} registered> }}",
            self.funcs.len()
        )
    }
}

impl UdfRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a named function.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        f: impl Fn(Vec<Value>) -> Value + Send + Sync + 'static,
    ) {
        self.funcs.insert(name.into(), Arc::new(f));
    }

    /// Invoke a registered function by name.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Parse`] when `name` is not registered.
    pub fn call(&self, name: &str, args: Vec<Value>) -> Result<Value, OxiSqlError> {
        match self.funcs.get(name) {
            Some(f) => Ok(f(args)),
            None => Err(OxiSqlError::Parse(format!("unknown UDF '{name}'"))),
        }
    }
}

// ── AggregateUdf ─────────────────────────────────────────────────────────────

/// A runtime aggregate user-defined function.
///
/// Aggregate UDFs follow the classic `init → step* → finalize` pattern:
///
/// 1. `init` produces the initial accumulator value.
/// 2. `step` folds each input `Value` into the accumulator.
/// 3. `finalize` converts the final accumulator into the result value.
///
/// Because GlueSQL does not support SQL-level aggregate functions, aggregate
/// UDFs are applied programmatically via
/// [`EmbeddedConnection::apply_aggregate`].
pub struct AggregateUdf {
    /// Produce the zero/identity accumulator value.
    pub init: Box<dyn Fn() -> Value + Send + Sync>,
    /// Fold one input value into the current accumulator.
    pub step: Box<dyn Fn(Value, Value) -> Value + Send + Sync>,
    /// Convert the final accumulator to the result.
    pub finalize: Box<dyn Fn(Value) -> Value + Send + Sync>,
}

impl fmt::Debug for AggregateUdf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "AggregateUdf {{ ... }}")
    }
}

// ── param substitution ───────────────────────────────────────────────────────

/// Substitute `$1`, `$2`, … placeholders with SQL-safe escaped values.
///
/// Delegates to [`bind_params`] after converting `&[&dyn ToSqlValue]` to an
/// owned `Vec<Value>`.
pub(crate) fn substitute_params(
    sql: &str,
    params: &[&dyn ToSqlValue],
) -> Result<String, OxiSqlError> {
    let values: Vec<Value> = params.iter().map(|p| p.to_value()).collect();
    bind_params(sql, &values)
}

// ── value conversion ─────────────────────────────────────────────────────────

/// Convert a GlueSQL [`gluesql::prelude::Value`] into an OxiSQL [`Value`].
///
/// GlueSQL has 26+ variants; integer widths narrower than 64-bit are widened,
/// floats narrower than 64-bit are widened, and complex types (Map, List,
/// Date, …) are mapped to the appropriate OxiSQL extended type where possible,
/// or to [`Value::Text`] via their `Display` representation for types without
/// a direct OxiSQL equivalent.
fn glue_value_to_oxisql(v: gluesql::prelude::Value) -> Value {
    use gluesql::prelude::Value as GV;
    match v {
        GV::Null => Value::Null,
        GV::Bool(b) => Value::Bool(b),
        GV::I8(n) => Value::I64(i64::from(n)),
        GV::I16(n) => Value::I64(i64::from(n)),
        GV::I32(n) => Value::I64(i64::from(n)),
        GV::I64(n) => Value::I64(n),
        GV::I128(n) => {
            // Clamp to i64 range; out-of-range values become Text.
            i64::try_from(n).map_or_else(|_| Value::Text(n.to_string()), Value::I64)
        }
        GV::U8(n) => Value::I64(i64::from(n)),
        GV::U16(n) => Value::I64(i64::from(n)),
        GV::U32(n) => Value::I64(i64::from(n)),
        GV::U64(n) => i64::try_from(n).map_or_else(|_| Value::Text(n.to_string()), Value::I64),
        GV::U128(n) => i64::try_from(n).map_or_else(|_| Value::Text(n.to_string()), Value::I64),
        GV::F32(n) => Value::F64(f64::from(n)),
        GV::F64(n) => Value::F64(n),
        GV::Str(s) => Value::Text(s),
        GV::Bytea(b) => Value::Blob(b),
        // Map GlueSQL Date/Time/Timestamp/Uuid/Decimal to extended types
        GV::Date(d) => {
            // chrono::NaiveDate → days since Unix epoch (1970-01-01).
            // signed_duration_since handles pre-epoch dates correctly.
            let unix_epoch =
                NaiveDate::from_ymd_opt(1970, 1, 1).expect("1970-01-01 is a valid date");
            let days = d.signed_duration_since(unix_epoch).num_days();
            Value::Date(i32::try_from(days).unwrap_or(i32::MAX))
        }
        GV::Time(t) => {
            // chrono::NaiveTime → microseconds since midnight.
            // num_microseconds() only returns None for durations > ~292 years,
            // which is impossible for a time-of-day value.
            let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("00:00:00 is a valid time");
            let micros = t
                .signed_duration_since(midnight)
                .num_microseconds()
                .unwrap_or(0);
            Value::Time(micros)
        }
        GV::Timestamp(ts) => {
            // chrono::NaiveDateTime → microseconds since Unix epoch (treated as UTC).
            Value::Timestamp(ts.and_utc().timestamp_micros())
        }
        GV::Uuid(u) => Value::Uuid(u),
        GV::Decimal(d) => Value::Decimal(format!("{d}")),
        GV::List(items) => {
            let vals: Vec<Value> = items.into_iter().map(glue_value_to_oxisql).collect();
            Value::Array(vals)
        }
        // All remaining types (Interval, Map, Point, Inet) are rendered as
        // their Debug/Display string so no data is silently lost.
        other => Value::Text(format!("{other:?}")),
    }
}

// ── GlueSQL DataType → SQL type name string ──────────────────────────────────

/// Map a GlueSQL [`DataType`] to its canonical SQL type name string.
///
/// Used by [`Connection::columns`] to populate [`ColumnInfo::data_type`].
/// The strings match the names produced by GlueSQL's own `to_ddl()` output
/// so that round-tripping through `import_from_sql`/`export_as_sql` works
/// correctly.
fn glue_data_type_to_str(dt: &gluesql::core::ast::DataType) -> &'static str {
    use gluesql::core::ast::DataType;
    match dt {
        DataType::Boolean => "BOOLEAN",
        DataType::Int8 => "INT8",
        DataType::Int16 => "INT16",
        DataType::Int32 => "INT32",
        DataType::Int => "INT",
        DataType::Int128 => "INT128",
        DataType::Uint8 => "UINT8",
        DataType::Uint16 => "UINT16",
        DataType::Uint32 => "UINT32",
        DataType::Uint64 => "UINT64",
        DataType::Uint128 => "UINT128",
        DataType::Float32 => "FLOAT32",
        DataType::Float => "FLOAT",
        DataType::Text => "TEXT",
        DataType::Bytea => "BYTEA",
        DataType::Inet => "INET",
        DataType::Date => "DATE",
        DataType::Timestamp => "TIMESTAMP",
        DataType::Time => "TIME",
        DataType::Interval => "INTERVAL",
        DataType::Uuid => "UUID",
        DataType::Map => "MAP",
        DataType::List => "LIST",
        DataType::Decimal => "DECIMAL",
        DataType::Point => "POINT",
    }
}

// ── payload → rows conversion ────────────────────────────────────────────────

/// Extract the row count from a non-SELECT [`Payload`].
pub(crate) fn payload_to_affected_rows(payload: &Payload) -> u64 {
    match payload {
        Payload::Insert(n) | Payload::Delete(n) | Payload::Update(n) => *n as u64,
        _ => 0,
    }
}

/// Extract [`Row`]s from a [`Payload::Select`].
pub(crate) fn payload_to_rows(payload: Payload) -> Vec<Row> {
    if let Payload::Select { labels, rows } = payload {
        rows.into_iter()
            .map(|row_vals| {
                let values: Vec<Value> = row_vals.into_iter().map(glue_value_to_oxisql).collect();
                Row::new(labels.clone(), values)
            })
            .collect()
    } else {
        Vec::new()
    }
}

// ── PRAGMA interception ──────────────────────────────────────────────────────

/// Intercept PRAGMA statements before they reach GlueSQL.
///
/// GlueSQL `MemoryStorage` has no PRAGMA support (that's SQLite-specific).
/// This function recognises the most common PRAGMA forms and returns a
/// synthetic result so callers can proceed without an error.
///
/// Returns `None` when the statement is not a PRAGMA (fast path for regular
/// SQL), or `Some(Result<Vec<Row>, OxiSqlError>)` when it is.
///
/// # Note
///
/// `execute_batch` and `prepare` do **not** invoke this helper because they
/// forward SQL directly to GlueSQL.  PRAGMA statements issued through those
/// paths will return a GlueSQL execution error.  Use `execute` / `query`
/// for PRAGMA interception.
pub(crate) fn handle_pragma(sql: &str) -> Option<Result<Vec<Row>, OxiSqlError>> {
    let trimmed = sql.trim().trim_end_matches(';').trim();
    let upper = trimmed.to_ascii_uppercase();

    if !upper.starts_with("PRAGMA") {
        return None;
    }

    // Everything after "PRAGMA" (including leading whitespace).
    // SAFETY: "PRAGMA" is 6 ASCII bytes, and `upper.starts_with("PRAGMA")` is true.
    let rest = trimmed[6..].trim();

    // Split `name` and optional `= value` parts.
    let (name, _value) = if let Some(eq_pos) = rest.find('=') {
        (rest[..eq_pos].trim(), Some(rest[eq_pos + 1..].trim()))
    } else {
        (rest, None)
    };

    let name_upper = name.to_ascii_uppercase();

    let rows = match name_upper.as_str() {
        "JOURNAL_MODE" => vec![Row::new(
            vec!["journal_mode".to_string()],
            vec![Value::Text("memory".to_string())],
        )],
        "FOREIGN_KEYS" => vec![Row::new(
            vec!["foreign_keys".to_string()],
            vec![Value::I64(0)],
        )],
        "PAGE_SIZE" => vec![Row::new(
            vec!["page_size".to_string()],
            vec![Value::I64(4096)],
        )],
        "PAGE_COUNT" => vec![Row::new(
            vec!["page_count".to_string()],
            vec![Value::I64(0)],
        )],
        "FREELIST_COUNT" => vec![Row::new(
            vec!["freelist_count".to_string()],
            vec![Value::I64(0)],
        )],
        "CACHE_SIZE" => vec![Row::new(
            vec!["cache_size".to_string()],
            vec![Value::I64(0)],
        )],
        "USER_VERSION" => vec![Row::new(
            vec!["user_version".to_string()],
            vec![Value::I64(0)],
        )],
        "INTEGRITY_CHECK" => vec![Row::new(
            vec!["integrity_check".to_string()],
            vec![Value::Text("ok".to_string())],
        )],
        // Unknown PRAGMA — return empty result rather than error.
        _ => vec![],
    };

    Some(Ok(rows))
}

// ── ATTACH interception ──────────────────────────────────────────────────────

/// Intercept ATTACH DATABASE / ATTACH SCHEMA statements before they reach GlueSQL.
///
/// GlueSQL `MemoryStorage` has no concept of attached databases.  Rather than
/// surfacing an opaque GlueSQL parse error, this function returns a clear
/// [`OxiSqlError::UnsupportedUri`] message explaining the limitation and the
/// recommended upgrade path.
///
/// Returns `None` when the statement is not an ATTACH (fast path for regular
/// SQL), or `Some(Err(…))` when it is.
pub(crate) fn handle_attach(sql: &str) -> Option<Result<Vec<Row>, OxiSqlError>> {
    let upper = sql.trim().to_ascii_uppercase();
    if upper.starts_with("ATTACH") {
        return Some(Err(OxiSqlError::UnsupportedUri(
            "ATTACH DATABASE is not supported with in-memory storage; \
             use EmbeddedConnection::open_file() for persistent \
             multi-database support"
                .into(),
        )));
    }
    None
}

// ── execute helpers ──────────────────────────────────────────────────────────

/// Run SQL against a mutable [`Glue`] instance and return the affected row
/// count (for DML/DDL) or zero (for other statement types).
async fn glue_execute(
    glue: &mut Glue<MemoryStorage>,
    sql: &str,
    params: &[&dyn ToSqlValue],
) -> Result<u64, OxiSqlError> {
    let sql = substitute_params(sql, params)?;
    let payloads = glue
        .execute(&sql)
        .await
        .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
    Ok(payloads.iter().map(payload_to_affected_rows).sum())
}

/// Run a SELECT SQL against a mutable [`Glue`] instance and return all rows.
async fn glue_query(
    glue: &mut Glue<MemoryStorage>,
    sql: &str,
    params: &[&dyn ToSqlValue],
) -> Result<Vec<Row>, OxiSqlError> {
    let sql = substitute_params(sql, params)?;
    let payloads = glue
        .execute(&sql)
        .await
        .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
    Ok(payloads.into_iter().flat_map(payload_to_rows).collect())
}

// ── EmbeddedConnection ───────────────────────────────────────────────────────

/// An in-memory SQL connection backed by GlueSQL [`MemoryStorage`].
///
/// The inner `Glue` instance is protected by an [`Arc`]<[`Mutex`]> so that
/// the connection is cheaply cloneable and `Send + Sync`, while still allowing
/// `&mut self` access during query execution.
///
/// # Why `Mutex` and not `RwLock` for the inner `Glue`
///
/// GlueSQL requires `&mut Glue` for all operations (both reads and writes),
/// so a `RwLock` would provide no concurrency benefit over `Mutex`.
/// All access is necessarily serialized through the write lock.
///
/// The [`UdfRegistry`], by contrast, uses `Arc<std::sync::RwLock<UdfRegistry>>`
/// because [`register_udf`][Self::register_udf] takes a write lock while
/// [`call_udf`][Self::call_udf] only needs a read lock, allowing concurrent
/// reads without contention.
///
/// Scalar user-defined functions are stored in `udf_registry` (registered via
/// [`register_udf`][Self::register_udf] and invocable via
/// [`call_udf`][Self::call_udf]).  Aggregate UDFs are stored separately in
/// `agg_registry` (registered via [`register_aggregate`][Self::register_aggregate]
/// and applied via [`apply_aggregate`][Self::apply_aggregate]).
///
/// Full-text search state is held in `fts_index`: an `Arc<RwLock<FtsIndex>>`
/// that is shared across clones of the same connection.  FTS virtual tables
/// (`CREATE VIRTUAL TABLE … USING fts5/fts4`) are intercepted before being
/// forwarded to GlueSQL (which has no FTS support), and subsequent inserts /
/// MATCH queries are handled entirely by the in-memory inverted index.
#[derive(Clone)]
pub struct EmbeddedConnection {
    inner: Arc<Mutex<Glue<MemoryStorage>>>,
    udf_registry: Arc<RwLock<UdfRegistry>>,
    agg_registry: Arc<RwLock<HashMap<String, Arc<AggregateUdf>>>>,
    fts_index: Arc<RwLock<FtsIndex>>,
    /// Virtual table registry.  Stored as a plain (non-`Arc`) field so that
    /// `register_virtual_table` / `unregister_virtual_table` take `&mut self`.
    /// Registrations made on one clone are **not** visible to other clones —
    /// this is intentional and matches the spec.
    vtable_registry: VirtualTableRegistry,
    /// B-tree secondary index registry.  Wrapped in `Arc<std::sync::Mutex>` so
    /// that it is `Clone` and can be cheaply shared across `from_arc` usages
    /// while still being mutated via `&self` methods.
    index_registry: Arc<std::sync::Mutex<IndexRegistry>>,
}

impl fmt::Debug for EmbeddedConnection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let agg_count = self.agg_registry.read().map(|g| g.len()).unwrap_or(0);
        let fts_count = self
            .fts_index
            .read()
            .map(|idx| idx.tables_len())
            .unwrap_or(0);
        f.debug_struct("EmbeddedConnection")
            .field("inner", &"Arc<Mutex<Glue<MemoryStorage>>>")
            .field(
                "udf_registry",
                &*self.udf_registry.read().unwrap_or_else(|e| e.into_inner()),
            )
            .field("agg_registry", &format!("<{agg_count} registered>"))
            .field("fts_index", &format!("<{fts_count} fts tables>"))
            .finish()
    }
}

impl EmbeddedConnection {
    /// Open a new, empty in-memory database.
    ///
    /// # Errors
    ///
    /// Currently infallible; returns `Ok` on all platforms.
    pub fn open_memory() -> Result<Self, OxiSqlError> {
        let storage = MemoryStorage::default();
        let glue = Glue::new(storage);
        Ok(Self {
            inner: Arc::new(Mutex::new(glue)),
            udf_registry: Arc::new(RwLock::new(UdfRegistry::new())),
            agg_registry: Arc::new(RwLock::new(HashMap::new())),
            fts_index: Arc::new(RwLock::new(FtsIndex::new())),
            vtable_registry: VirtualTableRegistry::new(),
            index_registry: Arc::new(std::sync::Mutex::new(IndexRegistry::new())),
        })
    }

    /// Open a file-backed embedded database.
    ///
    /// When the `sled-storage` feature is enabled this will use GlueSQL's
    /// `SledStorage` for durable persistence.  Without that feature this
    /// method always returns [`OxiSqlError::UnsupportedUri`].
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::UnsupportedUri`] unless the `sled-storage`
    /// feature is compiled in.
    pub async fn open_file(_path: &str) -> Result<Self, OxiSqlError> {
        // File-backed persistence is provided via `SledEmbeddedConnection::open(path)`.
        // This method is intentionally a no-op; `EmbeddedConnection` is memory-only.
        Err(OxiSqlError::UnsupportedUri(
            "persistent storage requires the sled-storage feature".into(),
        ))
    }

    /// Create an [`EmbeddedConnection`] backed by a shared [`Arc`].
    ///
    /// Useful when integrating with `oxisql_pool::embedded::EmbeddedPool` so
    /// that multiple "connections" share the same underlying in-memory storage.
    pub fn from_arc(arc: Arc<Mutex<Glue<MemoryStorage>>>) -> Self {
        Self {
            inner: arc,
            udf_registry: Arc::new(RwLock::new(UdfRegistry::new())),
            agg_registry: Arc::new(RwLock::new(HashMap::new())),
            fts_index: Arc::new(RwLock::new(FtsIndex::new())),
            vtable_registry: VirtualTableRegistry::new(),
            index_registry: Arc::new(std::sync::Mutex::new(IndexRegistry::new())),
        }
    }

    /// Return a reference to the inner `Arc<Mutex<Glue<MemoryStorage>>>` for
    /// advanced use cases (e.g., migration runner, pool integration).
    pub fn inner(&self) -> &Arc<Mutex<Glue<MemoryStorage>>> {
        &self.inner
    }

    /// Register a scalar user-defined function by name.
    ///
    /// The function receives a `Vec<Value>` of arguments and returns a single
    /// `Value`.  Registering under the same name as an existing UDF replaces
    /// the previous implementation.
    ///
    /// The registry is stored inside an [`Arc<RwLock>`] so this method takes
    /// `&self` — multiple clones of the connection share the same registry.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the internal write lock is poisoned.
    pub fn register_udf(
        &self,
        name: impl Into<String>,
        f: impl Fn(Vec<Value>) -> Value + Send + Sync + 'static,
    ) -> Result<(), OxiSqlError> {
        let mut reg = self
            .udf_registry
            .write()
            .map_err(|e| OxiSqlError::Other(format!("UDF registry lock poisoned: {e}")))?;
        reg.register(name, f);
        Ok(())
    }

    /// Invoke a registered user-defined function by name.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Parse`] if `name` is not registered.
    /// Returns [`OxiSqlError::Other`] if the internal read lock is poisoned.
    pub fn call_udf(&self, name: &str, args: Vec<Value>) -> Result<Value, OxiSqlError> {
        let reg = self
            .udf_registry
            .read()
            .map_err(|e| OxiSqlError::Other(format!("UDF registry lock poisoned: {e}")))?;
        reg.call(name, args)
    }

    /// Return the number of currently registered UDFs.
    ///
    /// Useful for diagnostics and testing.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the internal read lock is poisoned.
    pub fn udf_count(&self) -> Result<usize, OxiSqlError> {
        let reg = self
            .udf_registry
            .read()
            .map_err(|e| OxiSqlError::Other(format!("UDF registry lock poisoned: {e}")))?;
        Ok(reg.funcs.len())
    }

    /// Execute a semicolon-separated multi-statement SQL script.
    ///
    /// Delegates to [`Connection::execute_batch`].
    pub async fn execute_script(&self, sql: &str) -> Result<u64, OxiSqlError> {
        self.execute_batch(sql).await
    }

    /// Construct an [`EmbeddedConnection`] from a pre-existing GlueSQL instance.
    pub fn from_glue(glue: Glue<MemoryStorage>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(glue)),
            udf_registry: Arc::new(RwLock::new(UdfRegistry::new())),
            agg_registry: Arc::new(RwLock::new(HashMap::new())),
            fts_index: Arc::new(RwLock::new(FtsIndex::new())),
            vtable_registry: VirtualTableRegistry::new(),
            index_registry: Arc::new(std::sync::Mutex::new(IndexRegistry::new())),
        }
    }

    // ── Virtual table API ────────────────────────────────────────────────────

    /// Register a virtual table backed by a callback function.
    ///
    /// When a `SELECT * FROM name` (or `SELECT cols FROM name WHERE …`) query
    /// references `name`, the callback is invoked to produce the row data.
    /// Simple `WHERE col = 'val'` and `WHERE col = N` predicates are applied
    /// post-scan; complex predicates return all rows.
    ///
    /// Registration is scoped to **this clone** of the connection — other
    /// clones produced before or after this call do not see the new table.
    ///
    /// **Limitations**:
    /// - Interception only applies to [`Connection::query`], not to
    ///   `Transaction::query` or `PreparedStatement::query`.  Queries issued
    ///   through a transaction or prepared statement against a virtual table name
    ///   will reach GlueSQL and fail with "table not found".
    /// - `WHERE` filters with `$N` parameter placeholders are not applied;
    ///   queries with unresolved placeholders return all rows unfiltered.
    ///
    /// The closure must be `Send + Sync + 'static`.
    pub fn register_virtual_table(&mut self, name: &str, provider: VirtualTableFn) {
        self.vtable_registry.register(name, provider);
    }

    /// Remove a previously registered virtual table.  No-op if `name` is not
    /// registered.
    pub fn unregister_virtual_table(&mut self, name: &str) {
        self.vtable_registry.unregister(name);
    }

    /// Return the names of all registered virtual tables, sorted alphabetically.
    pub fn virtual_table_names(&self) -> Vec<String> {
        self.vtable_registry.names()
    }

    // ── B-tree index API ─────────────────────────────────────────────────────

    /// Create a B-tree secondary index on `(table, column)`.
    ///
    /// This is a Rust-side API equivalent of `CREATE INDEX`.  The index is
    /// maintained in memory and populated as rows are inserted via `execute`.
    ///
    /// Calling this method does **not** back-fill existing rows; index rows
    /// that were inserted before this call will not be found via index lookup.
    ///
    /// **Query acceleration**: the index is **not** used to accelerate SELECT
    /// queries issued through GlueSQL — GlueSQL still performs its own linear
    /// scan.  Use [`lookup_btree_index`][Self::lookup_btree_index] to read the
    /// index directly from application code (e.g., to short-circuit a query
    /// before issuing it).
    ///
    /// **INSERT form**: only `INSERT INTO t (col1, col2) VALUES (v1, v2)` with
    /// an explicit column list triggers automatic index maintenance.  Positional
    /// `INSERT INTO t VALUES (...)` (no column list) is silently ignored by the
    /// index updater.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the index lock is poisoned.
    pub fn create_btree_index(&self, table: &str, column: &str) -> Result<(), OxiSqlError> {
        let mut reg = self
            .index_registry
            .lock()
            .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))?;
        reg.create_index(table, column);
        Ok(())
    }

    /// Drop a B-tree secondary index on `(table, column)`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the index lock is poisoned.
    pub fn drop_btree_index(&self, table: &str, column: &str) -> Result<(), OxiSqlError> {
        let mut reg = self
            .index_registry
            .lock()
            .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))?;
        reg.drop_index(table, column);
        Ok(())
    }

    /// Return `true` if a B-tree index exists on `(table, column)`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the index lock is poisoned.
    pub fn has_btree_index(&self, table: &str, column: &str) -> Result<bool, OxiSqlError> {
        let reg = self
            .index_registry
            .lock()
            .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))?;
        Ok(reg.has_index(table, column))
    }

    /// Look up row IDs in the B-tree index for an exact key match on
    /// `(table, column)`.
    ///
    /// Returns `None` when no index exists for that pair; returns an empty set
    /// when the index exists but has no matching entries.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the index lock is poisoned.
    pub fn lookup_btree_index(
        &self,
        table: &str,
        column: &str,
        key: &IndexKey,
    ) -> Result<Option<std::collections::HashSet<i64>>, OxiSqlError> {
        let reg = self
            .index_registry
            .lock()
            .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))?;
        Ok(reg.lookup(table, column, key))
    }

    // ── EXPLAIN ──────────────────────────────────────────────────────────────

    /// Return a human-readable explanation of the query execution plan.
    ///
    /// GlueSQL does not expose a formal `EXPLAIN` AST, so this method
    /// produces a simple logical plan summary by inspecting the SQL text.
    /// The result is a multi-line tree string — no GlueSQL lock is acquired.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Parse`] if `sql` is empty.
    pub async fn explain(&self, sql: &str) -> Result<String, OxiSqlError> {
        if sql.trim().is_empty() {
            return Err(OxiSqlError::Parse("empty SQL statement".into()));
        }

        // ── Cost-based path for SELECT queries ───────────────────────────────
        // Attempt: parse → plan → optimize → explain_verbose (rows/cost tree).
        // Gated to Statement::Query only; any error falls through to the
        // pattern-based fallback below so that explain() never hard-errors.
        if let Ok(stmt) = oxisql_parse::parse_one(sql) {
            if let sqlparser::ast::Statement::Query(_) = &stmt {
                if let Ok(plan) = oxisql_parse::plan_statement(&stmt) {
                    let optimized = oxisql_parse::optimize(plan);
                    let cost_model = oxisql_parse::CostModel::new();
                    return Ok(oxisql_parse::explain_verbose(&optimized, &cost_model));
                }
            }
        }

        // ── Pattern-based fallback (non-SELECT / parse/plan failure) ─────────
        let upper = sql.to_uppercase();
        let mut lines: Vec<String> = Vec::new();

        // Detect joins.
        if upper.contains(" JOIN ") {
            lines.push("Join".into());
            lines.push("  ├── Scan (left)".into());
            lines.push("  └── Scan (right)".into());
        } else if upper.starts_with("SELECT") {
            // Detect filter (WHERE clause).
            if upper.contains(" WHERE ") {
                lines.push("Filter".into());
                lines.push("  └── Scan".into());
            } else {
                lines.push("Scan".into());
            }
        } else if upper.starts_with("INSERT") {
            lines.push("Insert".into());
        } else if upper.starts_with("UPDATE") {
            lines.push("Update".into());
            if upper.contains(" WHERE ") {
                lines.push("  └── Filter".into());
            }
        } else if upper.starts_with("DELETE") {
            lines.push("Delete".into());
            if upper.contains(" WHERE ") {
                lines.push("  └── Filter".into());
            }
        } else {
            lines.push(format!(
                "Statement: {}",
                sql.split_whitespace().next().unwrap_or("?")
            ));
        }

        Ok(lines.join("\n"))
    }

    /// Store a JSON string in a TEXT column, replacing any existing row with
    /// the same key.
    ///
    /// Runs two statements in sequence (DELETE then INSERT) so that the
    /// operation is an idempotent upsert.  GlueSQL `MemoryStorage` does not
    /// support `ON CONFLICT`, so this two-step approach is used instead.
    ///
    /// `table`, `key_col`, and `val_col` are interpolated directly into the
    /// SQL (caller-controlled identifiers); `key` and `json` are passed as
    /// `$1`/`$2` parameters and fully escaped by [`bind_params`].
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if either the DELETE or INSERT fails.
    pub async fn json_set(
        &self,
        table: &str,
        key_col: &str,
        val_col: &str,
        key: &str,
        json: &str,
    ) -> Result<(), OxiSqlError> {
        // Step 1: remove any existing row for this key.
        let del_sql = format!("DELETE FROM {table} WHERE {key_col} = $1");
        self.execute(&del_sql, &[&key as &dyn ToSqlValue]).await?;

        // Step 2: insert the new key/value pair.
        let ins_sql = format!("INSERT INTO {table} ({key_col}, {val_col}) VALUES ($1, $2)");
        self.execute(
            &ins_sql,
            &[&key as &dyn ToSqlValue, &json as &dyn ToSqlValue],
        )
        .await?;

        Ok(())
    }

    /// Retrieve a JSON string from a TEXT column by key.
    ///
    /// Returns `Ok(Some(json))` when the key is found, `Ok(None)` when no row
    /// matches, and `Err` only on SQL execution errors.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the SELECT fails.
    pub async fn json_get(
        &self,
        table: &str,
        key_col: &str,
        val_col: &str,
        key: &str,
    ) -> Result<Option<String>, OxiSqlError> {
        let sel_sql = format!("SELECT {val_col} FROM {table} WHERE {key_col} = $1");
        let rows = self.query(&sel_sql, &[&key as &dyn ToSqlValue]).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        match rows[0].try_get::<String>(val_col) {
            Ok(s) => Ok(Some(s)),
            Err(_) => Ok(None),
        }
    }

    // ── Task A: Savepoint support ─────────────────────────────────────────────

    /// Create a savepoint with the given name.
    ///
    /// GlueSQL `MemoryStorage` does not support nested transactions, so this
    /// method is a **no-op** — it returns `Ok(())` unconditionally.  The name
    /// is accepted for API compatibility with backends that do support
    /// savepoints (e.g. `oxisql-postgres`).
    ///
    /// If you need true savepoint semantics, use a backend that supports them.
    ///
    /// # Errors
    ///
    /// Currently infallible for `EmbeddedConnection`; always returns `Ok`.
    pub async fn savepoint(&self, _name: &str) -> Result<(), OxiSqlError> {
        // GlueSQL MemoryStorage does not support SAVEPOINT syntax.
        // Callers that need real savepoints should use a transactional backend.
        Ok(())
    }

    /// Roll back to a named savepoint.
    ///
    /// Like [`savepoint`][Self::savepoint], this is a **no-op** on the
    /// embedded backend — GlueSQL `MemoryStorage` has no partial-rollback
    /// capability.
    ///
    /// # Errors
    ///
    /// Currently infallible for `EmbeddedConnection`; always returns `Ok`.
    pub async fn rollback_to_savepoint(&self, _name: &str) -> Result<(), OxiSqlError> {
        Ok(())
    }

    /// Release (commit) a savepoint.
    ///
    /// Like [`savepoint`][Self::savepoint], this is a **no-op** on the
    /// embedded backend.
    ///
    /// # Errors
    ///
    /// Currently infallible for `EmbeddedConnection`; always returns `Ok`.
    pub async fn release_savepoint(&self, _name: &str) -> Result<(), OxiSqlError> {
        Ok(())
    }

    // ── Task B: Aggregate UDFs ────────────────────────────────────────────────

    /// Register an aggregate user-defined function by name.
    ///
    /// The aggregate follows the classic `init → step* → finalize` pattern:
    /// - `init()` returns the zero accumulator value.
    /// - `step(acc, val)` folds one input `val` into the accumulator `acc`.
    /// - `finalize(acc)` converts the final accumulator to the result.
    ///
    /// Registering under the same name as an existing aggregate replaces the
    /// previous implementation.  The registry is shared across all clones of
    /// this connection via `Arc<RwLock<…>>`.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the internal write lock is poisoned.
    pub fn register_aggregate(
        &self,
        name: impl Into<String>,
        init: impl Fn() -> Value + Send + Sync + 'static,
        step: impl Fn(Value, Value) -> Value + Send + Sync + 'static,
        finalize: impl Fn(Value) -> Value + Send + Sync + 'static,
    ) -> Result<(), OxiSqlError> {
        let udf = AggregateUdf {
            init: Box::new(init),
            step: Box::new(step),
            finalize: Box::new(finalize),
        };
        let mut reg = self
            .agg_registry
            .write()
            .map_err(|e| OxiSqlError::Other(format!("aggregate registry lock poisoned: {e}")))?;
        reg.insert(name.into(), Arc::new(udf));
        Ok(())
    }

    /// Apply a registered aggregate UDF over a slice of values.
    ///
    /// Runs `init()` then folds each element of `values` through `step(acc, v)`,
    /// and finally calls `finalize(acc)` to produce the result.
    ///
    /// # Errors
    ///
    /// - Returns [`OxiSqlError::Parse`] when `name` is not registered.
    /// - Returns [`OxiSqlError::Other`] if the internal read lock is poisoned.
    pub fn apply_aggregate(&self, name: &str, values: Vec<Value>) -> Result<Value, OxiSqlError> {
        let reg = self
            .agg_registry
            .read()
            .map_err(|e| OxiSqlError::Other(format!("aggregate registry lock poisoned: {e}")))?;
        let udf = reg
            .get(name)
            .ok_or_else(|| OxiSqlError::Parse(format!("unknown aggregate UDF '{name}'")))?;
        let udf = Arc::clone(udf);
        // Release the read lock before running user code.
        drop(reg);

        let mut acc = (udf.init)();
        for v in values {
            acc = (udf.step)(acc, v);
        }
        Ok((udf.finalize)(acc))
    }

    // ── Task C: export_as_sql / import_from_sql ───────────────────────────────

    /// Export all tables as a SQL dump (CREATE TABLE + INSERT statements).
    ///
    /// Enumerates all tables via `GlueStore::fetch_all_schemas`, emits a
    /// `CREATE TABLE …;` DDL statement for each table, then selects all rows
    /// and emits one `INSERT INTO … VALUES (…);` statement per row.
    ///
    /// The dump is suitable for round-tripping through
    /// [`import_from_sql`][Self::import_from_sql].
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if schema enumeration or any
    /// `SELECT *` query fails.
    pub async fn export_as_sql(&self) -> Result<String, OxiSqlError> {
        // 1. Collect all schemas (table definitions).
        let schemas: Vec<gluesql::core::data::Schema> = {
            let guard = self.inner.lock().await;
            guard
                .storage
                .fetch_all_schemas()
                .await
                .map_err(|e| OxiSqlError::Execution(format!("fetch_all_schemas: {e}")))?
        };

        let mut out = String::new();

        // 2. Emit CREATE TABLE DDL for every schema (includes CREATE INDEX lines).
        for schema in &schemas {
            let ddl = schema.to_ddl();
            out.push_str(&ddl);
            out.push('\n');
        }
        if !schemas.is_empty() {
            out.push('\n');
        }

        // 3. For each table, SELECT * and emit INSERT statements.
        for schema in &schemas {
            let table_name = &schema.table_name;
            let quoted = quote_ident(table_name);
            let select_sql = format!("SELECT * FROM {quoted}");

            // Build column-name list from schema (for the INSERT column list).
            let col_names: Vec<String> = schema
                .column_defs
                .as_deref()
                .map(|defs| defs.iter().map(|c| quote_ident(&c.name)).collect())
                .unwrap_or_default();

            let payloads: Vec<Payload> = {
                let mut guard = self.inner.lock().await;
                guard.execute(&select_sql).await.map_err(|e| {
                    OxiSqlError::Execution(format!("SELECT * FROM {table_name}: {e}"))
                })?
            };

            let mut row_count: usize = 0;
            for payload in payloads {
                if let Payload::Select { rows, .. } = payload {
                    for row_vals in &rows {
                        let values_str: Vec<String> = row_vals.iter().map(|v| v.to_sql()).collect();
                        let insert = if col_names.is_empty() {
                            format!("INSERT INTO {quoted} VALUES ({});\n", values_str.join(", "))
                        } else {
                            format!(
                                "INSERT INTO {quoted} ({}) VALUES ({});\n",
                                col_names.join(", "),
                                values_str.join(", ")
                            )
                        };
                        out.push_str(&insert);
                        row_count += 1;
                    }
                }
            }
            if row_count > 0 {
                out.push('\n');
            }
        }

        Ok(out)
    }

    /// Import/execute a SQL dump string.
    ///
    /// Executes each statement in `sql` sequentially using
    /// [`execute_batch`][Connection::execute_batch].  This is equivalent to
    /// calling `execute_batch` directly; the method exists as a named
    /// counterpart to [`export_as_sql`][Self::export_as_sql].
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if any statement in the dump fails.
    pub async fn import_from_sql(&self, sql: &str) -> Result<(), OxiSqlError> {
        self.execute_batch(sql).await?;
        Ok(())
    }

    // ── CSV import / export ───────────────────────────────────────────────────

    /// Import CSV data into a new table.
    ///
    /// The first row of the CSV is treated as the header — it provides the
    /// column names for the newly created table.  All columns are declared
    /// `TEXT`; explicit `CAST` in subsequent queries can convert to other types.
    ///
    /// # Behaviour
    ///
    /// - A new table named `table_name` is created (will error if it already
    ///   exists — drop it first if you want to re-import).
    /// - Each subsequent CSV row becomes one `INSERT` statement.
    /// - Empty fields are imported as `NULL`.
    /// - Column names are sanitised: spaces/hyphens become underscores, leading
    ///   digits get a `col_` prefix, non-ASCII characters are stripped.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Other`] if the CSV cannot be parsed, if any
    /// column name cannot be sanitised, or if no header row is present.
    /// Returns [`OxiSqlError::Execution`] if any DDL/DML statement fails.
    pub async fn import_csv(&self, table_name: &str, csv_data: &str) -> Result<usize, OxiSqlError> {
        let rows = csv::parse_csv(csv_data)?;

        if rows.is_empty() {
            return Ok(0);
        }

        // First row is the header
        let headers = &rows[0];
        if headers.is_empty() {
            return Err(OxiSqlError::Other("CSV import: header row is empty".into()));
        }

        // Sanitise all column names
        let columns: Vec<String> = headers
            .iter()
            .map(|h| csv::sanitise_column_name(h))
            .collect::<Result<Vec<_>, _>>()?;

        // CREATE TABLE
        let ddl = csv::build_create_table_sql(table_name, &columns);
        self.execute(&ddl, &[]).await?;

        // INSERT each data row
        let data_rows = &rows[1..];
        let mut inserted = 0usize;
        for row in data_rows {
            if row.is_empty() || row.iter().all(|f| f.is_empty()) {
                continue; // skip blank rows
            }
            // Pad or truncate to match column count
            let values: Vec<String> = (0..columns.len())
                .map(|i| row.get(i).cloned().unwrap_or_default())
                .collect();
            let dml = csv::build_insert_sql(table_name, &columns, &values);
            self.execute(&dml, &[]).await?;
            inserted += 1;
        }

        Ok(inserted)
    }

    /// Export a table to RFC 4180-compliant CSV.
    ///
    /// Queries `SELECT * FROM {table_name}` and writes all rows as CSV with
    /// a header row containing the column names from the first result row.
    ///
    /// # Returns
    ///
    /// A `String` containing the CSV data (CRLF line endings per RFC 4180).
    /// Returns an empty string (header-only CSV) if the table has no rows.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the SELECT fails (e.g. table
    /// does not exist).
    pub async fn export_table_to_csv(&self, table_name: &str) -> Result<String, OxiSqlError> {
        let sql = format!("SELECT * FROM {table_name}");
        let rows = self.query(&sql, &[]).await?;

        // Derive column headers: prefer row metadata when rows exist,
        // fall back to schema introspection for empty tables.
        let headers: Vec<String> = if rows.is_empty() {
            // Use schema introspection to get column names for empty tables.
            let col_infos = self.columns(table_name).await.unwrap_or_default();
            if col_infos.is_empty() {
                // No column info available — return empty string
                return Ok(String::new());
            }
            col_infos.into_iter().map(|c| c.name).collect()
        } else {
            rows[0].columns().to_vec()
        };

        // Convert rows to Vec<Vec<Value>>
        let value_rows: Vec<Vec<Value>> = rows
            .into_iter()
            .map(|row| {
                let col_count = row.column_count();
                (0..col_count)
                    .map(|i| row.get_by_index(i).cloned().unwrap_or(Value::Null))
                    .collect()
            })
            .collect();

        Ok(csv::build_csv_output(&headers, &value_rows))
    }

    // ── oxisql-parse integration ──────────────────────────────────────────────

    /// Normalize a SQL query to its canonical form for cache key purposes.
    ///
    /// Uses `oxisql_parse::normalize` to strip comments, standardize whitespace,
    /// and produce a stable representation of the SQL.  The normalized form is
    /// safe to use as a prepared-statement cache key.
    ///
    /// This function is infallible: it always returns a non-empty string.
    /// If the input is empty or whitespace-only the returned string will be
    /// empty after normalization.
    pub fn normalize_sql(sql: &str) -> String {
        oxisql_parse::normalize(sql)
    }

    /// Check if a SQL string is read-only (SELECT/EXPLAIN/SHOW only).
    ///
    /// Uses `oxisql_parse::parse_one` to parse the statement and
    /// `oxisql_parse::is_read_only` for fast AST-level classification.
    ///
    /// Returns `false` when the SQL cannot be parsed (e.g. for GlueSQL-specific
    /// syntax that the generic parser does not recognise) or when the statement
    /// modifies data or schema.
    pub fn is_read_only_sql(sql: &str) -> bool {
        match oxisql_parse::parse_one(sql) {
            Ok(stmt) => oxisql_parse::is_read_only(&stmt),
            Err(_) => false,
        }
    }
}

// ── SQL identifier quoting ───────────────────────────────────────────────────

/// Wrap an identifier in double-quotes, escaping internal double-quote chars
/// by doubling them (`"` → `""`).
///
/// This produces a standard SQL quoted identifier that is safe to embed in
/// `CREATE TABLE`, `INSERT INTO`, and `SELECT` statements.
fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

// ── FTS dispatch helpers ─────────────────────────────────────────────────────

/// Attempt to handle a DML statement against the FTS index.
///
/// Substitutes parameters in `sql` before dispatching so that the parser sees
/// concrete values.  Returns `None` when the SQL is not FTS-related.
fn try_fts_execute(
    sql: &str,
    params: &[&dyn ToSqlValue],
    fts: &Arc<RwLock<FtsIndex>>,
) -> Option<Result<u64, OxiSqlError>> {
    // Substitute parameters so the parser sees plain literals.
    let resolved = match substitute_params(sql, params) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let upper = resolved.trim().to_ascii_uppercase();

    if upper.starts_with("CREATE VIRTUAL TABLE") {
        let mut guard = match fts
            .write()
            .map_err(|e| OxiSqlError::Other(format!("FTS index write lock poisoned: {e}")))
        {
            Ok(g) => g,
            Err(e) => return Some(Err(e)),
        };
        return fts::handle_create_virtual_table(&resolved, &mut guard);
    }

    if upper.starts_with("INSERT INTO ") {
        // Only attempt FTS insert if the table is registered; otherwise fall through.
        let table_name = resolved
            .trim()
            .trim_end_matches(';')
            .trim_start_matches(|c: char| !c.is_alphanumeric() && c != '_')["INSERT INTO ".len()..]
            .trim_start()
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_string();

        let is_fts = fts
            .read()
            .map(|g| g.is_fts_table(&table_name))
            .unwrap_or(false);

        if is_fts {
            let mut guard = match fts
                .write()
                .map_err(|e| OxiSqlError::Other(format!("FTS index write lock poisoned: {e}")))
            {
                Ok(g) => g,
                Err(e) => return Some(Err(e)),
            };
            return fts::handle_fts_insert(&resolved, &mut guard);
        }
    }

    None
}

/// Attempt to handle a MATCH query against the FTS index.
///
/// Returns `None` when the SQL is not an FTS MATCH query or the table is not
/// registered.
fn try_fts_query(
    sql: &str,
    params: &[&dyn ToSqlValue],
    fts: &Arc<RwLock<FtsIndex>>,
) -> Option<Result<Vec<Row>, OxiSqlError>> {
    let upper = sql.trim().to_ascii_uppercase();
    if !upper.contains(" MATCH ") {
        return None;
    }
    // Substitute parameters so the MATCH string is a literal.
    let resolved = match substitute_params(sql, params) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let guard = match fts
        .read()
        .map_err(|e| OxiSqlError::Other(format!("FTS index read lock poisoned: {e}")))
    {
        Ok(g) => g,
        Err(e) => return Some(Err(e)),
    };
    fts::handle_fts_match(&resolved, &guard)
}

// ── B-tree index helpers ─────────────────────────────────────────────────────

/// Intercept `CREATE INDEX` and `DROP INDEX` statements.
///
/// Returns `Some(Ok(0))` when the statement was handled, `Some(Err(_))` on
/// error, and `None` when the SQL is not a CREATE/DROP INDEX statement.
fn try_btree_index_execute(
    sql: &str,
    params: &[&dyn ToSqlValue],
    index_registry: &Arc<std::sync::Mutex<IndexRegistry>>,
) -> Option<Result<u64, OxiSqlError>> {
    // Substitute parameters so the parser sees literal values.
    let resolved = match substitute_params(sql, params) {
        Ok(s) => s,
        Err(e) => return Some(Err(e)),
    };
    let upper = resolved.trim().to_ascii_uppercase();

    if upper.starts_with("CREATE INDEX") || upper.starts_with("CREATE UNIQUE INDEX") {
        if let Some((index_name, table, column)) = btree_index::parse_create_index(&resolved) {
            let mut reg = match index_registry
                .lock()
                .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))
            {
                Ok(g) => g,
                Err(e) => return Some(Err(e)),
            };
            reg.create_named_index(&index_name, &table, &column);
            return Some(Ok(0));
        }
    }

    if upper.starts_with("DROP INDEX") {
        if let Some((table, column)) = btree_index::parse_drop_index(&resolved) {
            let mut reg = match index_registry
                .lock()
                .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))
            {
                Ok(g) => g,
                Err(e) => return Some(Err(e)),
            };
            reg.drop_index(&table, &column);
            return Some(Ok(0));
        }
    }

    None
}

/// After a successful INSERT, update all registered B-tree indexes.
///
/// Best-effort: silently ignores parse errors so that the INSERT result is not
/// affected by index-maintenance failures.
fn update_btree_index_on_insert(
    sql: &str,
    params: &[&dyn ToSqlValue],
    index_registry: &Arc<std::sync::Mutex<IndexRegistry>>,
) {
    let resolved = match substitute_params(sql, params) {
        Ok(s) => s,
        Err(_) => return,
    };
    let upper = resolved.trim().to_ascii_uppercase();
    if !upper.starts_with("INSERT INTO") {
        return;
    }
    let info = match btree_index::parse_insert_values(&resolved) {
        Some(i) => i,
        None => return,
    };
    let mut reg = match index_registry.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    for (col, raw_val) in &info.pairs {
        if reg.has_index(&info.table, col) {
            let key = btree_index::sql_literal_to_index_key(raw_val);
            reg.index_row(&info.table, col, key);
        }
    }
}

#[async_trait]
impl Connection for EmbeddedConnection {
    async fn execute(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        if let Some(result) = handle_pragma(sql) {
            // PRAGMA was intercepted — consume the result and report 0 affected rows.
            let _ = result?;
            return Ok(0);
        }
        if let Some(result) = handle_attach(sql) {
            // ATTACH was intercepted — return a meaningful error rather than a
            // raw GlueSQL parse error.
            let _ = result?;
            return Ok(0);
        }
        // FTS virtual table interception (CREATE VIRTUAL TABLE / INSERT).
        if let Some(result) = try_fts_execute(sql, params, &self.fts_index) {
            return result;
        }
        // B-tree index: intercept CREATE INDEX / DROP INDEX.
        if let Some(result) = try_btree_index_execute(sql, params, &self.index_registry) {
            return result;
        }
        let affected = {
            let mut guard = self.inner.lock().await;
            glue_execute(&mut guard, sql, params).await?
        };
        // After a successful INSERT: update B-tree indexes for the new row.
        update_btree_index_on_insert(sql, params, &self.index_registry);
        Ok(affected)
    }

    async fn query(&self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        if let Some(result) = handle_pragma(sql) {
            return result;
        }
        if let Some(result) = handle_attach(sql) {
            return result;
        }
        // Virtual table interception: check before FTS so vtable names take
        // precedence when both are registered.
        if let Some(vtable_name) = self.vtable_registry.find_referenced(sql) {
            return Ok(self
                .vtable_registry
                .scan_with_filter(&vtable_name, sql)
                .unwrap_or_default());
        }
        // FTS MATCH query interception.
        if let Some(result) = try_fts_query(sql, params, &self.fts_index) {
            return result;
        }
        let mut guard = self.inner.lock().await;
        glue_query(&mut guard, sql, params).await
    }

    async fn transaction(&self) -> Result<Box<dyn Transaction + '_>, OxiSqlError> {
        // Acquire the mutex as an `OwnedMutexGuard` so the transaction can be
        // `'static` (required for `Box<dyn Transaction + '_>` across await
        // points without fighting the borrow checker).
        let guard = Arc::clone(&self.inner).lock_owned().await;
        let mut txn = EmbeddedTransaction { guard };
        txn.execute("BEGIN", &[]).await?;
        Ok(Box::new(txn))
    }

    async fn execute_batch(&self, sql: &str) -> Result<u64, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        let payloads = guard
            .execute(sql)
            .await
            .map_err(|e| OxiSqlError::Execution(e.to_string()))?;
        Ok(payloads.iter().map(payload_to_affected_rows).sum())
    }

    async fn ping(&self) -> Result<(), OxiSqlError> {
        // In-memory connections are always alive — just verify the lock is
        // obtainable.
        let _guard = self.inner.lock().await;
        Ok(())
    }

    async fn prepare(&self, sql: &str) -> Result<Box<dyn PreparedStatement + '_>, OxiSqlError> {
        Ok(Box::new(EmbeddedPrepared {
            inner: Arc::clone(&self.inner),
            sql_text: sql.to_string(),
        }))
    }

    /// List all tables visible in this in-memory database.
    ///
    /// Calls `GlueStore::fetch_all_schemas` on the underlying
    /// `MemoryStorage` and maps each `Schema` to a [`TableInfo`].
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the GlueSQL catalog call fails.
    async fn tables(&self) -> Result<Vec<TableInfo>, OxiSqlError> {
        let guard = self.inner.lock().await;
        let schemas = guard
            .storage
            .fetch_all_schemas()
            .await
            .map_err(|e| OxiSqlError::Execution(format!("fetch_all_schemas: {e}")))?;
        Ok(schemas
            .into_iter()
            .map(|s| TableInfo {
                name: s.table_name,
                schema: None,
                table_type: TableType::Base,
            })
            .collect())
    }

    /// List all columns in the named table.
    ///
    /// Calls `GlueStore::fetch_schema` and maps each
    /// `gluesql::core::ast::ColumnDef` to a [`ColumnInfo`].
    /// Returns an empty `Vec` when the table does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the GlueSQL catalog call fails.
    async fn columns(&self, table: &str) -> Result<Vec<ColumnInfo>, OxiSqlError> {
        let guard = self.inner.lock().await;
        let schema = guard
            .storage
            .fetch_schema(table)
            .await
            .map_err(|e| OxiSqlError::Execution(format!("fetch_schema({table}): {e}")))?;
        let defs = match schema.and_then(|s| s.column_defs) {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };
        Ok(defs
            .into_iter()
            .enumerate()
            .map(|(i, col)| ColumnInfo {
                name: col.name.clone(),
                ordinal_position: u32::try_from(i + 1).unwrap_or(u32::MAX),
                data_type: glue_data_type_to_str(&col.data_type).to_owned(),
                nullable: col.nullable,
                default: col.default.as_ref().map(|expr| {
                    use gluesql::core::ast::ToSql as _;
                    expr.to_sql()
                }),
                max_length: None,
                numeric_precision: None,
                numeric_scale: None,
            })
            .collect())
    }

    /// List all indexes defined on the named table.
    ///
    /// Merges two sources:
    ///
    /// 1. The GlueSQL catalog (`fetch_schema`) — indexes that were stored via
    ///    GlueSQL's own schema persistence (unlikely on MemoryStorage, but
    ///    handled for completeness).
    /// 2. The [`IndexRegistry`] — indexes created via `CREATE INDEX` statements,
    ///    which are intercepted by `EmbeddedConnection::execute` before they reach
    ///    GlueSQL (GlueSQL MemoryStorage does not support `CREATE INDEX`).
    ///
    /// For each GlueSQL `SchemaIndex` the column name is extracted from the
    /// index expression when it is a simple `Expr::Identifier`; compound
    /// expressions fall back to the rendered SQL text of the expression.
    ///
    /// Returns an empty `Vec` when the table does not exist or has no indexes.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the GlueSQL catalog call fails.
    async fn indexes(&self, table: &str) -> Result<Vec<IndexInfo>, OxiSqlError> {
        // 1. Collect GlueSQL schema indexes (acquires and releases the lock).
        let glue_indexes: Vec<IndexInfo> = {
            let guard = self.inner.lock().await;
            let schema = guard
                .storage
                .fetch_schema(table)
                .await
                .map_err(|e| OxiSqlError::Execution(format!("fetch_schema({table}): {e}")))?;
            match schema {
                None => Vec::new(),
                Some(s) => s
                    .indexes
                    .into_iter()
                    .map(|idx| {
                        // Extract a human-readable column name from the index expression.
                        // GlueSQL index expressions are typically `Expr::Identifier(col)`;
                        // fall back to the SQL-rendered text for compound expressions.
                        let col_name = match &idx.expr {
                            gluesql::core::ast::Expr::Identifier(name) => name.clone(),
                            other => {
                                use gluesql::core::ast::ToSql as _;
                                other.to_sql()
                            }
                        };
                        IndexInfo {
                            name: idx.name,
                            columns: vec![col_name],
                            unique: false,
                            primary: false,
                        }
                    })
                    .collect(),
            }
        };

        // 2. Collect indexes from the IndexRegistry (intercept-based CREATE INDEX).
        let registry_indexes: Vec<IndexInfo> = {
            let reg = self
                .index_registry
                .lock()
                .map_err(|e| OxiSqlError::Other(format!("index registry lock poisoned: {e}")))?;
            reg.named_indexes_for_table(table)
                .iter()
                .map(|ni| IndexInfo {
                    name: ni.index_name.clone(),
                    columns: vec![ni.column.clone()],
                    unique: false,
                    primary: false,
                })
                .collect()
        };

        // Merge: GlueSQL indexes first, then registry indexes (deduplicate by name).
        let mut seen_names = std::collections::HashSet::new();
        let mut merged: Vec<IndexInfo> = Vec::new();
        for idx in glue_indexes.into_iter().chain(registry_indexes) {
            if seen_names.insert(idx.name.clone()) {
                merged.push(idx);
            }
        }
        Ok(merged)
    }

    /// List foreign-key constraints on the named table.
    ///
    /// GlueSQL stores FK metadata in the `Schema` `foreign_keys` field, so
    /// this method can return real FK data when the schema was created with
    /// `FOREIGN KEY` clauses.  `MemoryStorage` does retain FK definitions at
    /// the schema level (they are included in `to_ddl()` output), so this
    /// will return the constraints that were declared via `CREATE TABLE`.
    ///
    /// Returns an empty `Vec` when the table does not exist or has no FKs.
    ///
    /// # Errors
    ///
    /// Returns [`OxiSqlError::Execution`] if the GlueSQL catalog call fails.
    async fn foreign_keys(&self, table: &str) -> Result<Vec<ForeignKeyInfo>, OxiSqlError> {
        let guard = self.inner.lock().await;
        let schema = guard
            .storage
            .fetch_schema(table)
            .await
            .map_err(|e| OxiSqlError::Execution(format!("fetch_schema({table}): {e}")))?;
        let schema = match schema {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };
        Ok(schema
            .foreign_keys
            .into_iter()
            .map(|fk| ForeignKeyInfo {
                constraint_name: fk.name,
                column: fk.referencing_column_name,
                foreign_table: fk.referenced_table_name,
                foreign_column: fk.referenced_column_name,
                ..Default::default()
            })
            .collect())
    }
}

// ── EmbeddedTransaction ──────────────────────────────────────────────────────

/// A serialised transaction over an [`EmbeddedConnection`].
///
/// Holds an [`OwnedMutexGuard`] for the duration of the transaction, ensuring
/// no other operation can interleave between `BEGIN` and `COMMIT`/`ROLLBACK`.
pub struct EmbeddedTransaction {
    guard: OwnedMutexGuard<Glue<MemoryStorage>>,
}

#[async_trait]
impl Transaction for EmbeddedTransaction {
    async fn execute(&mut self, sql: &str, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        glue_execute(&mut self.guard, sql, params).await
    }

    async fn query(
        &mut self,
        sql: &str,
        params: &[&dyn ToSqlValue],
    ) -> Result<Vec<Row>, OxiSqlError> {
        glue_query(&mut self.guard, sql, params).await
    }

    async fn commit(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        glue_execute(&mut self.guard, "COMMIT", &[]).await?;
        Ok(())
    }

    async fn rollback(mut self: Box<Self>) -> Result<(), OxiSqlError> {
        glue_execute(&mut self.guard, "ROLLBACK", &[]).await?;
        Ok(())
    }

    async fn savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by GlueSQL MemoryStorage".into(),
        ))
    }

    async fn release_savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by GlueSQL MemoryStorage".into(),
        ))
    }

    async fn rollback_to_savepoint(&mut self, _name: &str) -> Result<(), OxiSqlError> {
        Err(OxiSqlError::Other(
            "savepoints are not supported by GlueSQL MemoryStorage".into(),
        ))
    }
}

// ── EmbeddedPrepared ─────────────────────────────────────────────────────────

/// A "prepared" statement for the embedded backend.
///
/// GlueSQL has no native server-side prepared statement API.  This type
/// caches the SQL text and shares the connection's `Arc<Mutex<Glue>>` so
/// re-execution skips SQL re-parsing at the OxiSQL facade layer.  Actual
/// GlueSQL parsing happens on every call (a known limitation of the embedded
/// backend; see TODO.md P6).
pub struct EmbeddedPrepared {
    inner: Arc<Mutex<Glue<MemoryStorage>>>,
    sql_text: String,
}

#[async_trait]
impl PreparedStatement for EmbeddedPrepared {
    async fn execute(&mut self, params: &[&dyn ToSqlValue]) -> Result<u64, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        glue_execute(&mut guard, &self.sql_text, params).await
    }

    async fn query(&mut self, params: &[&dyn ToSqlValue]) -> Result<Vec<Row>, OxiSqlError> {
        let mut guard = self.inner.lock().await;
        glue_query(&mut guard, &self.sql_text, params).await
    }

    fn sql(&self) -> &str {
        &self.sql_text
    }
}

// ── FjallEmbeddedConnection ──────────────────────────────────────────────────

#[cfg(feature = "fjall-storage")]
pub mod fjall_conn;
#[cfg(feature = "fjall-storage")]
pub use fjall_conn::FjallEmbeddedConnection;

// ── RedbEmbeddedConnection ───────────────────────────────────────────────────

#[cfg(feature = "redb-storage")]
pub mod redb_conn;
#[cfg(feature = "redb-storage")]
pub use redb_conn::RedbEmbeddedConnection;

// ── SledEmbeddedConnection ───────────────────────────────────────────────────

#[cfg(feature = "sled-storage")]
mod sled_storage;
#[cfg(feature = "sled-storage")]
pub use sled_storage::SledGlueStorage;

#[cfg(feature = "sled-storage")]
pub mod sled_conn;
#[cfg(feature = "sled-storage")]
pub use sled_conn::SledEmbeddedConnection;
