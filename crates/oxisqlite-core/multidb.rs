//! Per-connection multi-database registry (`main`, `temp` and `ATTACH`ed files).
//!
//! Upstream SQLite gives every connection an array of database back-ends
//! (`sqlite3.aDb[]`): slot 0 is always `main`, slot 1 is always `temp`, and
//! slots 2.. hold the databases opened by `ATTACH DATABASE`. Each slot owns its
//! own B-tree/pager and its own schema (catalog) namespace. This module is the
//! same idea, adapted to this engine:
//!
//! * slot 0 (`main`) is *not* stored here — it is the connection's own
//!   [`crate::Connection::pager`] / [`crate::Connection::schema`] /
//!   [`crate::Connection::transaction_state`], which are shared with the owning
//!   [`crate::Database`] exactly as before. Nothing about single-database
//!   operation changes.
//! * slot 1 (`temp`) is created lazily, the first time a statement actually
//!   needs it, and is backed by a private in-memory pager. It is per-connection
//!   (invisible to every other connection) and dropped when the connection is
//!   closed, which is precisely `sqlite_temp_schema` semantics.
//! * slots 2.. are `ATTACH`ed databases, each a full nested [`crate::Database`]
//!   (so WAL setup, header bootstrap and schema parsing all reuse the same
//!   tested code paths as a top-level open).
//!
//! Every auxiliary slot carries its own [`TransactionState`], because a
//! statement may touch `main` and an attached database in the same step and the
//! two pagers have completely independent read/write transactions.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use parking_lot::RwLock;

use crate::io::{MemoryIO, OpenFlags};
use crate::result::LimboResult;
use crate::schema::{Schema, Table};
use crate::storage::database::{DatabaseStorage, FileMemoryStorage};
use crate::storage::pager::Pager;
use crate::util::normalize_ident;
use crate::{
    maybe_init_database_file, Connection, Database, LimboError, PagerCacheflushStatus, Result,
    TransactionState, IO,
};

/// Registry index of the `main` database. Always the connection's own pager.
pub(crate) const DB_MAIN: usize = 0;
/// Registry index of the `temp` database, mirroring `sqlite3.aDb[1]`.
pub(crate) const DB_TEMP: usize = 1;
/// Upstream's default `SQLITE_MAX_ATTACHED`.
pub(crate) const MAX_ATTACHED: usize = 10;

/// The canonical name of the `main` database.
pub(crate) const MAIN_DB_NAME: &str = "main";
/// The canonical name of the `temp` database.
pub(crate) const TEMP_DB_NAME: &str = "temp";
/// SQLite also accepts `sqlite_temp_master`-style spelling `temp` only, but the
/// schema name `TEMP` is case-insensitive, which `normalize_ident` handles.
pub(crate) const TEMP_DB_NAME_ALT: &str = "sqlite_temp";
/// Normalized key of a catalog's schema table.
const SCHEMA_TABLE_NAME: &str = "sqlite_schema";
/// Upstream's name for the `temp` database's schema table.
const TEMP_SCHEMA_TABLE_NAME: &str = "sqlite_temp_schema";

/// What kind of auxiliary database a registry slot holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum AuxDbKind {
    /// The lazily-created `temp` database (slot [`DB_TEMP`]).
    Temp,
    /// A database opened by `ATTACH DATABASE`.
    Attached,
}

/// One auxiliary (non-`main`) database attached to a connection.
pub(crate) struct AuxDb {
    /// Normalized schema name used to qualify object references
    /// (`temp` or the `ATTACH ... AS <alias>` alias).
    pub(crate) name: String,
    pub(crate) kind: AuxDbKind,
    /// Keeps the nested database alive for as long as the slot exists.
    _db: Arc<Database>,
    /// Keeps the nested connection (and therefore its pager) alive. Dropping it
    /// performs the usual best-effort checkpoint/shutdown.
    _conn: Arc<Connection>,
    pub(crate) pager: Rc<Pager>,
    pub(crate) schema: Arc<RwLock<Schema>>,
    pub(crate) txn_state: Cell<TransactionState>,
}

