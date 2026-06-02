//! `SledGlueStorage` — GlueSQL storage backend backed by a sled embedded key-value store.
//!
//! This module is compiled only when the `sled-storage` Cargo feature is enabled.
//!
//! Mirrors the `redb_storage.rs` design: uses **serde_json** (not `bincode`) for
//! all serialisation so the COOLJAPAN no-bincode policy is respected.
//!
//! # Storage layout
//!
//! Everything lives in a single sled `Db`.  Keys are namespaced by prefix:
//!
//! | Prefix          | Value                            |
//! |-----------------|----------------------------------|
//! | `schema/`       | JSON-serialised `Schema`         |
//! | `data/{table}\x00{encoded_key}` | JSON-serialised `DataRow` |
//! | `counter/{table}` | 8-byte big-endian i64 auto-increment counter |
//! | `func/`         | JSON-serialised `CustomFunction` |
//!
//! # Key encoding
//!
//! Row keys are encoded with the same order-preserving binary format used by
//! `redb_storage.rs`.  See that module for the full tag-table documentation.

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
use sled::Db;

// ── order-preserving key encoding (identical to redb_storage.rs) ─────────────

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

// ── key helpers ───────────────────────────────────────────────────────────────

/// Sled key for a schema entry: `"schema/{table_name}"`.
fn schema_key(table_name: &str) -> Vec<u8> {
    format!("schema/{table_name}").into_bytes()
}

/// Sled key prefix for all data rows of a table: `"data/{table_name}\x00"`.
fn data_prefix(table_name: &str) -> Vec<u8> {
    let mut k = format!("data/{table_name}").into_bytes();
    k.push(0x00);
    k
}

/// Full sled key for a specific data row: `"data/{table_name}\x00{encoded_key}"`.
fn data_key(table_name: &str, row_key: &Key) -> Vec<u8> {
    let mut k = data_prefix(table_name);
    k.extend_from_slice(&encode_key(row_key));
    k
}

/// Sled key for an auto-increment counter: `"counter/{table_name}"`.
fn counter_key(table_name: &str) -> Vec<u8> {
    format!("counter/{table_name}").into_bytes()
}

/// Sled key for a custom function: `"func/{UPPER(name)}"`.
fn func_key(func_name: &str) -> Vec<u8> {
    format!("func/{}", func_name.to_uppercase()).into_bytes()
}

// ── error helpers ─────────────────────────────────────────────────────────────

fn sled_err(e: impl std::fmt::Display) -> GlueError {
    GlueError::StorageMsg(format!("sled: {e}"))
}

// ── SledGlueStorage ───────────────────────────────────────────────────────────

/// A persistent GlueSQL storage backend backed by a [`sled`] key-value store.
///
/// `SledGlueStorage` implements the full GlueSQL `Store + StoreMut` surface
/// required by `gluesql::prelude::Glue`.
///
/// All serialisation uses **serde_json** (not `bincode`) to comply with the
/// COOLJAPAN no-bincode policy.
///
/// # Persistence
///
/// Each DDL/DML operation flushes to sled's log-structured storage.  Data
/// written through this connection survives process restarts as long as the
/// OS page-cache flush completes (sled calls `flush` after writes).
///
/// # Transaction semantics
///
/// Only autocommit mode is supported.  Attempting a non-autocommit
/// `Transaction::begin` returns an error, matching the behaviour of
/// `RedbGlueStorage` and `FjallGlueStorage`.
pub struct SledGlueStorage {
    db: Arc<Mutex<Db>>,
}

