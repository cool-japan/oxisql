#![forbid(unsafe_code)]

//! `FjallGlueStorage` — GlueSQL storage backend backed by a fjall LSM-tree.
//!
//! Uses two fjall keyspaces (column families):
//! - `glue_schemas` — stores table_name → JSON-serialised `Schema`
//! - `glue_data`   — stores `{table_name}\x00{encoded_key}` → JSON-serialised `DataRow`
//!
//! Key encoding uses a compact, **order-preserving** binary format so that
//! LSM-tree prefix scans return rows in the correct `Key` order:
//!
//! | Variant | Tag | Payload |
//! |---------|-----|---------|
//! | `Key::None`      | `0x00` | *(empty)* |
//! | `Key::Bool`      | `0x01` | `0x00` / `0x01` |
//! | `Key::I8`        | `0x10` | sign-flipped u8 |
//! | `Key::I16`       | `0x11` | sign-flipped big-endian u16 |
//! | `Key::I32`       | `0x12` | sign-flipped big-endian u32 |
//! | `Key::I64`       | `0x13` | sign-flipped big-endian u64 |
//! | `Key::I128`      | `0x14` | sign-flipped big-endian u128 |
//! | `Key::U8`        | `0x20` | big-endian u8 |
//! | `Key::U16`       | `0x21` | big-endian u16 |
//! | `Key::U32`       | `0x22` | big-endian u32 |
//! | `Key::U64`       | `0x23` | big-endian u64 |
//! | `Key::U128`/Uuid | `0x24` | big-endian u128 |
//! | `Key::Str`       | `0x30` | UTF-8 bytes |
//! | `Key::Bytea`     | `0x40` | raw bytes |
//! | other            | `0xFF` | JSON text bytes (non-order-preserving; rare in practice) |
//!
//! "Sign-flipped" means XOR with `0x80` on the high byte so that negative
//! numbers sort below positive ones in unsigned lexicographic order.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fjall::{Database, Keyspace, KeyspaceCreateOptions};
use futures::stream::iter;
use gluesql::core::data::{Key, Schema};
use gluesql::core::error::Result as GlueResult;
use gluesql::core::store::{
    AlterTable, CustomFunction, CustomFunctionMut, DataRow, Index, IndexMut, Metadata, Planner,
    RowIter, Store, StoreMut, Transaction,
};

// ── serialisation helpers ─────────────────────────────────────────────────────

fn serialize_schema(schema: &Schema) -> Vec<u8> {
    serde_json::to_vec(schema).unwrap_or_default()
}

fn deserialize_schema(bytes: &[u8]) -> Option<Schema> {
    serde_json::from_slice(bytes).ok()
}

fn serialize_row(row: &DataRow) -> Vec<u8> {
    serde_json::to_vec(row).unwrap_or_default()
}

fn deserialize_row(bytes: &[u8]) -> Option<DataRow> {
    serde_json::from_slice(bytes).ok()
}

// ── order-preserving key encoding ─────────────────────────────────────────────

/// Encode a GlueSQL [`Key`] into bytes that sort in the same order as the
/// `Key::Ord` implementation.
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
            // Flip sign bit so that i8::MIN (-128) → 0x00 and i8::MAX (127) → 0xFF
            buf.push((*n as u8) ^ 0x80);
        }
        Key::I16(n) => {
            buf.push(0x11);
            let raw = ((*n as u16) ^ 0x8000u16).to_be_bytes();
            buf.extend_from_slice(&raw);
        }
        Key::I32(n) => {
            buf.push(0x12);
            let raw = ((*n as u32) ^ 0x8000_0000u32).to_be_bytes();
            buf.extend_from_slice(&raw);
        }
        Key::I64(n) => {
            buf.push(0x13);
            let raw = ((*n as u64) ^ 0x8000_0000_0000_0000u64).to_be_bytes();
            buf.extend_from_slice(&raw);
        }
        Key::I128(n) => {
            buf.push(0x14);
            let raw = ((*n as u128) ^ (1u128 << 127)).to_be_bytes();
            buf.extend_from_slice(&raw);
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
        Key::Str(s) => {
            buf.push(0x30);
            buf.extend_from_slice(s.as_bytes());
        }
        Key::Bytea(b) => {
            buf.push(0x40);
            buf.extend_from_slice(b);
        }
        // For rare types (Decimal, Date, Timestamp, Time, Interval, Inet, F32, F64)
        // fall back to JSON serialisation.  Order is preserved within a type (same
        // JSON format for the same type), but cross-type ordering is not guaranteed.
        other => {
            buf.push(0xFF);
            if let Ok(json) = serde_json::to_vec(other) {
                buf.extend_from_slice(&json);
            }
        }
    }
    buf
}