/// Outcome of ending a transaction on one auxiliary database.
pub(crate) enum AuxTxnEnd {
    /// The transaction is fully committed/closed.
    Done,
    /// The pager needs more I/O; call again after the I/O completes.
    Io,
}

/// Stamp every B-tree table in `schema` with the registry index `db`.
///
/// A nested [`Database`] parses its schema believing it is `main` (index 0), so
/// after opening — and after every re-parse triggered by DDL on that database —
/// its tables must be re-tagged with the registry slot they actually live in.
/// Code generation reads this tag to decide which pager a cursor opens against.
pub(crate) fn retag_schema_db_index(schema: &mut Schema, db: usize) {
    #[allow(clippy::arc_with_non_send_sync)]
    let retagged: Vec<(String, Arc<Table>)> = schema
        .tables
        .iter()
        .filter_map(|(name, table)| match table.as_ref() {
            Table::BTree(btree) if btree.db_index != db => {
                let mut retagged_btree = (**btree).clone();
                retagged_btree.db_index = db;
                Some((
                    name.clone(),
                    Arc::new(Table::BTree(Rc::new(retagged_btree))),
                ))
            }
            _ => None,
        })
        .collect();
    for (name, table) in retagged {
        schema.tables.insert(name, table);
    }
}

/// A private, registry-tagged copy of a nested database's catalog.
///
/// The nested [`Database`] keeps its own untagged catalog (every table at
/// `db_index == 0`), because *it* compiles the `SELECT * FROM sqlite_schema`
/// used to reload the catalog and, from its point of view, its own tables really
/// are in `main`. The parent connection gets this separate copy, tagged with the
/// registry slot, so its code generation points cursors at the right pager.
fn retagged_catalog(db: &Arc<Database>, index: usize) -> Result<Arc<RwLock<Schema>>> {
    let mut tagged = db
        .schema
        .try_read()
        .ok_or(LimboError::SchemaLocked)?
        .clone();
    retag_schema_db_index(&mut tagged, index);
    Ok(Arc::new(RwLock::new(tagged)))
}

/// Build the in-memory database that backs `temp`.
fn open_temp_database() -> Result<Arc<Database>> {
    let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
    let file = io.open_file(":memory:", OpenFlags::default(), false)?;
    maybe_init_database_file(&file, &io)?;
    let db_file: Arc<dyn DatabaseStorage> = Arc::new(FileMemoryStorage::new(file));
    Database::open(io, ":memory:", db_file, false)
}

/// Open the database named by an `ATTACH` path.
///
/// `:memory:` (and the empty string, which upstream treats as a private
/// temporary database) get a private in-memory back-end; anything else is a
/// real file opened through the connection's own I/O backend.
///
/// Each attachment gets its own [`Database`], and therefore its own WAL state
/// and page cache. Attaching the *same* file twice (under two aliases, or from
/// two connections of this process) therefore behaves like two separate
/// processes opening it: they coordinate only through the on-disk WAL, not
/// through shared memory. Upstream SQLite shares one `Btree` per file within a
/// connection; matching that would mean a process-wide open-file registry.
fn open_attached_database(io: &Arc<dyn IO>, path: &str) -> Result<Arc<Database>> {
    if path.is_empty() || path.eq_ignore_ascii_case(":memory:") {
        return open_temp_database();
    }
    #[cfg(feature = "fs")]
    {
        Database::open_file(io.clone(), path, false)
    }
    #[cfg(not(feature = "fs"))]
    {
        let _ = io;
        Err(LimboError::InvalidArgument(format!(
            "cannot ATTACH '{path}': this build has no filesystem support (feature \"fs\" is off)"
        )))
    }
}