impl SledGlueStorage {
    /// Open or create a sled database at `path`.
    ///
    /// The directory is created if it does not exist.  On success all
    /// previously written data is immediately visible.
    ///
    /// # Errors
    ///
    /// Propagates any [`sled::Error`] (I/O errors, corrupt database, etc.)
    /// wrapped as [`GlueError::StorageMsg`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self, sled::Error> {
        let db = sled::open(path)?;
        Ok(Self {
            db: Arc::new(Mutex::new(db)),
        })
    }

    fn lock_db(&self) -> GlueResult<std::sync::MutexGuard<'_, Db>> {
        self.db
            .lock()
            .map_err(|e| GlueError::StorageMsg(format!("sled lock poisoned: {e}")))
    }

    // ── Schema helpers ────────────────────────────────────────────────────────

    fn read_schema_bytes(&self, table_name: &str) -> GlueResult<Option<Vec<u8>>> {
        let db = self.lock_db()?;
        let key = schema_key(table_name);
        db.get(&key)
            .map_err(sled_err)?
            .map(|iv| Ok(iv.to_vec()))
            .transpose()
    }

    fn read_all_schema_bytes(&self) -> GlueResult<Vec<(String, Vec<u8>)>> {
        let db = self.lock_db()?;
        let prefix = b"schema/";
        let mut result = Vec::new();
        for item in db.scan_prefix(prefix) {
            let (k, v) = item.map_err(sled_err)?;
            let full_key = std::str::from_utf8(&k)
                .map_err(|e| GlueError::StorageMsg(format!("sled schema key utf8: {e}")))?;
            let table_name = full_key
                .strip_prefix("schema/")
                .unwrap_or(full_key)
                .to_owned();
            result.push((table_name, v.to_vec()));
        }
        Ok(result)
    }

    fn write_schema(&self, table_name: &str, bytes: &[u8]) -> GlueResult<()> {
        let db = self.lock_db()?;
        let key = schema_key(table_name);
        db.insert(key, bytes).map_err(sled_err)?;
        db.flush().map_err(sled_err)?;
        Ok(())
    }

    fn delete_schema_and_data(&self, table_name: &str) -> GlueResult<()> {
        let db = self.lock_db()?;
        // Delete schema entry.
        db.remove(schema_key(table_name)).map_err(sled_err)?;
        // Delete all data rows for the table.
        let prefix = data_prefix(table_name);
        let keys_to_delete: Vec<Vec<u8>> = db
            .scan_prefix(&prefix)
            .map(|item| item.map(|(k, _)| k.to_vec()).map_err(sled_err))
            .collect::<GlueResult<Vec<_>>>()?;
        for k in &keys_to_delete {
            db.remove(k).map_err(sled_err)?;
        }
        // Delete auto-increment counter.
        db.remove(counter_key(table_name)).map_err(sled_err)?;
        db.flush().map_err(sled_err)?;
        Ok(())
    }

    // ── Data helpers ──────────────────────────────────────────────────────────

    fn read_row(&self, table_name: &str, key: &Key) -> GlueResult<Option<Vec<u8>>> {
        let db = self.lock_db()?;
        let compound = data_key(table_name, key);
        db.get(&compound)
            .map_err(sled_err)?
            .map(|iv| Ok(iv.to_vec()))
            .transpose()
    }

    fn scan_rows(&self, table_name: &str) -> GlueResult<Vec<(Key, Vec<u8>)>> {
        let db = self.lock_db()?;
        let prefix = data_prefix(table_name);
        let mut result = Vec::new();
        for item in db.scan_prefix(&prefix) {
            let (k, v) = item.map_err(sled_err)?;
            let row_key_bytes = &k[prefix.len()..];
            let glue_key = decode_key(row_key_bytes).ok_or_else(|| {
                GlueError::StorageMsg(format!(
                    "scan_rows: cannot decode key bytes {row_key_bytes:?}"
                ))
            })?;
            result.push((glue_key, v.to_vec()));
        }
        Ok(result)
    }

    fn write_rows(&self, rows: &[(Vec<u8>, Vec<u8>)]) -> GlueResult<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let db = self.lock_db()?;
        for (compound_key, value_bytes) in rows {
            db.insert(compound_key.as_slice(), value_bytes.as_slice())
                .map_err(sled_err)?;
        }
        db.flush().map_err(sled_err)?;
        Ok(())
    }

    fn delete_rows(&self, compound_keys: &[Vec<u8>]) -> GlueResult<()> {
        if compound_keys.is_empty() {
            return Ok(());
        }
        let db = self.lock_db()?;
        for key in compound_keys {
            db.remove(key.as_slice()).map_err(sled_err)?;
        }
        db.flush().map_err(sled_err)?;
        Ok(())
    }

    // ── Auto-increment helpers ────────────────────────────────────────────────

    fn read_counter(&self, table_name: &str) -> GlueResult<i64> {
        let db = self.lock_db()?;
        let ck = counter_key(table_name);
        match db.get(&ck).map_err(sled_err)? {
            None => Ok(0),
            Some(iv) => {
                let bytes: [u8; 8] = iv
                    .as_ref()
                    .try_into()
                    .map_err(|_| sled_err("counter value corrupted"))?;
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
        for row in rows {
            counter += 1;
            let row_key = Key::I64(counter);
            let compound = data_key(table_name, &row_key);
            let row_json = serde_json::to_vec(&row)
                .map_err(|e| GlueError::StorageMsg(format!("append_data serialize: {e}")))?;
            db.insert(compound, row_json).map_err(sled_err)?;
        }
        let ck = counter_key(table_name);
        db.insert(ck, &counter.to_be_bytes()).map_err(sled_err)?;
        db.flush().map_err(sled_err)?;
        Ok(())
    }
}

// ── Store (SELECT) ────────────────────────────────────────────────────────────

#[async_trait]
impl Store for SledGlueStorage {
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
impl StoreMut for SledGlueStorage {
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
impl CustomFunction for SledGlueStorage {
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
impl CustomFunctionMut for SledGlueStorage {
    async fn insert_function(&mut self, func: GlueCustomFunction) -> GlueResult<()> {
        let bytes = serde_json::to_vec(&func)
            .map_err(|e| GlueError::StorageMsg(format!("insert_function serialize: {e}")))?;
        let key = func_key(&func.func_name);
        let db = self.lock_db()?;
        db.insert(key, bytes).map_err(sled_err)?;
        db.flush().map_err(sled_err)?;
        Ok(())
    }

    async fn delete_function(&mut self, func_name: &str) -> GlueResult<()> {
        let key = func_key(func_name);
        let db = self.lock_db()?;
        db.remove(key).map_err(sled_err)?;
        db.flush().map_err(sled_err)?;
        Ok(())
    }
}

#[async_trait]
impl AlterTable for SledGlueStorage {}

#[async_trait]
impl Index for SledGlueStorage {}

#[async_trait]
impl IndexMut for SledGlueStorage {}

#[async_trait]
impl Metadata for SledGlueStorage {
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
impl Transaction for SledGlueStorage {
    async fn begin(&mut self, autocommit: bool) -> GlueResult<bool> {
        if autocommit {
            return Ok(false);
        }
        Err(GlueError::StorageMsg(
            "[SledGlueStorage] nested transactions are not supported".to_owned(),
        ))
    }

    async fn rollback(&mut self) -> GlueResult<()> {
        Ok(())
    }

    async fn commit(&mut self) -> GlueResult<()> {
        Ok(())
    }
}

impl Planner for SledGlueStorage {}
