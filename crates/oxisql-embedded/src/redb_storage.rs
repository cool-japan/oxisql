//! `RedbGlueStorage` — GlueSQL storage backend backed by a redb embedded database.
//!
//! Uses two redb tables:
//! - `glue_schemas` — maps `&[u8]` (table name as UTF-8) → `&[u8]` (JSON-serialised `Schema`).
//! - `glue_data` — maps `&[u8]` (compound key `"{table}\x00{encoded_key}"` for rows,
//!   `"\x01counter\x00{table}"` for auto-increment counters) → `&[u8]`
//!   (JSON-serialised `DataRow` for rows, big-endian i64 for counters).
//!
//! A third table `glue_functions` stores custom GlueSQL functions.
//!
//! # Key encoding
//!
//! GlueSQL row keys are encoded to a binary format that preserves sort order
//! within the same type. Different types use different leading tag bytes so
//! they never collide:
//!
//! | Variant     | Tag  | Payload                                    |
//! |-------------|------|--------------------------------------------|
//! | `None`      | 0x00 | *(empty)*                                  |
//! | `Bool`      | 0x01 | 0x00 / 0x01                                |
//! | `I8`        | 0x10 | sign-flipped big-endian i8                 |
//! | `I16`       | 0x11 | sign-flipped big-endian i16                |
//! | `I32`       | 0x12 | sign-flipped big-endian i32                |
//! | `I64`       | 0x13 | sign-flipped big-endian i64                |
//! | `I128`      | 0x14 | sign-flipped big-endian i128               |
//! | `U8`        | 0x20 | big-endian u8                              |
//! | `U16`       | 0x21 | big-endian u16                             |
//! | `U32`       | 0x22 | big-endian u32                             |
//! | `U64`       | 0x23 | big-endian u64                             |
//! | `U128`/Uuid | 0x24 | big-endian u128                            |
//! | `F32`       | 0x30 | big-endian f32 bits                        |
//! | `F64`       | 0x31 | big-endian f64 bits                        |
//! | `Decimal`   | 0x40 | UTF-8 decimal string                       |
//! | `Str`       | 0x50 | UTF-8 bytes                                |
//! | `Bytea`     | 0x60 | raw bytes                                  |
//! | `Date`      | 0x70 | sign-flipped days-since-epoch i64          |
//! | `Timestamp` | 0x71 | sign-flipped microseconds-since-epoch i64  |
//! | `Time`      | 0x72 | big-endian microseconds-since-midnight i64 |
//! | `Inet`      | 0x80 | UTF-8 IP address string                    |
//! | `Interval`  | 0x90 | JSON via serde_json                        |
//! | other       | 0xFF | JSON fallback (non-order-preserving)       |

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::iter;
use gluesql::core::chrono::Utc;
use gluesql::core::data::{CustomFunction as GlueCustomFunction, Key, Schema, Value};
use gluesql::core::error::{Error as GlueError, Result as GlueResult};
use gluesql::core::store::{
    AlterTable, CustomFunction, CustomFunctionMut, DataRow, Index, IndexMut, MetaIter, Metadata,
    Planner, RowIter, Store, StoreMut, Transaction,
};
use redb::{Database, DatabaseError, ReadableDatabase, ReadableTable, TableDefinition};

// ── redb table definitions ────────────────────────────────────────────────────

/// Stores GlueSQL schemas: `table_name_bytes → JSON(Schema)`.
const SCHEMA_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("glue_schemas");

/// Stores GlueSQL rows and per-table auto-increment counters.
///
/// Row keys have the format: `"{table}\x00{encoded_row_key}"`.
/// Counter keys have the format: `"\x01counter\x00{table}"` — the leading
/// `\x01` byte ensures counter keys never collide with any valid table name.
const DATA_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("glue_data");

/// Stores GlueSQL custom functions: `UPPER(name)_bytes → JSON(CustomFunction)`.
const FUNC_TABLE: TableDefinition<&[u8], &[u8]> = TableDefinition::new("glue_functions");

// ── order-preserving key encoding ─────────────────────────────────────────────