impl Connection {
    /// True when this connection has at least one auxiliary database, i.e. when
    /// name resolution has to consider more than `main`.
    pub(crate) fn has_aux_dbs(&self) -> bool {
        self.aux_dbs.borrow().iter().any(|slot| slot.is_some())
    }

    /// The pager backing registry slot `db`.
    pub(crate) fn pager_for_db(&self, db: usize) -> Result<Rc<Pager>> {
        if db == DB_MAIN {
            return Ok(self.pager.clone());
        }
        let slots = self.aux_dbs.borrow();
        match slots.get(db).and_then(|slot| slot.as_ref()) {
            Some(aux) => Ok(aux.pager.clone()),
            None => Err(LimboError::InternalError(format!(
                "no such database: index {db}"
            ))),
        }
    }

    /// The catalog backing registry slot `db`.
    pub(crate) fn schema_for_db(&self, db: usize) -> Result<Arc<RwLock<Schema>>> {
        if db == DB_MAIN {
            return Ok(self.schema.clone());
        }
        let slots = self.aux_dbs.borrow();
        match slots.get(db).and_then(|slot| slot.as_ref()) {
            Some(aux) => Ok(aux.schema.clone()),
            None => Err(LimboError::InternalError(format!(
                "no such database: index {db}"
            ))),
        }
    }

    /// Resolve a schema name (`main`, `temp`, or an `ATTACH` alias) to its
    /// registry index. The comparison is case-insensitive, like SQLite's.
    pub(crate) fn db_index_by_name(&self, name: &str) -> Option<usize> {
        let normalized = normalize_ident(name);
        if normalized.eq_ignore_ascii_case(MAIN_DB_NAME) {
            return Some(DB_MAIN);
        }
        if normalized.eq_ignore_ascii_case(TEMP_DB_NAME)
            || normalized.eq_ignore_ascii_case(TEMP_DB_NAME_ALT)
        {
            // `temp` names the temp database even before it exists; callers that
            // need it materialized go through `ensure_temp_db`.
            return Some(DB_TEMP);
        }
        self.aux_dbs
            .borrow()
            .iter()
            .position(|slot| {
                slot.as_ref().is_some_and(|aux| {
                    aux.kind == AuxDbKind::Attached && aux.name.eq_ignore_ascii_case(&normalized)
                })
            })
            .filter(|idx| *idx >= 2)
    }

    /// The schema name of registry slot `db`, if the slot exists.
    pub(crate) fn db_name_for_index(&self, db: usize) -> Option<String> {
        if db == DB_MAIN {
            return Some(MAIN_DB_NAME.to_string());
        }
        if db == DB_TEMP {
            return Some(TEMP_DB_NAME.to_string());
        }
        self.aux_dbs
            .borrow()
            .get(db)
            .and_then(|slot| slot.as_ref())
            .map(|aux| aux.name.clone())
    }

    /// Materialize the `temp` database if it does not exist yet, returning its
    /// registry index ([`DB_TEMP`]).
    pub(crate) fn ensure_temp_db(&self) -> Result<usize> {
        {
            let slots = self.aux_dbs.borrow();
            if slots.get(DB_TEMP).and_then(|slot| slot.as_ref()).is_some() {
                return Ok(DB_TEMP);
            }
        }
        let db = open_temp_database()?;
        let conn = db.connect()?;
        let pager = conn.pager.clone();
        let schema = retagged_catalog(&db, DB_TEMP)?;
        let aux = AuxDb {
            name: TEMP_DB_NAME.to_string(),
            kind: AuxDbKind::Temp,
            _db: db,
            _conn: conn,
            pager,
            schema,
            txn_state: Cell::new(TransactionState::None),
        };
        let mut slots = self.aux_dbs.borrow_mut();
        while slots.len() <= DB_TEMP {
            slots.push(None);
        }
        slots[DB_TEMP] = Some(aux);
        Ok(DB_TEMP)
    }