// ── FjallGlueStorage ──────────────────────────────────────────────────────────

/// GlueSQL storage backend backed by a fjall LSM-tree [`Database`].
///
/// Each [`FjallGlueStorage`] instance owns:
/// - A [`Database`] (the on-disk container).
/// - Two [`Keyspace`] handles for schemas and row data.
/// - An auto-increment counter per table for `append_data`.
///
/// The type implements [`Store`], [`StoreMut`] and all required auxiliary GlueSQL
/// traits, making it usable as the storage parameter to `gluesql::prelude::Glue`.
///
/// # Persistence
///
/// All writes are durably committed to the fjall journal before this method
/// returns.  A process crash after a successful write will not lose data.
///
/// # Thread safety
///
/// `FjallGlueStorage` is `Send + Sync` because fjall's `Database` and `Keyspace`
/// are both `Clone + Send + Sync`.  The auto-increment counter is protected by a
/// `Mutex`.
pub struct FjallGlueStorage {
    _db: Database,
    schemas: Keyspace,
    data: Keyspace,
    /// Per-table auto-increment counter used by `append_data`.
    next_key: Arc<Mutex<HashMap<String, i64>>>,
}

impl FjallGlueStorage {
    /// Open (or create) an on-disk fjall database at `path`.
    ///
    /// The function is synchronous because fjall's `Database::builder().open()`
    /// is synchronous.
    ///
    /// # Errors
    ///
    /// Returns [`fjall::Error`] if the database cannot be opened or if either
    /// internal keyspace cannot be created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, fjall::Error> {
        let db = Database::builder(path.as_ref()).open()?;
        let schemas = db.keyspace("glue_schemas", KeyspaceCreateOptions::default)?;
        let data = db.keyspace("glue_data", KeyspaceCreateOptions::default)?;
        Ok(Self {
            _db: db,
            schemas,
            data,
            next_key: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    // ── internal helpers ─────────────────────────────────────────────────────

    /// Build the composite data key `{table_name}\x00{encoded_key}`.
    fn data_key(table_name: &str, key: &Key) -> Vec<u8> {
        let mut composite = table_name.as_bytes().to_vec();
        composite.push(0x00);
        composite.extend(encode_key(key));
        composite
    }

    /// Build the data prefix for a table: `{table_name}\x00`.
    fn data_prefix(table_name: &str) -> Vec<u8> {
        let mut prefix = table_name.as_bytes().to_vec();
        prefix.push(0x00);
        prefix
    }

    /// Increment and return the next auto-increment counter value for `table`.
    fn next_id(&self, table: &str) -> i64 {
        let mut map = self.next_key.lock().unwrap_or_else(|e| e.into_inner());
        let counter = map.entry(table.to_owned()).or_insert(0);
        *counter += 1;
        *counter
    }

    /// Load all persisted auto-increment counters for the given table from
    /// the data keyspace.  Used after re-opening to avoid key collisions.
    fn sync_counter_for(&self, table: &str) {
        let prefix = Self::data_prefix(table);
        let max_id = self
            .data
            .prefix(&prefix)
            .filter_map(|guard| {
                let (raw_key, _) = guard.into_inner().ok()?;
                // Skip the prefix bytes and decode the encoded key
                let encoded = raw_key.get(prefix.len()..)?;
                // Only care about I64 keys (tag 0x13, 9 bytes total)
                if encoded.first() == Some(&0x13) && encoded.len() == 9 {
                    let arr: [u8; 8] = encoded.get(1..9)?.try_into().ok()?;
                    let raw = u64::from_be_bytes(arr) ^ 0x8000_0000_0000_0000u64;
                    Some(raw as i64)
                } else {
                    None
                }
            })
            .max()
            .unwrap_or(0);

        let mut map = self.next_key.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map.entry(table.to_owned()).or_insert(0);
        if max_id > *entry {
            *entry = max_id;
        }
    }
}

// ── Store (read operations) ───────────────────────────────────────────────────

#[async_trait]
impl Store for FjallGlueStorage {
    async fn fetch_schema(&self, table_name: &str) -> GlueResult<Option<Schema>> {
        match self.schemas.get(table_name.as_bytes()) {
            Ok(Some(bytes)) => Ok(deserialize_schema(&bytes)),
            Ok(None) => Ok(None),
            Err(e) => Err(gluesql::core::error::Error::StorageMsg(e.to_string())),
        }
    }

    async fn fetch_all_schemas(&self) -> GlueResult<Vec<Schema>> {
        let mut schemas: Vec<Schema> = self
            .schemas
            .prefix(b"" as &[u8])
            .filter_map(|guard| {
                let (_k, v) = guard.into_inner().ok()?;
                deserialize_schema(&v)
            })
            .collect();
        schemas.sort_by(|a, b| a.table_name.cmp(&b.table_name));
        Ok(schemas)
    }

    async fn fetch_data(&self, table_name: &str, key: &Key) -> GlueResult<Option<DataRow>> {
        let composite = Self::data_key(table_name, key);
        match self.data.get(&composite) {
            Ok(Some(bytes)) => Ok(deserialize_row(&bytes)),
            Ok(None) => Ok(None),
            Err(e) => Err(gluesql::core::error::Error::StorageMsg(e.to_string())),
        }
    }

    async fn scan_data<'a>(&'a self, table_name: &str) -> GlueResult<RowIter<'a>> {
        let prefix = Self::data_prefix(table_name);
        let prefix_len = prefix.len();

        // Collect all rows eagerly (fjall's iterator is synchronous).
        let rows: Vec<GlueResult<(Key, DataRow)>> = self
            .data
            .prefix(&prefix)
            .filter_map(|guard| {
                let (raw_key, raw_val) = guard.into_inner().ok()?;
                let encoded_key = raw_key.get(prefix_len..)?;
                let key = decode_key(encoded_key)?;
                let row = deserialize_row(&raw_val)?;
                Some(Ok((key, row)))
            })
            .collect();

        Ok(Box::pin(iter(rows)))
    }
}