/// Encode a GlueSQL [`Key`] into bytes that sort in the same order as `Key::Ord`.
fn encode_key(key: &Key) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    match key {
        Key::None => buf.push(0x00),
        Key::Bool(b) => {
            buf.push(0x01);
            buf.push(u8::from(*b));
        }
        Key::I8(n) => {
            buf.push(0x10);
            buf.push((*n as u8) ^ 0x80);
        }
        Key::I16(n) => {
            buf.push(0x11);
            buf.extend_from_slice(&((*n as u16) ^ 0x8000u16).to_be_bytes());
        }
        Key::I32(n) => {
            buf.push(0x12);
            buf.extend_from_slice(&((*n as u32) ^ 0x8000_0000u32).to_be_bytes());
        }
        Key::I64(n) => {
            buf.push(0x13);
            buf.extend_from_slice(&((*n as u64) ^ 0x8000_0000_0000_0000u64).to_be_bytes());
        }
        Key::I128(n) => {
            buf.push(0x14);
            buf.extend_from_slice(&((*n as u128) ^ (1u128 << 127)).to_be_bytes());
        }
        Key::U8(n) => {
            buf.push(0x20);
            buf.push(*n);
        }
        Key::U16(n) => {
            buf.push(0x21);
            buf.extend_from_slice(&n.to_be_bytes());
        }
        Key::U32(n) => {
            buf.push(0x22);
            buf.extend_from_slice(&n.to_be_bytes());
        }
        Key::U64(n) => {
            buf.push(0x23);
            buf.extend_from_slice(&n.to_be_bytes());
        }
        Key::U128(n) | Key::Uuid(n) => {
            buf.push(0x24);
            buf.extend_from_slice(&n.to_be_bytes());
        }
        Key::F32(f) => {
            buf.push(0x30);
            buf.extend_from_slice(&f.0.to_bits().to_be_bytes());
        }
        Key::F64(f) => {
            buf.push(0x31);
            buf.extend_from_slice(&f.0.to_bits().to_be_bytes());
        }
        Key::Decimal(d) => {
            buf.push(0x40);
            buf.extend_from_slice(d.to_string().as_bytes());
        }
        Key::Str(s) => {
            buf.push(0x50);
            buf.extend_from_slice(s.as_bytes());
        }
        Key::Bytea(b) => {
            buf.push(0x60);
            buf.extend_from_slice(b);
        }
        Key::Date(d) => {
            buf.push(0x70);
            let epoch = gluesql::core::chrono::NaiveDate::from_ymd_opt(1, 1, 1).unwrap_or_default();
            let days = d.signed_duration_since(epoch).num_days();
            // sign-flip for sort order
            buf.extend_from_slice(&((days as u64) ^ 0x8000_0000_0000_0000u64).to_be_bytes());
        }
        Key::Timestamp(ts) => {
            buf.push(0x71);
            let micros = ts.and_utc().timestamp_micros();
            buf.extend_from_slice(&((micros as u64) ^ 0x8000_0000_0000_0000u64).to_be_bytes());
        }
        Key::Time(t) => {
            buf.push(0x72);
            let midnight =
                gluesql::core::chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
            let micros = t
                .signed_duration_since(midnight)
                .num_microseconds()
                .unwrap_or(0);
            buf.extend_from_slice(&(micros as u64).to_be_bytes());
        }
        Key::Inet(ip) => {
            buf.push(0x80);
            buf.extend_from_slice(ip.to_string().as_bytes());
        }
        Key::Interval(interval) => {
            buf.push(0x90);
            let json = serde_json::to_string(interval).unwrap_or_default();
            buf.extend_from_slice(json.as_bytes());
        }
    }
    buf
}