    /// Implement `ATTACH DATABASE <path> AS <alias>`.
    ///
    /// # Errors
    ///
    /// * [`LimboError::InvalidArgument`] when `alias` collides with `main`,
    ///   `temp` or an already-attached alias, or when the attached-database
    ///   limit is reached.
    /// * [`LimboError::TxError`] when a transaction is open (upstream refuses to
    ///   change the set of databases mid-transaction).
    pub(crate) fn attach_db(&self, path: &str, alias: &str) -> Result<usize> {
        let normalized_alias = normalize_ident(alias);
        if normalized_alias.eq_ignore_ascii_case(MAIN_DB_NAME)
            || normalized_alias.eq_ignore_ascii_case(TEMP_DB_NAME)
        {
            return Err(LimboError::InvalidArgument(format!(
                "database {normalized_alias} is already in use"
            )));
        }
        if self.db_index_by_name(&normalized_alias).is_some() {
            return Err(LimboError::InvalidArgument(format!(
                "database {normalized_alias} is already in use"
            )));
        }
        if !self.auto_commit.get() {
            return Err(LimboError::TxError(
                "cannot ATTACH database within transaction".to_string(),
            ));
        }
        let attached_count = self
            .aux_dbs
            .borrow()
            .iter()
            .skip(2)
            .filter(|slot| slot.is_some())
            .count();
        if attached_count >= MAX_ATTACHED {
            return Err(LimboError::InvalidArgument(format!(
                "too many attached databases - max {MAX_ATTACHED}"
            )));
        }
        let db = open_attached_database(&self._db.io, path)?;
        let conn = db.connect()?;
        let pager = conn.pager.clone();
        let mut slots = self.aux_dbs.borrow_mut();
        while slots.len() <= DB_TEMP {
            slots.push(None);
        }
        let index = match slots.iter().skip(2).position(|slot| slot.is_none()) {
            Some(free) => free + 2,
            None => {
                slots.push(None);
                slots.len() - 1
            }
        };
        let schema = retagged_catalog(&db, index)?;
        slots[index] = Some(AuxDb {
            name: normalized_alias,
            kind: AuxDbKind::Attached,
            _db: db,
            _conn: conn,
            pager,
            schema,
            txn_state: Cell::new(TransactionState::None),
        });
        Ok(index)
    }

    /// Implement `DETACH DATABASE <alias>`.
    ///
    /// # Errors
    ///
    /// * [`LimboError::InvalidArgument`] when `alias` is not an attached
    ///   database (`main` and `temp` can never be detached).
    /// * [`LimboError::TxError`] when a transaction is open.
    pub(crate) fn detach_db(&self, alias: &str) -> Result<()> {
        let normalized_alias = normalize_ident(alias);
        if normalized_alias.eq_ignore_ascii_case(MAIN_DB_NAME)
            || normalized_alias.eq_ignore_ascii_case(TEMP_DB_NAME)
        {
            return Err(LimboError::InvalidArgument(format!(
                "cannot detach database {normalized_alias}"
            )));
        }
        let Some(index) = self.db_index_by_name(&normalized_alias) else {
            return Err(LimboError::InvalidArgument(format!(
                "no such database: {normalized_alias}"
            )));
        };
        if !self.auto_commit.get() {
            return Err(LimboError::TxError(
                "cannot DETACH database within transaction".to_string(),
            ));
        }
        let slot = self.aux_dbs.borrow_mut()[index].take();
        if let Some(aux) = slot {
            if !matches!(aux.txn_state.get(), TransactionState::None) {
                return Err(LimboError::TxError(format!(
                    "database {normalized_alias} is locked"
                )));
            }
            // Dropping the slot drops the nested connection, whose `Drop` does a
            // best-effort checkpoint of the attached file.
            drop(aux);
        }
        Ok(())
    }