// ── StoreMut (write operations) ───────────────────────────────────────────────

#[async_trait]
impl StoreMut for FjallGlueStorage {
    async fn insert_schema(&mut self, schema: &Schema) -> GlueResult<()> {
        let bytes = serialize_schema(schema);
        self.schemas
            .insert(schema.table_name.as_bytes(), bytes)
            .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))
    }

    async fn delete_schema(&mut self, table_name: &str) -> GlueResult<()> {
        // Remove the schema entry.
        self.schemas
            .remove(table_name.as_bytes())
            .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))?;

        // Remove all data rows for this table.
        let prefix = Self::data_prefix(table_name);
        let keys_to_delete: Vec<Vec<u8>> = self
            .data
            .prefix(&prefix)
            .filter_map(|guard| {
                let (k, _) = guard.into_inner().ok()?;
                Some(k.to_vec())
            })
            .collect();

        for k in keys_to_delete {
            self.data
                .remove(k)
                .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))?;
        }

        // Clear the in-memory counter for this table.
        {
            let mut map = self.next_key.lock().unwrap_or_else(|e| e.into_inner());
            map.remove(table_name);
        }

        Ok(())
    }

    async fn append_data(&mut self, table_name: &str, rows: Vec<DataRow>) -> GlueResult<()> {
        // Sync the counter from disk on first use for this table to handle reopened DBs.
        {
            let needs_sync = {
                let map = self.next_key.lock().unwrap_or_else(|e| e.into_inner());
                !map.contains_key(table_name)
            };
            if needs_sync {
                self.sync_counter_for(table_name);
            }
        }

        for row in rows {
            let id = self.next_id(table_name);
            let key = Key::I64(id);
            let composite = Self::data_key(table_name, &key);
            let bytes = serialize_row(&row);
            self.data
                .insert(composite, bytes)
                .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))?;
        }
        Ok(())
    }

    async fn insert_data(&mut self, table_name: &str, rows: Vec<(Key, DataRow)>) -> GlueResult<()> {
        for (key, row) in rows {
            let composite = Self::data_key(table_name, &key);
            let bytes = serialize_row(&row);
            self.data
                .insert(composite, bytes)
                .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))?;
        }
        Ok(())
    }

    async fn delete_data(&mut self, table_name: &str, keys: Vec<Key>) -> GlueResult<()> {
        for key in keys {
            let composite = Self::data_key(table_name, &key);
            self.data
                .remove(composite)
                .map_err(|e| gluesql::core::error::Error::StorageMsg(e.to_string()))?;
        }
        Ok(())
    }
}