/// Decode a byte slice produced by [`encode_key`] back into a [`Key`].
///
/// Returns `None` for unknown tags or malformed payloads.
fn decode_key(encoded: &[u8]) -> Option<Key> {
    use gluesql::core::chrono::{NaiveDate, NaiveTime};

    let (&tag, rest) = encoded.split_first()?;
    match tag {
        0x00 => Some(Key::None),
        0x01 => {
            let b = rest.first()?;
            Some(Key::Bool(*b != 0))
        }
        0x10 => {
            let byte = rest.first()?;
            Some(Key::I8((byte ^ 0x80) as i8))
        }
        0x11 => {
            let arr: [u8; 2] = rest.get(..2)?.try_into().ok()?;
            Some(Key::I16((u16::from_be_bytes(arr) ^ 0x8000u16) as i16))
        }
        0x12 => {
            let arr: [u8; 4] = rest.get(..4)?.try_into().ok()?;
            Some(Key::I32((u32::from_be_bytes(arr) ^ 0x8000_0000u32) as i32))
        }
        0x13 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            Some(Key::I64(
                (u64::from_be_bytes(arr) ^ 0x8000_0000_0000_0000u64) as i64,
            ))
        }
        0x14 => {
            let arr: [u8; 16] = rest.get(..16)?.try_into().ok()?;
            Some(Key::I128(
                (u128::from_be_bytes(arr) ^ (1u128 << 127)) as i128,
            ))
        }
        0x20 => Some(Key::U8(*rest.first()?)),
        0x21 => {
            let arr: [u8; 2] = rest.get(..2)?.try_into().ok()?;
            Some(Key::U16(u16::from_be_bytes(arr)))
        }
        0x22 => {
            let arr: [u8; 4] = rest.get(..4)?.try_into().ok()?;
            Some(Key::U32(u32::from_be_bytes(arr)))
        }
        0x23 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            Some(Key::U64(u64::from_be_bytes(arr)))
        }
        0x24 => {
            let arr: [u8; 16] = rest.get(..16)?.try_into().ok()?;
            Some(Key::U128(u128::from_be_bytes(arr)))
        }
        0x30 => {
            let arr: [u8; 4] = rest.get(..4)?.try_into().ok()?;
            let bits = u32::from_be_bytes(arr);
            // Build through gluesql's own `Value -> Key` conversion so the
            // `OrderedFloat` wrapper comes from gluesql's pinned `ordered-float`
            // version (avoids the multi-version mismatch with the workspace crate).
            Key::try_from(Value::F32(f32::from_bits(bits))).ok()
        }
        0x31 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            let bits = u64::from_be_bytes(arr);
            Key::try_from(Value::F64(f64::from_bits(bits))).ok()
        }
        0x40 => {
            let s = std::str::from_utf8(rest).ok()?;
            s.parse().ok().map(Key::Decimal)
        }
        0x50 => {
            let s = std::str::from_utf8(rest).ok()?;
            Some(Key::Str(s.to_owned()))
        }
        0x60 => Some(Key::Bytea(rest.to_vec())),
        0x70 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            let days = (u64::from_be_bytes(arr) ^ 0x8000_0000_0000_0000u64) as i64;
            let epoch = NaiveDate::from_ymd_opt(1, 1, 1).unwrap_or_default();
            let d = epoch + gluesql::core::chrono::Duration::try_days(days)?;
            Some(Key::Date(d))
        }
        0x71 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            let micros = (u64::from_be_bytes(arr) ^ 0x8000_0000_0000_0000u64) as i64;
            // Use DateTime::from_timestamp_micros then strip timezone.
            let dt = gluesql::core::chrono::DateTime::from_timestamp_micros(micros)?.naive_utc();
            Some(Key::Timestamp(dt))
        }
        0x72 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            let micros = u64::from_be_bytes(arr) as i64;
            let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
            let t = midnight + gluesql::core::chrono::Duration::microseconds(micros);
            Some(Key::Time(t))
        }
        0x80 => {
            let s = std::str::from_utf8(rest).ok()?;
            s.parse().ok().map(Key::Inet)
        }
        0x90 => serde_json::from_slice(rest).ok().map(Key::Interval),
        _ => None,
    }
}

// ── compound data-key helpers ─────────────────────────────────────────────────

/// Build the compound key `"{table}\x00{encoded_row_key}"` for the data table.
fn data_key(table: &str, row_key: &Key) -> Vec<u8> {
    let mut k = Vec::with_capacity(table.len() + 1 + 9);
    k.extend_from_slice(table.as_bytes());
    k.push(0x00);
    k.extend_from_slice(&encode_key(row_key));
    k
}

/// Build the compound prefix `"{table}\x00"` for range scans.
fn data_prefix(table: &str) -> Vec<u8> {
    let mut k = Vec::with_capacity(table.len() + 1);
    k.extend_from_slice(table.as_bytes());
    k.push(0x00);
    k
}

/// The special key used to store the auto-increment counter for a table.
///
/// The leading `\x01` byte is below any valid UTF-8 table-name prefix (table
/// names are UTF-8 identifiers whose first byte is always ≥ 0x41/'A'), so
/// counter keys can never collide with data row keys regardless of what table
/// name the caller uses.
fn counter_key(table: &str) -> Vec<u8> {
    let mut k = b"\x01counter\x00".to_vec();
    k.extend_from_slice(table.as_bytes());
    k
}

