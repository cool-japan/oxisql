//! Auto-generated module
//!
//! 🤖 Generated with [SplitRS](https://github.com/cool-japan/splitrs)

use crate::util::normalize_ident;
use crate::VirtualTable;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::bootstrap::{SCHEMA_TABLE_NAME, SCHEMA_TABLE_NAME_ALT};
use super::index::Index;
use super::sqlite_schema_table;
use super::table::{BTreeTable, Table, View};
use super::trigger::{Trigger, TriggerRef};

/// A catalog namespace.
///
/// One `Schema` describes exactly one database (`main`, `temp`, or an attached
/// file). [`crate::Connection::resolved_schema`] additionally builds a merged,
/// *compilation-only* view whose keys carry `"<db>.<object>"` qualifiers; that
/// merged value is cloned per statement, which is why `Schema` is `Clone`
/// (every value it owns is already behind an `Arc`, so the clone is cheap).
#[derive(Clone)]
pub struct Schema {
    pub tables: HashMap<String, Arc<Table>>,
    /// table_name to list of indexes for the table
    pub indexes: HashMap<String, Vec<Arc<Index>>>,
    /// Triggers by normalized trigger name. Triggers live in their own
    /// namespace (a trigger may share a name with a column, but not with
    /// another trigger or an index — SQLite keeps triggers and indexes in
    /// separate namespaces, so only trigger-vs-trigger collides).
    pub triggers: HashMap<String, TriggerRef>,
    /// In-memory side-map of persisted `sqlite_stat1` statistics, loaded after
    /// schema parsing and consumed by the System-R optimizer. Empty (every
    /// lookup returns `None`) until `ANALYZE` statistics are loaded, so the
    /// optimizer falls back bit-for-bit to its hardcoded estimates.
    pub stats: crate::statistics::SchemaStats,
    /// Used for index_experimental feature flag to track whether a table has an index.
    /// This is necessary because we won't populate indexes so that we don't use them but
    /// we still need to know if a table has an index to disallow any write operation that requires
    /// indexes.
    #[cfg(not(feature = "index_experimental"))]
    pub has_indexes: std::collections::HashSet<String>,
    /// Owning database registry index for each object key in this catalog.
    ///
    /// Only populated for the merged, multi-database catalog built by
    /// [`crate::Connection::resolved_schema`], where the same key space holds
    /// objects from `main`, `temp` and attached databases. An absent key means
    /// `main` (index 0), which is the entire single-database case, so this map
    /// stays empty and free until `TEMP`/`ATTACH` is actually used.
    pub object_db: HashMap<String, usize>,
}
impl Schema {
    pub fn new() -> Self {
        let mut tables: HashMap<String, Arc<Table>> = HashMap::new();
        #[cfg(not(feature = "index_experimental"))]
        let has_indexes = std::collections::HashSet::new();
        let indexes: HashMap<String, Vec<Arc<Index>>> = HashMap::new();
        #[allow(clippy::arc_with_non_send_sync)]
        tables.insert(
            SCHEMA_TABLE_NAME.to_string(),
            Arc::new(Table::BTree(sqlite_schema_table().into())),
        );
        Self {
            tables,
            indexes,
            triggers: HashMap::new(),
            stats: crate::statistics::SchemaStats::new(),
            #[cfg(not(feature = "index_experimental"))]
            has_indexes,
            object_db: HashMap::new(),
        }
    }

    /// Whether this catalog knows a database by that schema name.
    ///
    /// `main` always exists. Any other name only exists in a merged
    /// multi-database catalog, where every object also carries a
    /// `"<db>.<object>"` key -- so a single-database catalog accepts `main` and
    /// rejects everything else, exactly as before `ATTACH` existed.
    pub fn has_database(&self, db_name: &str) -> bool {
        let normalized = normalize_ident(db_name);
        if normalized.eq_ignore_ascii_case(crate::multidb::MAIN_DB_NAME) {
            return true;
        }
        let prefix = format!("{normalized}.");
        self.object_db.keys().any(|key| key.starts_with(&prefix))
    }