    /// Ensure a read (and, when `write`, a write) transaction is open on
    /// auxiliary database `db`.
    ///
    /// Cursor-opening and B-tree-creating opcodes call this lazily instead of
    /// the prologue emitting one `Transaction` opcode per database, so a
    /// statement only ever locks the databases it actually touches.
    ///
    /// Returns `true` when the pager reports `BUSY` and the caller must yield.
    pub(crate) fn begin_aux_txn(&self, db: usize, write: bool) -> Result<bool> {
        if db == DB_MAIN {
            return Ok(false);
        }
        let (pager, current) = {
            let slots = self.aux_dbs.borrow();
            let Some(aux) = slots.get(db).and_then(|slot| slot.as_ref()) else {
                return Err(LimboError::InternalError(format!(
                    "no such database: index {db}"
                )));
            };
            (aux.pager.clone(), aux.txn_state.get())
        };
        if matches!(current, TransactionState::None) {
            if let LimboResult::Busy = pager.begin_read_tx()? {
                return Ok(true);
            }
            self.set_aux_txn_state(db, TransactionState::Read)?;
        }
        if write && !matches!(self.aux_txn_state(db)?, TransactionState::Write) {
            if let LimboResult::Busy = pager.begin_write_tx()? {
                return Ok(true);
            }
            self.set_aux_txn_state(db, TransactionState::Write)?;
        }
        Ok(false)
    }

    fn aux_txn_state(&self, db: usize) -> Result<TransactionState> {
        let slots = self.aux_dbs.borrow();
        match slots.get(db).and_then(|slot| slot.as_ref()) {
            Some(aux) => Ok(aux.txn_state.get()),
            None => Err(LimboError::InternalError(format!(
                "no such database: index {db}"
            ))),
        }
    }

    fn set_aux_txn_state(&self, db: usize, state: TransactionState) -> Result<()> {
        let slots = self.aux_dbs.borrow();
        match slots.get(db).and_then(|slot| slot.as_ref()) {
            Some(aux) => {
                aux.txn_state.set(state);
                Ok(())
            }
            None => Err(LimboError::InternalError(format!(
                "no such database: index {db}"
            ))),
        }
    }

    /// Index of the first auxiliary database at or after `from` that has an open
    /// transaction, used to drive the resumable multi-database commit loop.
    pub(crate) fn next_aux_db_with_txn(&self, from: usize) -> Option<usize> {
        let slots = self.aux_dbs.borrow();
        (from.max(1)..slots.len()).find(|idx| {
            slots[*idx]
                .as_ref()
                .is_some_and(|aux| !matches!(aux.txn_state.get(), TransactionState::None))
        })
    }

    /// Drive one step of ending the transaction on auxiliary database `db`.
    ///
    /// Mirrors `Program::step_end_write_txn` for a single auxiliary pager: a
    /// write transaction may need several calls (the cache flush is
    /// asynchronous), a read transaction always finishes immediately.
    pub(crate) fn step_end_aux_txn(&self, db: usize) -> Result<AuxTxnEnd> {
        let (pager, state) = {
            let slots = self.aux_dbs.borrow();
            let Some(aux) = slots.get(db).and_then(|slot| slot.as_ref()) else {
                return Ok(AuxTxnEnd::Done);
            };
            (aux.pager.clone(), aux.txn_state.get())
        };
        match state {
            TransactionState::Write => match pager.end_tx()? {
                PagerCacheflushStatus::Done(_) => {
                    self.set_aux_txn_state(db, TransactionState::None)?;
                    Ok(AuxTxnEnd::Done)
                }
                PagerCacheflushStatus::IO => Ok(AuxTxnEnd::Io),
            },
            TransactionState::Read => {
                pager.end_read_tx()?;
                self.set_aux_txn_state(db, TransactionState::None)?;
                Ok(AuxTxnEnd::Done)
            }
            TransactionState::None => Ok(AuxTxnEnd::Done),
        }
    }