/// The prefix used to identify counter keys (excluded from data scans).
const COUNTER_PREFIX: &[u8] = b"\x01counter\x00";

// ── error helpers ─────────────────────────────────────────────────────────────

fn redb_err(e: impl std::fmt::Display) -> GlueError {
    GlueError::StorageMsg(format!("redb: {e}"))
}

fn map_table_err(e: redb::TableError) -> GlueError {
    GlueError::StorageMsg(format!("redb table: {e}"))
}

fn map_commit_err(e: redb::CommitError) -> GlueError {
    GlueError::StorageMsg(format!("redb commit: {e}"))
}

fn map_txn_err(e: redb::TransactionError) -> GlueError {
    GlueError::StorageMsg(format!("redb txn: {e}"))
}

// ── RedbGlueStorage ───────────────────────────────────────────────────────────

/// A persistent GlueSQL storage backend backed by a [`redb`] embedded database.
///
/// `RedbGlueStorage` implements the full GlueSQL `GStore + GStoreMut` surface
/// required to use it as the storage backend for `gluesql::prelude::Glue`.
///
/// # Persistence
///
/// Each DDL/DML operation opens a redb write transaction, writes the change,
/// and commits immediately.  This means every `INSERT`, `CREATE TABLE`, etc.
/// is durable after the `await` returns.
///
/// # Transaction semantics
///
/// The `Transaction::begin` method returns an error for non-autocommit mode,
/// matching the `MemoryStorage` and `FjallGlueStorage` behaviour.
///
/// # In-memory mode
///
/// [`RedbGlueStorage::open_in_memory`] uses redb's `InMemoryBackend` so no
/// file is created — useful for testing.
pub struct RedbGlueStorage {
    db: Arc<Mutex<Database>>,
}

impl RedbGlueStorage {
    /// Open or create a persistent redb database at `path`.
    ///
    /// The file is created if it does not exist.  Tables are created on first
    /// open.  On success all previously written data is immediately visible.
    ///
    /// # Errors
    ///
    /// Propagates any [`DatabaseError`] from redb (I/O errors, version
    /// mismatch, corrupt database, etc.).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let db = Database::create(path.as_ref())?;
        Self::ensure_tables(&db)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Create an in-memory redb database (non-persistent, for testing).
    ///
    /// Uses redb's [`redb::backends::InMemoryBackend`] so no file is created.
    ///
    /// # Errors
    ///
    /// Propagates redb [`DatabaseError`] on failure.
    pub fn open_in_memory() -> Result<Self, DatabaseError> {
        let backend = redb::backends::InMemoryBackend::new();
        let db = redb::Database::builder().create_with_backend(backend)?;
        Self::ensure_tables(&db)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    /// Create all three redb tables if they do not yet exist.
    fn ensure_tables(db: &Database) -> Result<(), DatabaseError> {
        let txn = db.begin_write().map_err(|e| {
            DatabaseError::Storage(redb::StorageError::Io(std::io::Error::other(e.to_string())))
        })?;
        {
            txn.open_table(SCHEMA_TABLE).map_err(|e| {
                DatabaseError::Storage(redb::StorageError::Io(std::io::Error::other(e.to_string())))
            })?;
            txn.open_table(DATA_TABLE).map_err(|e| {
                DatabaseError::Storage(redb::StorageError::Io(std::io::Error::other(e.to_string())))
            })?;
            txn.open_table(FUNC_TABLE).map_err(|e| {
                DatabaseError::Storage(redb::StorageError::Io(std::io::Error::other(e.to_string())))
            })?;
        }
        txn.commit().map_err(|e| {
            DatabaseError::Storage(redb::StorageError::Io(std::io::Error::other(e.to_string())))
        })?;
        Ok(())
    }

    fn lock_db(&self) -> GlueResult<std::sync::MutexGuard<'_, Database>> {
        self.db
            .lock()
            .map_err(|e| GlueError::StorageMsg(format!("redb lock poisoned: {e}")))
    }

    // ── Schema helpers ────────────────────────────────────────────────────────

    fn read_schema_bytes(&self, table_name: &str) -> GlueResult<Option<Vec<u8>>> {
        let db = self.lock_db()?;
        let read_txn = db.begin_read().map_err(map_txn_err)?;
        let table = match read_txn.open_table(SCHEMA_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(map_table_err(e)),
        };
        Ok(table
            .get(table_name.as_bytes())
            .map_err(redb_err)?
            .map(|g| g.value().to_vec()))
    }