    /// Which database owns the named object, honouring an optional schema
    /// qualifier. Returns [`crate::multidb::DB_MAIN`] for any object this
    /// catalog does not explicitly place elsewhere.
    pub fn db_index_for_object(&self, db_name: Option<&str>, name: &str) -> usize {
        let normalized = normalize_ident(name);
        let key = match db_name {
            Some(db_name) => format!("{}.{}", normalize_ident(db_name), normalized),
            None => normalized,
        };
        self.object_db
            .get(&key)
            .copied()
            .unwrap_or(crate::multidb::DB_MAIN)
    }

    /// Register (or replace) a trigger under its normalized name.
    pub fn add_trigger(&mut self, trigger: TriggerRef) {
        let name = normalize_ident(&trigger.name);
        self.triggers.insert(name, trigger);
    }

    /// Remove a trigger by name, returning it if it existed.
    pub fn remove_trigger(&mut self, name: &str) -> Option<TriggerRef> {
        self.triggers.remove(&normalize_ident(name))
    }

    /// Every trigger attached to `table_name`, in a deterministic order
    /// (by trigger name) so that generated bytecode is reproducible.
    ///
    /// SQLite does not define a firing order for multiple triggers of the same
    /// kind on the same table; sorting by name simply makes this engine's choice
    /// stable across runs instead of dependent on `HashMap` iteration order.
    pub fn triggers_for_table(&self, table_name: &str) -> Vec<TriggerRef> {
        let name = normalize_ident(table_name);
        let mut found: Vec<TriggerRef> = self
            .triggers
            .values()
            .filter(|t| t.tbl_name == name)
            .cloned()
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }

    /// Remove every trigger attached to `table_name`, returning how many were
    /// removed. Used by `DROP TABLE`, which drops the table's triggers with it.
    pub fn remove_triggers_for_table(&mut self, table_name: &str) -> usize {
        let name = normalize_ident(table_name);
        let before = self.triggers.len();
        self.triggers.retain(|_, t| t.tbl_name != name);
        before - self.triggers.len()
    }

    /// Whether any trigger is registered under `name` (trigger namespace only).
    pub fn trigger_exists(&self, name: &str) -> bool {
        self.triggers.contains_key(&normalize_ident(name))
    }

    /// Convenience constructor used by the schema loader.
    pub fn add_trigger_owned(&mut self, trigger: Trigger) {
        self.add_trigger(Arc::new(trigger));
    }
    pub fn is_unique_idx_name(&self, name: &str) -> bool {
        !self
            .indexes
            .iter()
            .any(|idx| idx.1.iter().any(|i| i.name == name))
    }
    pub fn add_btree_table(&mut self, table: Rc<BTreeTable>) {
        let name = normalize_ident(&table.name);
        self.tables.insert(name, Table::BTree(table).into());
    }
    pub fn add_virtual_table(&mut self, table: Rc<VirtualTable>) {
        let name = normalize_ident(&table.name);
        self.tables.insert(name, Table::Virtual(table).into());
    }
    /// Register (or re-register, overwriting) a view under its normalized name,
    /// in the same flat namespace as tables and virtual tables. Re-registration
    /// is used by the two-phase schema loader: a placeholder view (empty
    /// `columns`) is first inserted so every view is name-resolvable, then
    /// replaced by a column-resolved copy in a later pass.
    pub fn add_view(&mut self, view: Rc<View>) {
        let name = normalize_ident(&view.name);
        self.tables.insert(name, Table::View(view).into());
    }
    pub fn get_table(&self, name: &str) -> Option<Arc<Table>> {
        let name = normalize_ident(name);
        let name = if name.eq_ignore_ascii_case(&SCHEMA_TABLE_NAME_ALT) {
            SCHEMA_TABLE_NAME
        } else {
            &name
        };
        self.tables.get(name).cloned()
    }
    pub fn remove_table(&mut self, table_name: &str) {
        let name = normalize_ident(table_name);
        self.tables.remove(&name);
    }