    /// Every catalog this connection can see: `main` first, then each auxiliary
    /// database in registry order. Used by operations whose namespace spans all
    /// databases (trigger names) rather than one.
    pub(crate) fn all_catalogs(&self) -> Vec<Arc<RwLock<Schema>>> {
        let mut catalogs = vec![self.schema.clone()];
        catalogs.extend(
            self.aux_dbs
                .borrow()
                .iter()
                .flatten()
                .map(|aux| aux.schema.clone()),
        );
        catalogs
    }

    /// Roll back every auxiliary database that has an open transaction.
    pub(crate) fn rollback_aux_txns(&self) {
        let slots = self.aux_dbs.borrow();
        for slot in slots.iter().flatten() {
            if !matches!(slot.txn_state.get(), TransactionState::None) {
                slot.pager.rollback();
                slot.txn_state.set(TransactionState::None);
            }
        }
    }

    /// Re-parse the catalog of auxiliary database `db` from its own
    /// `sqlite_schema`, re-tagging every table with `db`.
    pub(crate) fn reparse_aux_schema(
        &self,
        db: usize,
        mv_tx_id: Option<crate::mvcc::database::TxID>,
    ) -> Result<()> {
        let (conn, schema) = {
            let slots = self.aux_dbs.borrow();
            let Some(aux) = slots.get(db).and_then(|slot| slot.as_ref()) else {
                return Err(LimboError::InternalError(format!(
                    "no such database: index {db}"
                )));
            };
            (aux._conn.clone(), aux.schema.clone())
        };
        let stmt = conn.prepare("SELECT * FROM sqlite_schema")?;
        let mut parsed = Schema::new();
        crate::util::parse_schema_rows(
            Some(stmt),
            &mut parsed,
            conn.pager.io.clone(),
            &conn.syms.borrow(),
            mv_tx_id,
        )?;
        retag_schema_db_index(&mut parsed, db);
        *schema.write() = parsed;
        Ok(())
    }

    /// The catalog to compile the next statement against.
    ///
    /// # Errors
    ///
    /// [`LimboError::SchemaLocked`] when the catalog is being rewritten by a
    /// concurrent schema reload.
    pub(crate) fn compile_schema(&self) -> Result<CompileSchema<'_>> {
        match self.resolved_schema()? {
            Some(merged) => Ok(CompileSchema::Merged(Box::new(merged))),
            None => Ok(CompileSchema::Main(
                self.schema.try_read().ok_or(LimboError::SchemaLocked)?,
            )),
        }
    }

    /// Build the catalog view a statement is compiled against.
    ///
    /// Returns `None` when this connection has no auxiliary database, in which
    /// case the caller keeps using `main`'s catalog directly and pays nothing.
    /// Otherwise the returned [`Schema`] contains, for every object:
    ///
    /// * a `"<db>.<object>"` key for qualified references (`main.t`, `temp.t`,
    ///   `alias.t`), and
    /// * a plain key following upstream's unqualified search order
    ///   `temp` → `main` → attached (in attach order).
    pub(crate) fn resolved_schema(&self) -> Result<Option<Schema>> {
        if !self.has_aux_dbs() {
            return Ok(None);
        }
        let main_schema = self
            .schema
            .try_read()
            .ok_or(LimboError::SchemaLocked)?
            .clone();
        let mut merged = main_schema.clone();
        // Qualified aliases for `main` itself.
        overlay_schema(&mut merged, &main_schema, MAIN_DB_NAME, DB_MAIN, false);
        let slots = self.aux_dbs.borrow();
        // Attached databases first (lowest precedence for unqualified names),
        // in attach order.
        for (index, slot) in slots.iter().enumerate().skip(2) {
            let Some(slot) = slot.as_ref() else { continue };
            let aux_schema = slot.schema.read();
            overlay_schema(&mut merged, &aux_schema, &slot.name, index, false);
        }
        // `temp` last: it shadows both `main` and attached databases.
        if let Some(temp) = slots.get(DB_TEMP).and_then(|slot| slot.as_ref()) {
            let temp_schema = temp.schema.read();
            overlay_schema(&mut merged, &temp_schema, TEMP_DB_NAME, DB_TEMP, true);
        }
        Ok(Some(merged))
    }
}