    fn read_all_schema_bytes(&self) -> GlueResult<Vec<(String, Vec<u8>)>> {
        let db = self.lock_db()?;
        let read_txn = db.begin_read().map_err(map_txn_err)?;
        let table = match read_txn.open_table(SCHEMA_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(vec![]),
            Err(e) => return Err(map_table_err(e)),
        };
        let mut result = Vec::new();
        for entry in table.iter().map_err(redb_err)? {
            let (k, v) = entry.map_err(redb_err)?;
            let name = std::str::from_utf8(k.value()).map_err(redb_err)?.to_owned();
            result.push((name, v.value().to_vec()));
        }
        Ok(result)
    }

    fn write_schema(&self, table_name: &str, bytes: &[u8]) -> GlueResult<()> {
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(SCHEMA_TABLE).map_err(map_table_err)?;
            table
                .insert(table_name.as_bytes(), bytes)
                .map_err(redb_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    fn delete_schema_and_data(&self, table_name: &str) -> GlueResult<()> {
        let prefix = data_prefix(table_name);
        let ck = counter_key(table_name);
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut schema_table = txn.open_table(SCHEMA_TABLE).map_err(map_table_err)?;
            schema_table
                .remove(table_name.as_bytes())
                .map_err(redb_err)?;

            let mut data_table = txn.open_table(DATA_TABLE).map_err(map_table_err)?;

            // Collect keys to delete (cannot delete while iterating).
            let keys_to_delete: Vec<Vec<u8>> = {
                let iter = data_table.range(prefix.as_slice()..).map_err(redb_err)?;
                let mut keys = Vec::new();
                for entry in iter {
                    let (k, _) = entry.map_err(redb_err)?;
                    let key_bytes = k.value();
                    if !key_bytes.starts_with(&prefix) {
                        break;
                    }
                    keys.push(key_bytes.to_vec());
                }
                keys
            };
            for key_bytes in &keys_to_delete {
                data_table.remove(key_bytes.as_slice()).map_err(redb_err)?;
            }
            // Delete counter key if present.
            data_table.remove(ck.as_slice()).map_err(redb_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    // ── Data helpers ──────────────────────────────────────────────────────────

    fn read_row(&self, table_name: &str, key: &Key) -> GlueResult<Option<Vec<u8>>> {
        let compound = data_key(table_name, key);
        let db = self.lock_db()?;
        let read_txn = db.begin_read().map_err(map_txn_err)?;
        let table = match read_txn.open_table(DATA_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
            Err(e) => return Err(map_table_err(e)),
        };
        Ok(table
            .get(compound.as_slice())
            .map_err(redb_err)?
            .map(|g| g.value().to_vec()))
    }

    fn scan_rows(&self, table_name: &str) -> GlueResult<Vec<(Key, Vec<u8>)>> {
        let prefix = data_prefix(table_name);
        let db = self.lock_db()?;
        let read_txn = db.begin_read().map_err(map_txn_err)?;
        let table = match read_txn.open_table(DATA_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(vec![]),
            Err(e) => return Err(map_table_err(e)),
        };
        let mut result = Vec::new();
        let iter = table.range(prefix.as_slice()..).map_err(redb_err)?;
        for entry in iter {
            let (k, v) = entry.map_err(redb_err)?;
            let key_bytes = k.value();
            if !key_bytes.starts_with(&prefix) {
                break;
            }
            // Skip auto-increment counter entries.
            if key_bytes.starts_with(COUNTER_PREFIX) {
                continue;
            }
            let row_key_bytes = &key_bytes[prefix.len()..];
            let glue_key = decode_key(row_key_bytes).ok_or_else(|| {
                GlueError::StorageMsg(format!(
                    "scan_rows: cannot decode key bytes {row_key_bytes:?}"
                ))
            })?;
            result.push((glue_key, v.value().to_vec()));
        }
        Ok(result)
    }

    fn write_rows(&self, rows: &[(Vec<u8>, Vec<u8>)]) -> GlueResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut data_table = txn.open_table(DATA_TABLE).map_err(map_table_err)?;
            for (compound_key, value_bytes) in rows {
                data_table
                    .insert(compound_key.as_slice(), value_bytes.as_slice())
                    .map_err(redb_err)?;
            }
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    fn delete_rows(&self, compound_keys: &[Vec<u8>]) -> GlueResult<()> {
        if compound_keys.is_empty() {
            return Ok(());
        }
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut data_table = txn.open_table(DATA_TABLE).map_err(map_table_err)?;
            for key in compound_keys {
                data_table.remove(key.as_slice()).map_err(redb_err)?;
            }
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    // ── Auto-increment helpers ────────────────────────────────────────────────

    fn read_counter(&self, table_name: &str) -> GlueResult<i64> {
        let ck = counter_key(table_name);
        let db = self.lock_db()?;
        let read_txn = db.begin_read().map_err(map_txn_err)?;
        let table = match read_txn.open_table(DATA_TABLE) {
            Ok(t) => t,
            Err(redb::TableError::TableDoesNotExist(_)) => return Ok(0),
            Err(e) => return Err(map_table_err(e)),
        };
        match table.get(ck.as_slice()).map_err(redb_err)? {
            None => Ok(0),
            Some(g) => {
                let bytes: [u8; 8] = g
                    .value()
                    .try_into()
                    .map_err(|_| redb_err("counter value corrupted"))?;
                Ok(i64::from_be_bytes(bytes))
            }
        }
    }

    fn append_rows_to_table(&self, table_name: &str, rows: Vec<DataRow>) -> GlueResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut counter = self.read_counter(table_name)?;
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut data_table = txn.open_table(DATA_TABLE).map_err(map_table_err)?;
            for row in rows {
                counter += 1;
                let row_key = Key::I64(counter);
                let compound = data_key(table_name, &row_key);
                let row_json = serde_json::to_vec(&row)
                    .map_err(|e| GlueError::StorageMsg(format!("append_data serialize: {e}")))?;
                data_table
                    .insert(compound.as_slice(), row_json.as_slice())
                    .map_err(redb_err)?;
            }
            // Persist the updated counter.
            let ck = counter_key(table_name);
            data_table
                .insert(ck.as_slice(), counter.to_be_bytes().as_slice())
                .map_err(redb_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }
}

// ── Store (SELECT) ────────────────────────────────────────────────────────────

#[async_trait]
impl Store for RedbGlueStorage {
    async fn fetch_schema(&self, table_name: &str) -> GlueResult<Option<Schema>> {
        match self.read_schema_bytes(table_name)? {
            None => Ok(None),
            Some(bytes) => {
                let schema: Schema = serde_json::from_slice(&bytes)
                    .map_err(|e| GlueError::StorageMsg(format!("fetch_schema deserialize: {e}")))?;
                Ok(Some(schema))
            }
        }
    }

    async fn fetch_all_schemas(&self) -> GlueResult<Vec<Schema>> {
        let all = self.read_all_schema_bytes()?;
        let mut schemas = Vec::with_capacity(all.len());
        for (_, bytes) in all {
            let schema: Schema = serde_json::from_slice(&bytes).map_err(|e| {
                GlueError::StorageMsg(format!("fetch_all_schemas deserialize: {e}"))
            })?;
            schemas.push(schema);
        }
        schemas.sort_by(|a, b| a.table_name.cmp(&b.table_name));
        Ok(schemas)
    }

    async fn fetch_data(&self, table_name: &str, key: &Key) -> GlueResult<Option<DataRow>> {
        match self.read_row(table_name, key)? {
            None => Ok(None),
            Some(bytes) => {
                let row: DataRow = serde_json::from_slice(&bytes)
                    .map_err(|e| GlueError::StorageMsg(format!("fetch_data deserialize: {e}")))?;
                Ok(Some(row))
            }
        }
    }

    async fn scan_data<'a>(&'a self, table_name: &str) -> GlueResult<RowIter<'a>> {
        let raw_rows = self.scan_rows(table_name)?;
        let mut rows = Vec::with_capacity(raw_rows.len());
        for (key, bytes) in raw_rows {
            let row: DataRow = serde_json::from_slice(&bytes)
                .map_err(|e| GlueError::StorageMsg(format!("scan_data deserialize: {e}")))?;
            rows.push(Ok((key, row)));
        }
        Ok(Box::pin(iter(rows)))
    }
}

// ── StoreMut (INSERT/UPDATE/DELETE/CREATE TABLE/DROP TABLE) ───────────────────

#[async_trait]
impl StoreMut for RedbGlueStorage {
    async fn insert_schema(&mut self, schema: &Schema) -> GlueResult<()> {
        let bytes = serde_json::to_vec(schema)
            .map_err(|e| GlueError::StorageMsg(format!("insert_schema serialize: {e}")))?;
        self.write_schema(&schema.table_name, &bytes)
    }

    async fn delete_schema(&mut self, table_name: &str) -> GlueResult<()> {
        self.delete_schema_and_data(table_name)
    }

    async fn append_data(&mut self, table_name: &str, rows: Vec<DataRow>) -> GlueResult<()> {
        self.append_rows_to_table(table_name, rows)
    }

    async fn insert_data(&mut self, table_name: &str, rows: Vec<(Key, DataRow)>) -> GlueResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut encoded: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(rows.len());
        for (key, row) in &rows {
            let compound = data_key(table_name, key);
            let row_json = serde_json::to_vec(row)
                .map_err(|e| GlueError::StorageMsg(format!("insert_data serialize: {e}")))?;
            encoded.push((compound, row_json));
        }
        self.write_rows(&encoded)
    }

    async fn delete_data(&mut self, table_name: &str, keys: Vec<Key>) -> GlueResult<()> {
        if keys.is_empty() {
            return Ok(());
        }
        let compound_keys: Vec<Vec<u8>> = keys.iter().map(|k| data_key(table_name, k)).collect();
        self.delete_rows(&compound_keys)
    }
}

// ── Auxiliary GlueSQL traits (no-op or default implementations) ───────────────

#[async_trait]
impl CustomFunction for RedbGlueStorage {
    async fn fetch_function<'a>(
        &'a self,
        _func_name: &str,
    ) -> GlueResult<Option<&'a GlueCustomFunction>> {
        // Cannot return a reference into a deserialized-on-the-fly value.
        // Return None so GlueSQL uses its built-in function registry.
        Ok(None)
    }