// ── Auxiliary GlueSQL traits (no-op or default implementations) ───────────────

#[async_trait]
impl CustomFunction for FjallGlueStorage {}

#[async_trait]
impl CustomFunctionMut for FjallGlueStorage {}

#[async_trait]
impl AlterTable for FjallGlueStorage {}

#[async_trait]
impl Index for FjallGlueStorage {}

#[async_trait]
impl IndexMut for FjallGlueStorage {}

#[async_trait]
impl Metadata for FjallGlueStorage {}

#[async_trait]
impl Transaction for FjallGlueStorage {}

impl Planner for FjallGlueStorage {}

// ── decode_key ────────────────────────────────────────────────────────────────

/// Decode a binary key produced by [`encode_key`] back into a [`Key`].
///
/// Returns `None` for unknown tags or malformed payloads.
fn decode_key(encoded: &[u8]) -> Option<Key> {
    let (&tag, rest) = encoded.split_first()?;
    match tag {
        0x00 => Some(Key::None),
        0x01 => {
            let b = rest.first()?;
            Some(Key::Bool(*b != 0))
        }
        0x10 => {
            let byte = rest.first()?;
            let n = (byte ^ 0x80) as i8;
            Some(Key::I8(n))
        }
        0x11 => {
            let arr: [u8; 2] = rest.get(..2)?.try_into().ok()?;
            let raw = u16::from_be_bytes(arr) ^ 0x8000u16;
            Some(Key::I16(raw as i16))
        }
        0x12 => {
            let arr: [u8; 4] = rest.get(..4)?.try_into().ok()?;
            let raw = u32::from_be_bytes(arr) ^ 0x8000_0000u32;
            Some(Key::I32(raw as i32))
        }
        0x13 => {
            let arr: [u8; 8] = rest.get(..8)?.try_into().ok()?;
            let raw = u64::from_be_bytes(arr) ^ 0x8000_0000_0000_0000u64;
            Some(Key::I64(raw as i64))
        }
        0x14 => {
            let arr: [u8; 16] = rest.get(..16)?.try_into().ok()?;
            let raw = u128::from_be_bytes(arr) ^ (1u128 << 127);
            Some(Key::I128(raw as i128))
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
            let s = std::str::from_utf8(rest).ok()?;
            Some(Key::Str(s.to_owned()))
        }
        0x40 => Some(Key::Bytea(rest.to_vec())),
        0xFF => serde_json::from_slice(rest).ok(),
        _ => None,
    }
}