/// The catalog a single statement is compiled against.
///
/// Single-database connections (the overwhelming majority, and every connection
/// that never touches `TEMP`/`ATTACH`) borrow `main`'s catalog directly and pay
/// nothing extra; only a connection with auxiliary databases builds the merged
/// view, and even then the clone is `Arc`-shallow.
pub(crate) enum CompileSchema<'conn> {
    Main(parking_lot::RwLockReadGuard<'conn, Schema>),
    Merged(Box<Schema>),
}

impl CompileSchema<'_> {
    pub(crate) fn as_ref(&self) -> &Schema {
        match self {
            CompileSchema::Main(guard) => guard,
            CompileSchema::Merged(schema) => schema,
        }
    }
}

/// Copy `source`'s objects into `target` under the schema name `db_name`.
///
/// Every object gains a `"<db_name>.<name>"` key. `shadow` decides what happens
/// to the unqualified key: `temp` overwrites it (upstream's precedence), an
/// attached database only fills it in when nothing already claims it.
fn overlay_schema(target: &mut Schema, source: &Schema, db_name: &str, db: usize, shadow: bool) {
    let is_main = db == DB_MAIN;
    for (name, table) in source.tables.iter() {
        if name.contains('.') {
            // Already a qualified key from an earlier overlay pass.
            continue;
        }
        let qualified = format!("{db_name}.{name}");
        target.tables.insert(qualified.clone(), table.clone());
        target.object_db.insert(qualified, db);
        // The schema table is the one name `temp` must NOT shadow: upstream
        // keeps unqualified `sqlite_schema`/`sqlite_master` pointing at `main`
        // and exposes the temp one only as `sqlite_temp_schema` (or
        // `temp.sqlite_schema`).
        let is_schema_table = name == SCHEMA_TABLE_NAME;
        if is_schema_table && !is_main {
            if db == DB_TEMP {
                target
                    .tables
                    .insert(TEMP_SCHEMA_TABLE_NAME.to_string(), table.clone());
                target
                    .object_db
                    .insert(TEMP_SCHEMA_TABLE_NAME.to_string(), db);
            }
            continue;
        }
        if shadow || !target.tables.contains_key(name) {
            target.tables.insert(name.clone(), table.clone());
            target.object_db.insert(name.clone(), db);
        }
    }
    for (table_name, indexes) in source.indexes.iter() {
        if table_name.contains('.') {
            continue;
        }
        target
            .indexes
            .insert(format!("{db_name}.{table_name}"), indexes.clone());
        if shadow || !target.indexes.contains_key(table_name) {
            target.indexes.insert(table_name.clone(), indexes.clone());
        }
        // Index names live in their own key space for `DROP INDEX`.
        for index in indexes {
            let index_key = normalize_ident(&index.name);
            target
                .object_db
                .insert(format!("{db_name}.{index_key}"), db);
            if shadow || !target.object_db.contains_key(&index_key) {
                target.object_db.insert(index_key, db);
            }
        }
    }
    #[cfg(not(feature = "index_experimental"))]
    for table_name in source.has_indexes.iter() {
        if table_name.contains('.') {
            continue;
        }
        target.has_indexes.insert(format!("{db_name}.{table_name}"));
        target.has_indexes.insert(table_name.clone());
    }
    for (trigger_name, trigger) in source.triggers.iter() {
        // Triggers live in a single namespace per connection; a `temp` trigger
        // shadows a `main` trigger of the same name, exactly like tables.
        target
            .object_db
            .insert(format!("{db_name}.{trigger_name}"), db);
        if shadow || !target.triggers.contains_key(trigger_name) {
            target
                .triggers
                .insert(trigger_name.clone(), trigger.clone());
            target.object_db.insert(trigger_name.clone(), db);
        }
    }
}