    async fn fetch_all_functions<'a>(&'a self) -> GlueResult<Vec<&'a GlueCustomFunction>> {
        Ok(vec![])
    }
}

#[async_trait]
impl CustomFunctionMut for RedbGlueStorage {
    async fn insert_function(&mut self, func: GlueCustomFunction) -> GlueResult<()> {
        let bytes = serde_json::to_vec(&func)
            .map_err(|e| GlueError::StorageMsg(format!("insert_function serialize: {e}")))?;
        let name_upper = func.func_name.to_uppercase();
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(FUNC_TABLE).map_err(map_table_err)?;
            table
                .insert(name_upper.as_bytes(), bytes.as_slice())
                .map_err(redb_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }

    async fn delete_function(&mut self, func_name: &str) -> GlueResult<()> {
        let name_upper = func_name.to_uppercase();
        let db = self.lock_db()?;
        let txn = db.begin_write().map_err(map_txn_err)?;
        {
            let mut table = txn.open_table(FUNC_TABLE).map_err(map_table_err)?;
            table.remove(name_upper.as_bytes()).map_err(redb_err)?;
        }
        txn.commit().map_err(map_commit_err)?;
        Ok(())
    }
}

#[async_trait]
impl AlterTable for RedbGlueStorage {}

#[async_trait]
impl Index for RedbGlueStorage {}

#[async_trait]
impl IndexMut for RedbGlueStorage {}

#[async_trait]
impl Metadata for RedbGlueStorage {
    async fn scan_table_meta(&self) -> GlueResult<MetaIter> {
        let all = self.read_all_schema_bytes()?;
        let meta: Vec<(String, BTreeMap<String, Value>)> = all
            .into_iter()
            .map(|(name, _)| {
                let created = BTreeMap::from([(
                    "CREATED".to_owned(),
                    Value::Timestamp(Utc::now().naive_utc()),
                )]);
                (name, created)
            })
            .collect();
        Ok(Box::new(meta.into_iter().map(Ok)))
    }
}

#[async_trait]
impl Transaction for RedbGlueStorage {
    async fn begin(&mut self, autocommit: bool) -> GlueResult<bool> {
        if autocommit {
            return Ok(false);
        }
        Err(GlueError::StorageMsg(
            "[RedbGlueStorage] nested transactions are not supported".to_owned(),
        ))
    }

    async fn rollback(&mut self) -> GlueResult<()> {
        Ok(())
    }

    async fn commit(&mut self) -> GlueResult<()> {
        Ok(())
    }
}

impl Planner for RedbGlueStorage {}