    /// Look up a table honouring an optional schema qualifier.
    ///
    /// In a merged (multi-database) catalog every object is registered twice:
    /// under its plain name, following upstream's `temp` -> `main` -> attached
    /// precedence, and under `"<db>.<name>"`. An unqualified lookup therefore
    /// takes the precedence path and a qualified one addresses exactly the
    /// database named. In a single-database catalog only `main.<name>` exists as
    /// a qualifier, so `main.t` still resolves and everything else misses.
    pub fn get_table_qualified(&self, db_name: Option<&str>, name: &str) -> Option<Arc<Table>> {
        match db_name {
            None => self.get_table(name),
            Some(db_name) => {
                // `sqlite_master` is an alias for `sqlite_schema` in a qualified
                // reference too (`temp.sqlite_master`), so the object part goes
                // through the same aliasing `get_table` applies unqualified.
                let object = normalize_ident(name);
                let object = if object.eq_ignore_ascii_case(SCHEMA_TABLE_NAME_ALT) {
                    SCHEMA_TABLE_NAME.to_string()
                } else {
                    object
                };
                let qualified = format!("{}.{}", normalize_ident(db_name), object);
                self.get_table(&qualified).or_else(|| {
                    // A single-database catalog has no qualified keys at all;
                    // `main.<t>` must still resolve there.
                    if normalize_ident(db_name).eq_ignore_ascii_case("main") {
                        self.get_table(name)
                    } else {
                        None
                    }
                })
            }
        }
    }
    pub fn get_btree_table(&self, name: &str) -> Option<Rc<BTreeTable>> {
        let name = normalize_ident(name);
        if let Some(table) = self.tables.get(&name) {
            table.btree()
        } else {
            None
        }
    }
    #[cfg(feature = "index_experimental")]
    pub fn add_index(&mut self, index: Arc<Index>) {
        let table_name = normalize_ident(&index.table_name);
        self.indexes
            .entry(table_name)
            .or_default()
            .push(index.clone())
    }
    pub fn get_indices(&self, table_name: &str) -> &[Arc<Index>] {
        let name = normalize_ident(table_name);
        self.indexes
            .get(&name)
            .map_or_else(|| &[] as &[Arc<Index>], |v| v.as_slice())
    }
    pub fn get_index(&self, table_name: &str, index_name: &str) -> Option<&Arc<Index>> {
        let name = normalize_ident(table_name);
        self.indexes
            .get(&name)?
            .iter()
            .find(|index| index.name == index_name)
    }
    pub fn remove_indices_for_table(&mut self, table_name: &str) {
        let name = normalize_ident(table_name);
        self.indexes.remove(&name);
    }
    /// Remove `idx` from its table's index list.
    ///
    /// A table with no registered indexes is a no-op rather than a panic: the
    /// caller (DROP INDEX) has already established the index exists in
    /// `sqlite_schema`, and the in-memory list can legitimately be empty when
    /// the `index_experimental` feature is off, in which case indexes are never
    /// materialized into `Schema::indexes` at all.
    pub fn remove_index(&mut self, idx: &Index) {
        let name = normalize_ident(&idx.table_name);
        if let Some(indexes) = self.indexes.get_mut(&name) {
            indexes.retain_mut(|other_idx| other_idx.name != idx.name);
        }
    }
    #[cfg(not(feature = "index_experimental"))]
    pub fn table_has_indexes(&self, table_name: &str) -> bool {
        self.has_indexes.contains(table_name)
    }
    #[cfg(not(feature = "index_experimental"))]
    pub fn table_set_has_index(&mut self, table_name: &str) {
        self.has_indexes.insert(table_name.to_string());
    }
}
