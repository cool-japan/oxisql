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

pub struct Schema {
    pub tables: HashMap<String, Arc<Table>>,
    /// table_name to list of indexes for the table
    pub indexes: HashMap<String, Vec<Arc<Index>>>,
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
            stats: crate::statistics::SchemaStats::new(),
            #[cfg(not(feature = "index_experimental"))]
            has_indexes,
        }
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
    pub fn remove_index(&mut self, idx: &Index) {
        let name = normalize_ident(&idx.table_name);
        self.indexes
            .get_mut(&name)
            .expect("Must have the index")
            .retain_mut(|other_idx| other_idx.name != idx.name);
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
