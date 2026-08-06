// vendored from limbo 0.0.22 upstream — upstream URLs/HTML in doc comments
//! Core engine of the C-free **oxisqlite** SQLite-compatible database, a
//! Pure-Rust fork of limbo 0.0.22 powering the `oxisql-sqlite-compat` backend.
//!
//! Implements the VDBE bytecode interpreter, B-tree storage/pager/WAL, a
//! System-R cost-based SQL query planner, MVCC transactions (`ROLLBACK`,
//! `SAVEPOINT`), `UPSERT`, and JSON/JSONB support.
#![allow(
    rustdoc::bare_urls,
    rustdoc::invalid_html_tags,
    rustdoc::broken_intra_doc_links
)]
#![allow(clippy::arc_with_non_send_sync)]
// vendored from limbo 0.0.22 upstream; see NOTICE
#![allow(unused_assignments)]
// vendored from limbo 0.0.22 upstream; see NOTICE
#![allow(mismatched_lifetime_syntaxes)]
// vendored from limbo 0.0.22 upstream; see NOTICE
#![allow(unpredictable_function_pointer_comparisons)]
// UPSTREAM: vendored Limbo fork — allow upstream style
#![allow(
    clippy::bool_assert_comparison,
    clippy::collapsible_match,
    clippy::clone_on_copy,
    clippy::comparison_to_empty,
    clippy::derivable_impls,
    clippy::doc_lazy_continuation,
    clippy::doc_overindented_list_items,
    clippy::duplicated_attributes,
    clippy::enum_variant_names,
    clippy::excessive_precision,
    clippy::explicit_auto_deref,
    clippy::explicit_counter_loop,
    clippy::extra_unused_lifetimes,
    clippy::filter_next,
    clippy::from_over_into,
    clippy::get_first,
    clippy::identity_op,
    clippy::inherent_to_string,
    clippy::iter_cloned_collect,
    clippy::large_enum_variant,
    clippy::len_without_is_empty,
    clippy::len_zero,
    clippy::let_and_return,
    clippy::manual_ignore_case_cmp,
    clippy::manual_inspect,
    clippy::manual_is_multiple_of,
    clippy::manual_map,
    clippy::manual_ok_err,
    clippy::manual_range_contains,
    clippy::manual_repeat_n,
    clippy::manual_saturating_arithmetic,
    clippy::manual_strip,
    clippy::map_clone,
    clippy::match_like_matches_macro,
    clippy::needless_borrow,
    clippy::needless_borrows_for_generic_args,
    clippy::needless_lifetimes,
    clippy::needless_range_loop,
    clippy::needless_return,
    clippy::new_without_default,
    clippy::nonminimal_bool,
    clippy::obfuscated_if_else,
    clippy::option_as_ref_deref,
    clippy::option_map_or_none,
    clippy::partialeq_ne_impl,
    clippy::partialeq_to_none,
    clippy::ptr_arg,
    clippy::question_mark,
    clippy::redundant_closure,
    clippy::redundant_field_names,
    clippy::redundant_pattern_matching,
    clippy::should_implement_trait,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::unnecessary_cast,
    clippy::unnecessary_map_or,
    clippy::unnecessary_mut_passed,
    clippy::unnecessary_unwrap,
    clippy::unneeded_struct_pattern,
    clippy::unused_unit,
    clippy::upper_case_acronyms,
    clippy::useless_conversion,
    clippy::useless_format,
    clippy::wrong_self_convention
)]

mod error;
mod ext;
mod fast_lock;
mod function;
mod functions;
mod info;
mod io;
#[cfg(feature = "json")]
mod json;
mod multidb;
pub mod mvcc;
mod parameters;
mod pragma;
mod pseudo;
pub mod result;
mod schema;
mod statistics;
mod storage;
mod translate;
pub mod types;
#[allow(dead_code)]
mod util;
mod vdbe;
mod vector;
mod vtab;

#[cfg(feature = "fuzz")]
pub mod numeric;

#[cfg(not(feature = "fuzz"))]
mod numeric;

use crate::vtab::VirtualTable;
use crate::{fast_lock::SpinLock, translate::optimizer::optimize_plan};
use core::str;
pub use error::LimboError;
use fallible_iterator::FallibleIterator;
pub use io::clock::{Clock, Instant};
#[cfg(all(feature = "fs", target_family = "unix", feature = "native-io"))]
pub use io::UnixIO;
#[cfg(all(feature = "fs", target_os = "linux", feature = "io_uring"))]
pub use io::UringIO;
pub use io::{
    Buffer, Completion, File, MemoryFile, MemoryIO, OpenFlags, PlatformIO, SyscallIO,
    WriteCompletion, IO,
};
use limbo_sqlite3_parser::{ast, ast::Cmd, lexer::sql::Parser};
use parking_lot::RwLock;
use schema::Schema;
use std::{
    borrow::Cow,
    cell::{Cell, RefCell, UnsafeCell},
    collections::HashMap,
    fmt::Display,
    io::Write,
    num::NonZero,
    ops::Deref,
    rc::Rc,
    sync::{Arc, OnceLock},
};
use storage::btree::{btree_init_page, BTreePageInner};
#[cfg(feature = "fs")]
use storage::database::DatabaseFile;
pub use storage::pager::PagerCacheflushStatus;
pub use storage::{
    buffer_pool::BufferPool,
    database::DatabaseStorage,
    pager::PageRef,
    pager::{Page, Pager},
    wal::{CheckpointMode, CheckpointResult, CheckpointStatus, Wal, WalFile, WalFileShared},
};
use storage::{
    database::FileMemoryStorage,
    page_cache::DumbLruPageCache,
    pager::allocate_page,
    sqlite3_ondisk::{DatabaseHeader, DATABASE_HEADER_SIZE, MIN_PAGE_SIZE},
};
use tracing::{instrument, Level};
use translate::delete::prepare_delete_plan;
use translate::select::prepare_select_plan;
use translate::update::prepare_update_plan;
pub use types::RefValue;
pub use types::Value;
use util::parse_schema_rows;
use vdbe::builder::QueryMode;
use vdbe::builder::TableRefIdCounter;

pub type Result<T, E = LimboError> = std::result::Result<T, E>;
pub static DATABASE_VERSION: OnceLock<String> = OnceLock::new();

#[derive(Clone, Copy, PartialEq, Eq)]
enum TransactionState {
    Write,
    Read,
    None,
}

pub(crate) type MvStore = mvcc::MvStore<mvcc::LocalClock>;

pub(crate) type MvCursor = mvcc::cursor::ScanCursor<mvcc::LocalClock>;

pub struct Database {
    mv_store: Option<Rc<MvStore>>,
    schema: Arc<RwLock<Schema>>,
    // TODO: make header work without lock
    header: Arc<SpinLock<DatabaseHeader>>,
    db_file: Arc<dyn DatabaseStorage>,
    io: Arc<dyn IO>,
    page_size: u32,
    // Shared structures of a Database are the parts that are common to multiple threads that might
    // create DB connections.
    _shared_page_cache: Arc<RwLock<DumbLruPageCache>>,
    shared_wal: Arc<UnsafeCell<WalFileShared>>,
    open_flags: OpenFlags,
}

unsafe impl Send for Database {}
unsafe impl Sync for Database {}

impl Database {
    #[cfg(feature = "fs")]
    pub fn open_file(io: Arc<dyn IO>, path: &str, enable_mvcc: bool) -> Result<Arc<Database>> {
        Self::open_file_with_flags(io, path, OpenFlags::default(), enable_mvcc)
    }

    #[cfg(feature = "fs")]
    pub fn open_file_with_flags(
        io: Arc<dyn IO>,
        path: &str,
        flags: OpenFlags,
        enable_mvcc: bool,
    ) -> Result<Arc<Database>> {
        let file = io.open_file(path, flags, true)?;
        // `db_freshly_created` is true when the main database file did not exist
        // (or was empty) and we just wrote its bootstrap page. In that case any
        // `-wal` file found alongside it is an orphan from a previous database
        // incarnation and must NOT be replayed (doing so resurrects stale
        // committed data — see `WalFileShared::open_shared`).
        let db_freshly_created = maybe_init_database_file(&file, &io)?;
        let db_file = Arc::new(DatabaseFile::new(file));
        Self::open_with_flags_inner(io, path, db_file, flags, enable_mvcc, db_freshly_created)
    }

    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open(
        io: Arc<dyn IO>,
        path: &str,
        db_file: Arc<dyn DatabaseStorage>,
        enable_mvcc: bool,
    ) -> Result<Arc<Database>> {
        Self::open_with_flags(io, path, db_file, OpenFlags::default(), enable_mvcc)
    }

    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open_with_flags(
        io: Arc<dyn IO>,
        path: &str,
        db_file: Arc<dyn DatabaseStorage>,
        flags: OpenFlags,
        enable_mvcc: bool,
    ) -> Result<Arc<Database>> {
        // Callers that supply their own `db_file` (custom storage backends,
        // tests) take responsibility for WAL lifecycle, so we conservatively
        // treat the database as pre-existing (never discard a WAL here).
        Self::open_with_flags_inner(io, path, db_file, flags, enable_mvcc, false)
    }

    #[allow(clippy::arc_with_non_send_sync)]
    fn open_with_flags_inner(
        io: Arc<dyn IO>,
        path: &str,
        db_file: Arc<dyn DatabaseStorage>,
        flags: OpenFlags,
        enable_mvcc: bool,
        db_freshly_created: bool,
    ) -> Result<Arc<Database>> {
        let db_header = Pager::begin_open(db_file.clone())?;
        // ensure db header is there
        io.run_once()?;

        let page_size = db_header.lock().get_page_size();
        let wal_path = format!("{}-wal", path);
        let shared_wal = WalFileShared::open_shared_inner(
            &io,
            wal_path.as_str(),
            page_size,
            db_freshly_created,
        )?;

        DATABASE_VERSION.get_or_init(|| {
            let version = db_header.lock().version_number;
            version.to_string()
        });

        let mv_store = if enable_mvcc {
            Some(Rc::new(MvStore::new(
                mvcc::LocalClock::new(),
                mvcc::persistent_storage::Storage::new_noop(),
            )))
        } else {
            None
        };

        let shared_page_cache = Arc::new(RwLock::new(DumbLruPageCache::default()));
        let schema = Arc::new(RwLock::new(Schema::new()));
        let db = Database {
            mv_store,
            schema: schema.clone(),
            header: db_header.clone(),
            _shared_page_cache: shared_page_cache.clone(),
            shared_wal: shared_wal.clone(),
            db_file,
            io: io.clone(),
            page_size,
            open_flags: flags,
        };
        let db = Arc::new(db);
        {
            // parse schema
            let conn = db.connect()?;
            // The header was read straight from the main database file above,
            // bypassing the WAL. If a previous session committed a page-1 change
            // (e.g. `PRAGMA application_id` / `PRAGMA user_version`) to the WAL
            // without checkpointing it back into the main file, that change is
            // durable in the WAL but absent from the freshly-read header. Resolve
            // page 1 through the now-recovered WAL so the shared header reflects
            // the latest committed cookie values before any query runs.
            conn.pager.refresh_header_from_wal()?;
            {
                let rows = conn.query("SELECT * FROM sqlite_schema")?;
                let mut schema = schema
                    .try_write()
                    .expect("lock on schema should succeed first try");
                let syms = conn.syms.borrow();
                if let Err(LimboError::ExtensionError(e)) =
                    parse_schema_rows(rows, &mut schema, io, &syms, None)
                {
                    // this means that a vtab exists and we no longer have the module loaded. we print
                    // a warning to the user to load the module
                    eprintln!("Warning: {}", e);
                }
            } // schema write guard + syms dropped here
              // Load persisted ANALYZE statistics (no-op for un-analyzed databases).
            conn.load_persistent_stats()?;
        }
        Ok(db)
    }

    pub fn connect(self: &Arc<Database>) -> Result<Arc<Connection>> {
        let buffer_pool = Rc::new(BufferPool::new(self.page_size as usize));

        let wal = Rc::new(RefCell::new(WalFile::new(
            self.io.clone(),
            self.page_size,
            self.shared_wal.clone(),
            buffer_pool.clone(),
        )));
        // For now let's open database without shared cache by default.
        let pager = Rc::new(Pager::finish_open(
            self.header.clone(),
            self.db_file.clone(),
            wal,
            self.io.clone(),
            Arc::new(RwLock::new(DumbLruPageCache::default())),
            buffer_pool,
        )?);
        let conn = Arc::new(Connection {
            _db: self.clone(),
            pager: pager.clone(),
            schema: self.schema.clone(),
            header: self.header.clone(),
            last_insert_rowid: Cell::new(0),
            auto_commit: Cell::new(true),
            mv_transactions: RefCell::new(Vec::new()),
            transaction_state: Cell::new(TransactionState::None),
            last_change: Cell::new(0),
            syms: RefCell::new(SymbolTable::new()),
            total_changes: Cell::new(0),
            _shared_cache: false,
            cache_size: Cell::new(self.header.lock().default_page_cache_size),
            closed: Cell::new(false),
            aux_dbs: RefCell::new(Vec::new()),
        });
        if let Err(e) = conn.register_builtins() {
            return Err(LimboError::ExtensionError(e));
        }
        Ok(conn)
    }

    /// Open a new database file with a specified VFS without an existing database
    /// connection and symbol table to register extensions.
    #[cfg(feature = "fs")]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open_new(path: &str, vfs: &str) -> Result<(Arc<dyn IO>, Arc<Database>)> {
        let vfsmods = ext::add_builtin_vfs_extensions(None)?;
        let io: Arc<dyn IO> = match vfsmods.iter().find(|v| v.0 == vfs).map(|v| v.1.clone()) {
            Some(vfs) => vfs,
            None => match vfs.trim() {
                "memory" => Arc::new(MemoryIO::new()),
                "syscall" => Arc::new(SyscallIO::new()?),
                #[cfg(all(target_os = "linux", feature = "io_uring"))]
                "io_uring" => Arc::new(UringIO::new()?),
                other => {
                    return Err(LimboError::InvalidArgument(format!(
                        "no such VFS: {}",
                        other
                    )));
                }
            },
        };
        let db = Self::open_file(io.clone(), path, false)?;
        Ok((io, db))
    }

    /// Open a database directly from an in-memory SQLite database image
    /// (e.g. the output of `include_bytes!`, `VACUUM INTO`, or
    /// `sqlite3_serialize()`).
    ///
    /// The bytes are copied into a fresh in-memory page store backed by
    /// [`MemoryIO`] / [`MemoryFile`]; `bytes` is never mutated and no file
    /// I/O ever occurs. This entry point is deliberately **not** gated by the
    /// `fs` feature, so it works on `wasm32`/WASI and read-only filesystems
    /// where materializing a temp file is impossible.
    ///
    /// The database is treated as pre-existing (see [`Database::open`]): any
    /// companion `-wal` is irrelevant because [`MemoryIO`] always hands back a
    /// fresh empty WAL file, so a fresh WAL header is created and nothing is
    /// replayed.
    ///
    /// # Errors
    ///
    /// Returns [`LimboError::NotADB`] if `bytes` is shorter than the 100-byte
    /// header or does not start with the `"SQLite format 3\0"` magic, and
    /// [`LimboError::Corrupt`] if the header encodes an invalid page size.
    /// Never panics on malformed input.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn open_from_bytes(bytes: &[u8], enable_mvcc: bool) -> Result<Arc<Database>> {
        const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";
        // Mirrors the private `sqlite3_ondisk::MAX_PAGE_SIZE`.
        const SQLITE_MAX_PAGE_SIZE: u32 = 65536;

        if bytes.len() < DATABASE_HEADER_SIZE || &bytes[0..16] != SQLITE_MAGIC {
            return Err(LimboError::NotADB);
        }
        // Page size lives at header offset 16..18, big-endian. The value 1
        // encodes the 65536 maximum (which does not fit in a u16).
        let raw = u16::from_be_bytes([bytes[16], bytes[17]]);
        let page_size = if raw == 1 {
            SQLITE_MAX_PAGE_SIZE
        } else {
            u32::from(raw)
        };
        if page_size < MIN_PAGE_SIZE
            || page_size > SQLITE_MAX_PAGE_SIZE
            || page_size.count_ones() != 1
        {
            return Err(LimboError::Corrupt(format!(
                "invalid page size {page_size} in database header"
            )));
        }

        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let file: Arc<dyn File> = Arc::new(MemoryFile::from_bytes(bytes));
        let db_file: Arc<dyn DatabaseStorage> = Arc::new(FileMemoryStorage::new(file));
        Self::open(io, ":memory:", db_file, enable_mvcc)
    }
}

/// Initialize a brand-new database file (write the bootstrap page 1) if the
/// file is empty.
///
/// Returns `true` when the file was freshly created (its size was 0 before this
/// call wrote the bootstrap header), and `false` when an existing, non-empty
/// database file was opened. Callers use this to detect the
/// "fresh main DB file + pre-existing `-wal`" situation, where the WAL is
/// orphaned from a previous database incarnation and MUST NOT be replayed (see
/// [`storage::wal::WalFileShared::open_shared`]).
pub fn maybe_init_database_file(file: &Arc<dyn File>, io: &Arc<dyn IO>) -> Result<bool> {
    if file.size()? == 0 {
        // init db
        let db_header = DatabaseHeader::default();
        let page1 = allocate_page(
            1,
            &Rc::new(BufferPool::new(db_header.get_page_size() as usize)),
            DATABASE_HEADER_SIZE,
        );
        let page1 = Arc::new(BTreePageInner {
            page: RefCell::new(page1),
        });
        {
            // Create the sqlite_schema table, for this we just need to create the btree page
            // for the first page of the database which is basically like any other btree page
            // but with a 100 byte offset, so we just init the page so that sqlite understands
            // this is a correct page.
            btree_init_page(
                &page1,
                storage::sqlite3_ondisk::PageType::TableLeaf,
                DATABASE_HEADER_SIZE,
                (db_header.get_page_size() - db_header.reserved_space as u32) as u16,
            );

            let page1 = page1.get();
            let contents = page1
                .get()
                .contents
                .as_mut()
                .expect("invariant: page1 contents initialized before write"); // UPSTREAM (Limbo): unwrap — needs proper error propagation
            contents.write_database_header(&db_header);
            // write the first page to disk synchronously
            let flag_complete = Rc::new(RefCell::new(false));
            {
                let flag_complete = flag_complete.clone();
                let completion = Completion::Write(WriteCompletion::new(Box::new(move |_| {
                    *flag_complete.borrow_mut() = true;
                })));
                #[allow(clippy::arc_with_non_send_sync)]
                file.pwrite(0, contents.buffer.clone(), Arc::new(completion))?;
            }
            let mut limit = 100;
            loop {
                io.run_once()?;
                if *flag_complete.borrow() {
                    break;
                }
                limit -= 1;
                if limit == 0 {
                    panic!("Database file couldn't be initialized, io loop run for {} iterations and write didn't finish", limit);
                }
            }
        }
        // The file was empty and has just been initialized: it is a brand-new
        // database, so any pre-existing WAL belongs to a previous incarnation.
        return Ok(true);
    }
    Ok(false)
}

pub struct Connection {
    _db: Arc<Database>,
    pager: Rc<Pager>,
    schema: Arc<RwLock<Schema>>,
    header: Arc<SpinLock<DatabaseHeader>>,
    auto_commit: Cell<bool>,
    mv_transactions: RefCell<Vec<crate::mvcc::database::TxID>>,
    transaction_state: Cell<TransactionState>,
    last_insert_rowid: Cell<i64>,
    last_change: Cell<i64>,
    total_changes: Cell<i64>,
    syms: RefCell<SymbolTable>,
    _shared_cache: bool,
    cache_size: Cell<i32>,
    /// Set once the connection has been checkpoint-closed (via `close()` or
    /// `Drop`) so the work is not repeated.
    closed: Cell<bool>,
    /// Per-connection registry of auxiliary databases, mirroring upstream
    /// SQLite's `sqlite3.aDb[]`: index 0 (`main`) is intentionally never stored
    /// here (it is this connection's own `pager`/`schema`/`transaction_state`),
    /// index 1 is the lazily-created `temp` database, and indices 2.. are
    /// `ATTACH`ed databases. Empty (and therefore free) until a statement first
    /// needs `TEMP` or `ATTACH`. See [`crate::multidb`].
    aux_dbs: RefCell<Vec<Option<multidb::AuxDb>>>,
}

impl Connection {
    #[instrument(skip_all, level = Level::TRACE)]
    pub fn prepare(self: &Arc<Connection>, sql: impl AsRef<str>) -> Result<Statement> {
        if sql.as_ref().is_empty() {
            return Err(LimboError::InvalidArgument(
                "The supplied SQL string contains no statements".to_string(),
            ));
        }

        let sql = sql.as_ref();
        tracing::trace!("Preparing: {}", sql);
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser.next()?;
        let syms = self.syms.borrow();
        let cmd = cmd.expect("Successful parse on nonempty input string should produce a command");
        let byte_offset_end = parser.offset();
        let input = str::from_utf8(&sql.as_bytes()[..byte_offset_end])
            .expect("invariant: sql is valid UTF-8 bytes slice") // UPSTREAM (Limbo): unwrap — needs proper error propagation
            .trim();
        match cmd {
            Cmd::Stmt(stmt) => {
                let program = Rc::new(translate::translate(
                    self.compile_schema()?.as_ref(),
                    stmt,
                    self.header.clone(),
                    self.pager.clone(),
                    self.clone(),
                    &syms,
                    QueryMode::Normal,
                    &input,
                )?);
                Ok(Statement::new(
                    program,
                    self._db.mv_store.clone(),
                    self.pager.clone(),
                ))
            }
            // `QueryMode::Explain` only gates comment-capture during code
            // generation (see `ProgramBuilder::new` in `vdbe::builder`); it
            // does not change which opcodes get emitted, so the compiled
            // program is the very same executable statement `Cmd::Stmt`
            // would produce. Stepping the returned `Statement` therefore
            // *runs the real statement* rather than listing bytecode --
            // callers must call `Statement::explain()` to get the `EXPLAIN`
            // text, exactly as `Connection::execute()`'s own `Cmd::Explain`
            // arm does (it builds the program and calls `.explain()`
            // without ever stepping it).
            Cmd::Explain(stmt) => {
                let program = Rc::new(translate::translate(
                    self.compile_schema()?.as_ref(),
                    stmt,
                    self.header.clone(),
                    self.pager.clone(),
                    self.clone(),
                    &syms,
                    QueryMode::Explain,
                    &input,
                )?);
                Ok(Statement::new(
                    program,
                    self._db.mv_store.clone(),
                    self.pager.clone(),
                ))
            }
            // `EXPLAIN QUERY PLAN` never compiles to a `vdbe::Program`: it
            // computes and formats the planner's chosen access method
            // directly (see `Connection::explain_query_plan`), so there is
            // no `Statement` for `prepare()` to hand back. `query()` and
            // `execute()` handle it directly and print the plan text.
            Cmd::ExplainQueryPlan(_stmt) => crate::bail_parse_error!(
                "EXPLAIN QUERY PLAN is not supported via Connection::prepare(); use Connection::query() or Connection::execute() instead"
            ),
        }
    }

    #[instrument(skip_all, level = Level::TRACE)]
    pub fn query(self: &Arc<Connection>, sql: impl AsRef<str>) -> Result<Option<Statement>> {
        let sql = sql.as_ref();
        tracing::trace!("Querying: {}", sql);
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser.next()?;
        let byte_offset_end = parser.offset();
        let input = str::from_utf8(&sql.as_bytes()[..byte_offset_end])
            .expect("invariant: sql is valid UTF-8 bytes slice") // UPSTREAM (Limbo): unwrap — needs proper error propagation
            .trim();
        match cmd {
            Some(cmd) => self.run_cmd(cmd, input),
            None => Ok(None),
        }
    }

    #[instrument(skip_all, level = Level::TRACE)]
    pub(crate) fn run_cmd(
        self: &Arc<Connection>,
        cmd: Cmd,
        input: &str,
    ) -> Result<Option<Statement>> {
        let syms = self.syms.borrow();
        match cmd {
            Cmd::Stmt(ref stmt) | Cmd::Explain(ref stmt) => {
                let program = translate::translate(
                    self.compile_schema()?.as_ref(),
                    stmt.clone(),
                    self.header.clone(),
                    self.pager.clone(),
                    self.clone(),
                    &syms,
                    cmd.into(),
                    input,
                )?;
                let stmt = Statement::new(
                    program.into(),
                    self._db.mv_store.clone(),
                    self.pager.clone(),
                );
                Ok(Some(stmt))
            }
            Cmd::ExplainQueryPlan(stmt) => {
                let plan_text = self.explain_query_plan(stmt)?;
                let _ = std::io::stdout().write_all(plan_text.as_bytes());
                Ok(None)
            }
        }
    }

    /// Build the `EXPLAIN QUERY PLAN` textual output for `stmt`.
    ///
    /// Shared by [`Connection::run_cmd`] and [`Connection::execute`] so the
    /// plan-preparation logic -- `prepare_select_plan` / `prepare_update_plan`
    /// / `prepare_delete_plan`, followed by `optimize_plan` and the
    /// `Display` impls in `translate::display` -- lives in exactly one
    /// place instead of being duplicated between the two call sites.
    ///
    /// Unlike `EXPLAIN`, `EXPLAIN QUERY PLAN` never touches the VDBE: it
    /// computes the planner's chosen access method (table scan vs. rowid /
    /// index search) and formats it directly, without compiling or running
    /// any bytecode, so it is always side-effect free.
    fn explain_query_plan(&self, stmt: ast::Stmt) -> Result<String> {
        let mut table_ref_counter = TableRefIdCounter::new();
        let schema = self.schema.try_read().ok_or(LimboError::SchemaLocked)?;
        let plan = match stmt {
            ast::Stmt::Select(select) => {
                let syms = self.syms.borrow();
                let mut plan = prepare_select_plan(
                    schema.deref(),
                    *select,
                    &syms,
                    &[],
                    &mut table_ref_counter,
                    translate::plan::QueryDestination::ResultRows,
                )?;
                optimize_plan(&mut plan, schema.deref())?;
                plan
            }
            ast::Stmt::Update(mut update) => {
                let mut plan =
                    prepare_update_plan(schema.deref(), &mut update, &mut table_ref_counter)?;
                optimize_plan(&mut plan, schema.deref())?;
                plan
            }
            ast::Stmt::Delete(delete) => {
                let ast::Delete {
                    tbl_name,
                    where_clause,
                    limit,
                    ..
                } = *delete;
                let mut plan = prepare_delete_plan(
                    schema.deref(),
                    &tbl_name,
                    where_clause,
                    limit,
                    &mut table_ref_counter,
                )?;
                optimize_plan(&mut plan, schema.deref())?;
                plan
            }
            // `Insert` has no `Plan`/`Display` type yet (see `translate::plan`
            // and `translate::display`) -- report cleanly instead of
            // reaching an unreachable/panicking arm.
            ast::Stmt::Insert(_) => {
                crate::bail_parse_error!(
                    "EXPLAIN QUERY PLAN is not supported for INSERT statements"
                )
            }
            _ => crate::bail_parse_error!(
                "EXPLAIN QUERY PLAN is only supported for SELECT, UPDATE, and DELETE statements"
            ),
        };
        Ok(plan.to_string())
    }

    pub fn query_runner<'a>(self: &'a Arc<Connection>, sql: &'a [u8]) -> QueryRunner<'a> {
        QueryRunner::new(self, sql)
    }

    /// Execute will run a query from start to finish taking ownership of I/O because it will run pending I/Os if it didn't finish.
    /// TODO: make this api async
    #[instrument(skip_all, level = Level::TRACE)]
    pub fn execute(self: &Arc<Connection>, sql: impl AsRef<str>) -> Result<()> {
        let sql = sql.as_ref();
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser.next()?;
        let byte_offset_end = parser.offset();
        let input = str::from_utf8(&sql.as_bytes()[..byte_offset_end])
            .expect("invariant: sql is valid UTF-8 bytes slice") // UPSTREAM (Limbo): unwrap — needs proper error propagation
            .trim();
        if let Some(cmd) = cmd {
            match cmd {
                Cmd::Explain(stmt) => {
                    // `syms` only needs to live through translation, not past it -- scoped to
                    // this block (rather than borrowed once for the whole function) so it is
                    // dropped before any subsequent statement executes. See the `Cmd::Stmt` arm
                    // below for why that distinction matters.
                    let syms = self.syms.borrow();
                    let program = translate::translate(
                        self.compile_schema()?.as_ref(),
                        stmt,
                        self.header.clone(),
                        self.pager.clone(),
                        self.clone(),
                        &syms,
                        QueryMode::Explain,
                        &input,
                    )?;
                    let _ = std::io::stdout().write_all(program.explain().as_bytes());
                }
                Cmd::ExplainQueryPlan(stmt) => {
                    let plan_text = self.explain_query_plan(stmt)?;
                    let _ = std::io::stdout().write_all(plan_text.as_bytes());
                }
                Cmd::Stmt(stmt) => {
                    // `syms` (a `RefCell` borrow) must be dropped before `program.step()` runs
                    // below: bytecode execution can itself need a *mutable* borrow of the same
                    // `RefCell` -- e.g. `Insn::VCreate` (`CREATE VIRTUAL TABLE`) registers the
                    // newly-created table into `syms.vtabs`. Scoping `syms` to just this
                    // translation call (instead of borrowing it once for the whole function, as
                    // this used to do) ensures it is released before the execution loop starts,
                    // instead of staying alive for the rest of `execute()` and making any such
                    // instruction panic with "already borrowed".
                    let program = {
                        let syms = self.syms.borrow();
                        translate::translate(
                            self.compile_schema()?.as_ref(),
                            stmt,
                            self.header.clone(),
                            self.pager.clone(),
                            self.clone(),
                            &syms,
                            QueryMode::Normal,
                            &input,
                        )?
                    };

                    let mut state =
                        vdbe::ProgramState::new(program.max_registers, program.cursor_ref.len());
                    loop {
                        let res = program.step(
                            &mut state,
                            self._db.mv_store.clone(),
                            self.pager.clone(),
                        )?;
                        if matches!(res, StepResult::Done) {
                            break;
                        }
                        self._db.io.run_once()?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn wal_frame_count(&self) -> Result<u64> {
        self.pager.wal_frame_count()
    }

    pub fn wal_get_frame(
        &self,
        frame_no: u32,
        p_frame: *mut u8,
        frame_len: u32,
    ) -> Result<Arc<Completion>> {
        self.pager.wal_get_frame(frame_no, p_frame, frame_len)
    }

    /// Flush dirty pages to disk.
    /// This will write the dirty pages to the WAL and then fsync the WAL.
    /// If the WAL size is over the checkpoint threshold, it will checkpoint the WAL to
    /// the database file and then fsync the database file.
    pub fn cacheflush(&self) -> Result<PagerCacheflushStatus> {
        self.pager.cacheflush()
    }

    pub fn clear_page_cache(&self) -> Result<()> {
        self.pager.clear_page_cache();
        Ok(())
    }

    pub fn checkpoint(&self) -> Result<CheckpointResult> {
        let checkpoint_result = self.pager.wal_checkpoint();
        Ok(checkpoint_result)
    }

    /// Run a checkpoint in TRUNCATE mode (resets the WAL file to empty).
    pub fn checkpoint_truncate(&self) -> Result<CheckpointResult> {
        self.pager.wal_checkpoint_mode(CheckpointMode::Truncate)
    }

    /// Close a connection, checkpointing and truncating the WAL so the database
    /// file is self-contained. Idempotent: a second call is a no-op.
    pub fn close(&self) -> Result<()> {
        if self.closed.replace(true) {
            return Ok(());
        }
        // Auxiliary databases are per-connection. Dropping the registry drops
        // each nested connection (which checkpoints an attached file on the way
        // out) and discards the `temp` database and everything in it -- exactly
        // upstream SQLite's `sqlite_temp_schema` lifetime.
        self.aux_dbs.borrow_mut().clear();
        self.pager.checkpoint_shutdown()
    }

    pub fn last_insert_rowid(&self) -> i64 {
        self.last_insert_rowid.get()
    }

    fn update_last_rowid(&self, rowid: i64) {
        self.last_insert_rowid.set(rowid);
    }

    pub fn set_changes(&self, nchange: i64) {
        self.last_change.set(nchange);
        let prev_total_changes = self.total_changes.get();
        self.total_changes.set(prev_total_changes + nchange);
    }

    /// Return the number of rows changed by the most recent DML statement.
    ///
    /// Mirrors `sqlite3_changes()` semantics: DDL and `BEGIN`/`COMMIT`/`ROLLBACK`
    /// return 0.  The count is updated by [`Connection::set_changes`] when a write transaction
    /// commits.
    pub fn changes(&self) -> i64 {
        self.last_change.get()
    }

    pub fn total_changes(&self) -> i64 {
        self.total_changes.get()
    }

    pub fn get_cache_size(&self) -> i32 {
        self.cache_size.get()
    }
    pub fn set_cache_size(&self, size: i32) {
        self.cache_size.set(size);
    }

    #[cfg(feature = "fs")]
    pub fn open_new(&self, path: &str, vfs: &str) -> Result<(Arc<dyn IO>, Arc<Database>)> {
        Database::open_with_vfs(&self._db, path, vfs)
    }

    pub fn list_vfs(&self) -> Vec<String> {
        let mut all_vfs = vec![String::from("memory")];
        #[cfg(feature = "fs")]
        {
            #[cfg(target_family = "unix")]
            {
                all_vfs.push("syscall".to_string());
            }
            #[cfg(all(target_os = "linux", feature = "io_uring"))]
            {
                all_vfs.push("io_uring".to_string());
            }
            all_vfs.extend(crate::ext::list_vfs_modules());
        }
        all_vfs
    }

    pub fn get_auto_commit(&self) -> bool {
        self.auto_commit.get()
    }

    pub fn parse_schema_rows(self: &Arc<Connection>) -> Result<()> {
        let rows = self.query("SELECT * FROM sqlite_schema")?;
        {
            let mut schema = self
                .schema
                .try_write()
                .expect("lock on schema should succeed first try");
            let syms = self.syms.borrow();
            if let Err(LimboError::ExtensionError(e)) =
                parse_schema_rows(rows, &mut schema, self.pager.io.clone(), &syms, None)
            {
                // this means that a vtab exists and we no longer have the module loaded. we print
                // a warning to the user to load the module
                eprintln!("Warning: {}", e);
            }
        } // schema write guard dropped here
        self.load_persistent_stats()?;
        Ok(())
    }

    /// Load persisted `ANALYZE` statistics (`sqlite_stat1`) into the schema's
    /// in-memory side-map. No-op (bit-for-bit unchanged) when `sqlite_stat1`
    /// does not exist, i.e. for databases that have never been analyzed.
    pub fn load_persistent_stats(self: &Arc<Connection>) -> Result<()> {
        {
            let schema = self.schema.read();
            if schema.get_table("sqlite_stat1").is_none() {
                return Ok(());
            }
        } // read lock dropped before preparing / write-locking (no deadlock)
        let rows = self.query("SELECT tbl, idx, stat FROM sqlite_stat1")?;
        let mut schema = self.schema.write();
        schema.stats.clear();
        crate::util::load_stat1(rows, &mut schema, self.pager.io.clone(), None)?;
        Ok(())
    }

    // Clearly there is something to improve here, Vec<Vec<Value>> isn't a couple of tea
    /// Query the current rows/values of `pragma_name`.
    pub fn pragma_query(self: &Arc<Connection>, pragma_name: &str) -> Result<Vec<Vec<Value>>> {
        let pragma = format!("PRAGMA {}", pragma_name);
        let mut stmt = self.prepare(pragma)?;
        let mut results = Vec::new();
        loop {
            match stmt.step()? {
                vdbe::StepResult::Row => {
                    let row: Vec<Value> = stmt
                        .row()
                        .expect("invariant: row available after StepResult::Row") // UPSTREAM (Limbo): unwrap — needs proper error propagation
                        .get_values()
                        .map(|v| v.clone())
                        .collect();
                    results.push(row);
                }
                vdbe::StepResult::Interrupt | vdbe::StepResult::Busy => {
                    return Err(LimboError::Busy);
                }
                _ => break,
            }
        }

        Ok(results)
    }

    /// Set a new value to `pragma_name`.
    ///
    /// Some pragmas will return the updated value which cannot be retrieved
    /// with this method.
    pub fn pragma_update<V: Display>(
        self: &Arc<Connection>,
        pragma_name: &str,
        pragma_value: V,
    ) -> Result<Vec<Vec<Value>>> {
        let pragma = format!("PRAGMA {} = {}", pragma_name, pragma_value);
        let mut stmt = self.prepare(pragma)?;
        let mut results = Vec::new();
        loop {
            match stmt.step()? {
                vdbe::StepResult::Row => {
                    let row: Vec<Value> = stmt
                        .row()
                        .expect("invariant: row available after StepResult::Row") // UPSTREAM (Limbo): unwrap — needs proper error propagation
                        .get_values()
                        .map(|v| v.clone())
                        .collect();
                    results.push(row);
                }
                vdbe::StepResult::IO => {
                    // The async IO model requires re-polling until the pager
                    // finishes flushing; continue the loop instead of breaking.
                    stmt.run_once()?;
                }
                vdbe::StepResult::Interrupt | vdbe::StepResult::Busy => {
                    return Err(LimboError::Busy);
                }
                _ => break,
            }
        }

        Ok(results)
    }

    /// Query the current value(s) of `pragma_name` associated to
    /// `pragma_value`.
    ///
    /// This method can be used with query-only pragmas which need an argument
    /// (e.g. `table_info('one_tbl')`) or pragmas which returns value(s)
    /// (e.g. `integrity_check`).
    pub fn pragma<V: Display>(
        self: &Arc<Connection>,
        pragma_name: &str,
        pragma_value: V,
    ) -> Result<Vec<Vec<Value>>> {
        let pragma = format!("PRAGMA {}({})", pragma_name, pragma_value);
        let mut stmt = self.prepare(pragma)?;
        let mut results = Vec::new();
        loop {
            match stmt.step()? {
                vdbe::StepResult::Row => {
                    let row: Vec<Value> = stmt
                        .row()
                        .expect("invariant: row available after StepResult::Row") // UPSTREAM (Limbo): unwrap — needs proper error propagation
                        .get_values()
                        .map(|v| v.clone())
                        .collect();
                    results.push(row);
                }
                vdbe::StepResult::IO => {
                    // The async IO model requires re-polling until the pager
                    // finishes flushing; continue the loop instead of breaking.
                    stmt.run_once()?;
                }
                vdbe::StepResult::Interrupt | vdbe::StepResult::Busy => {
                    return Err(LimboError::Busy);
                }
                _ => break,
            }
        }

        Ok(results)
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Best-effort clean close: checkpoint + truncate the WAL so a clean
        // process exit leaves the `.db` self-contained for a byte-level reader.
        // This must never panic or propagate errors from a destructor.
        if self.closed.replace(true) {
            return;
        }
        // Same per-connection lifetime as `close()`: `temp` and every attached
        // database go away with the connection.
        self.aux_dbs.borrow_mut().clear();
        if let Err(e) = self.pager.checkpoint_shutdown() {
            tracing::warn!("Connection::drop best-effort checkpoint failed: {e}");
        }
    }
}

pub struct Statement {
    program: Rc<vdbe::Program>,
    state: vdbe::ProgramState,
    mv_store: Option<Rc<MvStore>>,
    pager: Rc<Pager>,
}

impl Statement {
    pub fn new(
        program: Rc<vdbe::Program>,
        mv_store: Option<Rc<MvStore>>,
        pager: Rc<Pager>,
    ) -> Self {
        let state = vdbe::ProgramState::new(program.max_registers, program.cursor_ref.len());
        Self {
            program,
            state,
            mv_store,
            pager,
        }
    }

    pub fn set_mv_tx_id(&mut self, mv_tx_id: Option<u64>) {
        self.state.mv_tx_id = mv_tx_id;
    }

    pub fn interrupt(&mut self) {
        self.state.interrupt();
    }

    pub fn step(&mut self) -> Result<StepResult> {
        self.program
            .step(&mut self.state, self.mv_store.clone(), self.pager.clone())
    }

    pub fn run_once(&self) -> Result<()> {
        self.pager.io.run_once()
    }

    pub fn num_columns(&self) -> usize {
        self.program.result_columns.len()
    }

    pub fn get_column_name(&self, idx: usize) -> Cow<str> {
        let column = &self.program.result_columns.get(idx).expect("No column");
        match column.name(&self.program.table_references) {
            Some(name) => Cow::Borrowed(name),
            None => Cow::Owned(column.expr.to_string()),
        }
    }

    /// Return the declared SQL type string for result column `idx`, if available.
    ///
    /// The declared type is the text written after the column name in `CREATE TABLE`
    /// (e.g. `"DATE"`, `"TIMESTAMP"`, `"UUID"`, `"INTEGER"`, …).  For computed
    /// expressions or columns from sub-selects the method returns `None`.
    pub fn get_column_decl_type(&self, idx: usize) -> Option<Cow<str>> {
        let column = self.program.result_columns.get(idx)?;
        match &column.expr {
            limbo_sqlite3_parser::ast::Expr::Column {
                table,
                column: col_idx,
                ..
            } => self
                .program
                .table_references
                .find_table_by_internal_id(*table)
                .and_then(|tbl| tbl.get_column_at(*col_idx))
                .map(|c| Cow::Owned(c.ty_str.clone())),
            _ => None,
        }
    }

    pub fn parameters(&self) -> &parameters::Parameters {
        &self.program.parameters
    }

    pub fn parameters_count(&self) -> usize {
        self.program.parameters.count()
    }

    pub fn bind_at(&mut self, index: NonZero<usize>, value: Value) {
        self.state.bind_at(index, value);
    }

    pub fn reset(&mut self) {
        self.state.reset();
        self.program.n_change.set(0);
    }

    pub fn row(&self) -> Option<&Row> {
        self.state.result_row.as_ref()
    }

    pub fn explain(&self) -> String {
        self.program.explain()
    }
}

pub type Row = vdbe::Row;

pub type StepResult = vdbe::StepResult;

pub struct SymbolTable {
    pub functions: HashMap<String, Rc<function::ExternalFunc>>,
    pub vtabs: HashMap<String, Rc<VirtualTable>>,
    pub vtab_modules: HashMap<String, Rc<crate::ext::VTabImpl>>,
}

impl std::fmt::Debug for SymbolTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SymbolTable")
            .field("functions", &self.functions)
            .finish()
    }
}

fn is_shared_library(path: &std::path::Path) -> bool {
    path.extension()
        .map_or(false, |ext| ext == "so" || ext == "dylib" || ext == "dll")
}

pub fn resolve_ext_path(extpath: &str) -> Result<std::path::PathBuf> {
    let path = std::path::Path::new(extpath);
    if !path.exists() {
        if is_shared_library(path) {
            return Err(LimboError::ExtensionError(format!(
                "Extension file not found: {}",
                extpath
            )));
        };
        let maybe = path.with_extension(std::env::consts::DLL_EXTENSION);
        maybe
            .exists()
            .then_some(maybe)
            .ok_or(LimboError::ExtensionError(format!(
                "Extension file not found: {}",
                extpath
            )))
    } else {
        Ok(path.to_path_buf())
    }
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            vtabs: HashMap::new(),
            vtab_modules: HashMap::new(),
        }
    }

    pub fn resolve_function(
        &self,
        name: &str,
        _arg_count: usize,
    ) -> Option<Rc<function::ExternalFunc>> {
        self.functions.get(name).cloned()
    }
}

pub struct QueryRunner<'a> {
    parser: Parser<'a>,
    conn: &'a Arc<Connection>,
    statements: &'a [u8],
    last_offset: usize,
}

impl<'a> QueryRunner<'a> {
    pub(crate) fn new(conn: &'a Arc<Connection>, statements: &'a [u8]) -> Self {
        Self {
            parser: Parser::new(statements),
            conn,
            statements,
            last_offset: 0,
        }
    }
}

impl Iterator for QueryRunner<'_> {
    type Item = Result<Option<Statement>>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.parser.next() {
            Ok(Some(cmd)) => {
                let byte_offset_end = self.parser.offset();
                let input = str::from_utf8(&self.statements[self.last_offset..byte_offset_end])
                    .expect("invariant: statements are valid UTF-8 bytes slice") // UPSTREAM (Limbo): unwrap — needs proper error propagation
                    .trim();
                self.last_offset = byte_offset_end;
                Some(self.conn.run_cmd(cmd, &input))
            }
            Ok(None) => None,
            Err(err) => {
                self.parser.finalize();
                Some(Result::Err(LimboError::from(err)))
            }
        }
    }
}

#[cfg(test)]
mod explain_tests {
    use super::*;

    fn new_mem_conn() -> Arc<Connection> {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db = Database::open_file(io, ":memory:", false).expect("open :memory: database");
        db.connect().expect("connect to :memory: database")
    }

    fn exec(conn: &Arc<Connection>, sql: &str) {
        conn.execute(sql)
            .unwrap_or_else(|e| panic!("execute {sql:?} failed: {e:?}"));
    }

    /// Run `sql` to completion via `prepare()` + `step()` and return the sole
    /// integer column of its sole result row.
    fn scalar_i64(conn: &Arc<Connection>, sql: &str) -> i64 {
        let mut stmt = conn
            .prepare(sql)
            .unwrap_or_else(|e| panic!("prepare {sql:?} failed: {e:?}"));
        loop {
            match stmt
                .step()
                .unwrap_or_else(|e| panic!("step {sql:?} failed: {e:?}"))
            {
                StepResult::Row => {
                    let row = stmt.row().expect("row available after StepResult::Row");
                    return match row
                        .get_values()
                        .next()
                        .expect("query should return exactly one column")
                    {
                        Value::Integer(i) => *i,
                        other => panic!("expected an integer column, got {other:?}"),
                    };
                }
                StepResult::IO => stmt.run_once().expect("run_once"),
                other => panic!("expected a result row for {sql:?}, got {other:?}"),
            }
        }
    }

    /// Parse `sql` (expected to be `EXPLAIN QUERY PLAN ...`) and run it
    /// through the private `Connection::explain_query_plan` helper directly,
    /// bypassing the stdout-printing public entry points so the plan text
    /// itself can be asserted on.
    fn query_plan_text(conn: &Arc<Connection>, sql: &str) -> Result<String> {
        let mut parser = Parser::new(sql.as_bytes());
        let cmd = parser
            .next()
            .unwrap_or_else(|e| panic!("parse {sql:?} failed: {e:?}"))
            .unwrap_or_else(|| panic!("{sql:?} did not parse to a command"));
        match cmd {
            Cmd::ExplainQueryPlan(stmt) => conn.explain_query_plan(stmt),
            other => panic!("expected Cmd::ExplainQueryPlan for {sql:?}, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // EXPLAIN: bytecode listing, never a panic, never real side effects.
    // -----------------------------------------------------------------

    #[test]
    fn explain_select_via_prepare_lists_bytecode() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10), (2, 20)");

        let stmt = conn
            .prepare("EXPLAIN SELECT * FROM t")
            .expect("prepare EXPLAIN SELECT should succeed");
        let text = stmt.explain();
        assert!(
            text.contains("addr") && text.contains("opcode"),
            "explain output should have the addr/opcode header, got: {text}"
        );
        assert!(
            text.contains("Halt"),
            "explain output should list the terminating Halt opcode, got: {text}"
        );
    }

    #[test]
    fn explain_update_via_prepare_does_not_run_the_update() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10)");

        let stmt = conn
            .prepare("EXPLAIN UPDATE t SET v = 999 WHERE id = 1")
            .expect("prepare EXPLAIN UPDATE should succeed");
        let text = stmt.explain();
        assert!(
            text.contains("addr") && text.contains("opcode"),
            "explain output should have the addr/opcode header, got: {text}"
        );

        // Only `Statement::explain()` was called -- `step()` never ran --
        // so the real UPDATE must not have executed.
        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 1"), 10);
    }

    #[test]
    fn explain_delete_via_prepare_does_not_run_the_delete() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10)");

        let stmt = conn
            .prepare("EXPLAIN DELETE FROM t WHERE id = 1")
            .expect("prepare EXPLAIN DELETE should succeed");
        let text = stmt.explain();
        assert!(
            text.contains("addr") && text.contains("opcode"),
            "explain output should have the addr/opcode header, got: {text}"
        );

        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM t"), 1);
    }

    #[test]
    fn explain_via_execute_prints_bytecode_and_does_not_run_the_statement() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10)");

        conn.execute("EXPLAIN UPDATE t SET v = 999 WHERE id = 1")
            .expect("execute EXPLAIN UPDATE should succeed");

        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 1"), 10);
    }

    // -----------------------------------------------------------------
    // EXPLAIN QUERY PLAN: plan description reflects the access method.
    // -----------------------------------------------------------------

    #[test]
    fn explain_query_plan_select_reflects_scan_vs_rowid_search() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10), (2, 20)");

        let scan = query_plan_text(&conn, "EXPLAIN QUERY PLAN SELECT * FROM t")
            .expect("explain query plan (scan) should succeed");
        assert!(
            scan.contains("SCAN t"),
            "unfiltered SELECT should be a full scan, got: {scan}"
        );
        assert!(!scan.contains("SEARCH"), "got: {scan}");

        let search = query_plan_text(&conn, "EXPLAIN QUERY PLAN SELECT * FROM t WHERE id = 1")
            .expect("explain query plan (search) should succeed");
        assert!(
            search.contains("SEARCH t USING INTEGER PRIMARY KEY"),
            "rowid equality SELECT should be a rowid search, got: {search}"
        );
        assert!(!search.contains("SCAN"), "got: {search}");
    }

    #[test]
    fn explain_query_plan_update_reflects_scan_vs_rowid_search() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10), (2, 20)");

        let scan = query_plan_text(&conn, "EXPLAIN QUERY PLAN UPDATE t SET v = 0")
            .expect("explain query plan (scan) should succeed");
        assert!(
            scan.contains("UPDATE t"),
            "unfiltered UPDATE should be a full scan, got: {scan}"
        );
        assert!(!scan.contains("SEARCH"), "got: {scan}");

        let search = query_plan_text(&conn, "EXPLAIN QUERY PLAN UPDATE t SET v = 0 WHERE id = 1")
            .expect("explain query plan (search) should succeed");
        assert!(
            search.contains("SEARCH t USING INTEGER PRIMARY KEY"),
            "rowid equality UPDATE should be a rowid search, got: {search}"
        );

        // EXPLAIN QUERY PLAN must never actually run the UPDATE.
        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 1"), 10);
        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 2"), 20);
    }

    #[test]
    fn explain_query_plan_delete_reflects_scan_vs_rowid_search() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10), (2, 20)");

        let scan = query_plan_text(&conn, "EXPLAIN QUERY PLAN DELETE FROM t")
            .expect("explain query plan (scan) should succeed");
        assert!(
            scan.contains("DELETE FROM t") && !scan.contains("USING"),
            "unfiltered DELETE should be a full scan (no access-method suffix), got: {scan}"
        );

        let search = query_plan_text(&conn, "EXPLAIN QUERY PLAN DELETE FROM t WHERE id = 1")
            .expect("explain query plan (search) should succeed");
        // `DeletePlan`'s `Display` impl (translate::display) folds the access
        // method into the same "DELETE FROM ..." line rather than using a
        // separate "SEARCH" line like Select/Update plans do.
        assert!(
            search.contains("DELETE FROM t USING INTEGER PRIMARY KEY"),
            "rowid equality DELETE should be a rowid search, got: {search}"
        );

        // EXPLAIN QUERY PLAN must never actually run the DELETE.
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM t"), 2);
    }

    // -----------------------------------------------------------------
    // EXPLAIN QUERY PLAN: public entry points (query()/execute()), no panic.
    // -----------------------------------------------------------------

    #[test]
    fn explain_query_plan_select_update_delete_via_query_do_not_panic() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10)");

        for sql in [
            "EXPLAIN QUERY PLAN SELECT * FROM t WHERE id = 1",
            "EXPLAIN QUERY PLAN UPDATE t SET v = 0 WHERE id = 1",
            "EXPLAIN QUERY PLAN DELETE FROM t WHERE id = 1",
        ] {
            let result = conn
                .query(sql)
                .unwrap_or_else(|e| panic!("query {sql:?} failed: {e:?}"));
            assert!(
                result.is_none(),
                "EXPLAIN QUERY PLAN via query() should not yield a Statement: {sql:?}"
            );
        }

        // None of the above actually ran.
        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 1"), 10);
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM t"), 1);
    }

    #[test]
    fn explain_query_plan_select_update_delete_via_execute_do_not_panic() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 10)");

        for sql in [
            "EXPLAIN QUERY PLAN SELECT * FROM t WHERE id = 1",
            "EXPLAIN QUERY PLAN UPDATE t SET v = 0 WHERE id = 1",
            "EXPLAIN QUERY PLAN DELETE FROM t WHERE id = 1",
        ] {
            conn.execute(sql)
                .unwrap_or_else(|e| panic!("execute {sql:?} failed: {e:?}"));
        }

        assert_eq!(scalar_i64(&conn, "SELECT v FROM t WHERE id = 1"), 10);
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM t"), 1);
    }

    // -----------------------------------------------------------------
    // EXPLAIN QUERY PLAN INSERT / prepare(): clean errors, never a panic.
    // -----------------------------------------------------------------

    #[test]
    fn explain_query_plan_insert_is_a_clean_error_not_a_panic() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");

        match conn.query("EXPLAIN QUERY PLAN INSERT INTO t (id, v) VALUES (1, 10)") {
            Err(LimboError::ParseError(_)) => {}
            Err(other) => panic!("expected LimboError::ParseError via query(), got: {other:?}"),
            Ok(_) => panic!("expected an error via query() for EXPLAIN QUERY PLAN INSERT"),
        }

        match conn.execute("EXPLAIN QUERY PLAN INSERT INTO t (id, v) VALUES (1, 10)") {
            Err(LimboError::ParseError(_)) => {}
            Err(other) => panic!("expected LimboError::ParseError via execute(), got: {other:?}"),
            Ok(()) => panic!("expected an error via execute() for EXPLAIN QUERY PLAN INSERT"),
        }

        // Neither failed attempt inserted a row.
        assert_eq!(scalar_i64(&conn, "SELECT count(*) FROM t"), 0);
    }

    #[test]
    fn explain_query_plan_via_prepare_is_a_clean_error_not_a_panic() {
        let conn = new_mem_conn();
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)");

        // `prepare()` has no `Statement` representation for `EXPLAIN QUERY
        // PLAN` (see `Connection::prepare`); it must report a clean parse
        // error rather than panicking via the `todo!()` that used to sit
        // here.
        match conn.prepare("EXPLAIN QUERY PLAN SELECT * FROM t") {
            Err(LimboError::ParseError(_)) => {}
            Err(other) => panic!("expected LimboError::ParseError, got: {other:?}"),
            Ok(_) => panic!("prepare() should not return a Statement for EXPLAIN QUERY PLAN"),
        }
    }
}

#[cfg(test)]
mod open_from_bytes_tests {
    //! Tests for [`Database::open_from_bytes`]: the in-memory database-image
    //! open path. Fixtures are synthesized at run time (no checked-in binary
    //! blobs) — either an empty single-page image at a custom page size (built
    //! through the same bootstrap primitives as `maybe_init_database_file`) or
    //! a full image produced by the engine itself and serialized back out.
    use super::*;

    /// Build a minimal, valid, *empty* SQLite database image at `page_size`.
    ///
    /// Mirrors the page-1 bootstrap performed by `maybe_init_database_file`,
    /// but at an arbitrary (power-of-two, 512..=65536) page size so the
    /// non-default page-size open path can be exercised without relying on the
    /// (still unimplemented) `PRAGMA page_size = N` SQL path.
    fn build_empty_db_image(page_size: u32) -> Vec<u8> {
        let mut db_header = DatabaseHeader::default();
        db_header.update_page_size(page_size);

        let page1 = allocate_page(
            1,
            &Rc::new(BufferPool::new(db_header.get_page_size() as usize)),
            DATABASE_HEADER_SIZE,
        );
        let page1 = Arc::new(BTreePageInner {
            page: RefCell::new(page1),
        });
        btree_init_page(
            &page1,
            storage::sqlite3_ondisk::PageType::TableLeaf,
            DATABASE_HEADER_SIZE,
            (db_header.get_page_size() - db_header.reserved_space as u32) as u16,
        );
        let page1 = page1.get();
        let contents = page1
            .get()
            .contents
            .as_mut()
            .expect("page1 contents initialized");
        contents.write_database_header(&db_header);
        let buffer = contents.buffer.clone();
        let image = buffer.borrow().as_slice().to_vec();
        assert_eq!(image.len(), page_size as usize, "image is exactly one page");
        image
    }

    fn exec(conn: &Arc<Connection>, sql: &str) {
        conn.execute(sql)
            .unwrap_or_else(|e| panic!("execute {sql:?} failed: {e:?}"));
    }

    /// Drain `sql` and return the first integer column of the first row.
    fn scalar_i64(conn: &Arc<Connection>, sql: &str) -> i64 {
        let mut stmt = conn
            .prepare(sql)
            .unwrap_or_else(|e| panic!("prepare {sql:?} failed: {e:?}"));
        loop {
            match stmt
                .step()
                .unwrap_or_else(|e| panic!("step {sql:?} failed: {e:?}"))
            {
                StepResult::Row => {
                    let row = stmt.row().expect("row available after StepResult::Row");
                    return match row
                        .get_values()
                        .next()
                        .expect("query returns at least one column")
                    {
                        Value::Integer(i) => *i,
                        other => panic!("expected an integer column, got {other:?}"),
                    };
                }
                StepResult::IO => stmt.run_once().expect("run_once"),
                other => panic!("expected a result row for {sql:?}, got {other:?}"),
            }
        }
    }

    /// Drain `sql` and return the first text column of the first row.
    fn scalar_text(conn: &Arc<Connection>, sql: &str) -> String {
        let mut stmt = conn
            .prepare(sql)
            .unwrap_or_else(|e| panic!("prepare {sql:?} failed: {e:?}"));
        loop {
            match stmt
                .step()
                .unwrap_or_else(|e| panic!("step {sql:?} failed: {e:?}"))
            {
                StepResult::Row => {
                    let row = stmt.row().expect("row available after StepResult::Row");
                    return match row
                        .get_values()
                        .next()
                        .expect("query returns at least one column")
                    {
                        Value::Text(t) => t.to_string(),
                        other => panic!("expected a text column, got {other:?}"),
                    };
                }
                StepResult::IO => stmt.run_once().expect("run_once"),
                other => panic!("expected a result row for {sql:?}, got {other:?}"),
            }
        }
    }

    /// A fresh empty image at each supported page size must open, accept
    /// writes (including overflow-sized rows), and read back correctly. Small
    /// page sizes force overflow pages for large payloads.
    ///
    /// The 65536 page size is deliberately excluded from the *write* matrix:
    /// the engine represents a page's usable space in a `u16`, which cannot
    /// hold 65536, so any write at that page size hits a pre-existing
    /// subtract-with-overflow in `payload_overflow_threshold_max`. The
    /// read-only open of a 65536 image is covered separately below.
    #[test]
    fn test_open_from_bytes_all_page_sizes_read_write() {
        for &page_size in &[1024u32, 4096, 8192] {
            let image = build_empty_db_image(page_size);
            let db = Database::open_from_bytes(&image, false).unwrap_or_else(|e| {
                panic!("open_from_bytes at page_size {page_size} failed: {e:?}")
            });
            let conn = db.connect().expect("connect");

            exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, body TEXT)");

            // A payload larger than the page size forces overflow pages.
            let big = "x".repeat((page_size as usize) * 2 + 321);
            exec(
                &conn,
                &format!("INSERT INTO t (id, body) VALUES (1, '{big}')"),
            );
            exec(&conn, "INSERT INTO t (id, body) VALUES (2, 'short')");

            assert_eq!(
                scalar_i64(&conn, "SELECT count(*) FROM t"),
                2,
                "row count at page_size {page_size}"
            );
            assert_eq!(
                scalar_i64(&conn, "SELECT length(body) FROM t WHERE id = 1"),
                big.len() as i64,
                "overflow payload length at page_size {page_size}"
            );
            assert_eq!(
                scalar_text(&conn, "SELECT body FROM t WHERE id = 2"),
                "short",
                "plain read at page_size {page_size}"
            );

            // Index coverage requires the engine's index-maintenance feature
            // (the same one the compat consumer enables); exercise it whenever
            // it is compiled in.
            #[cfg(feature = "index_experimental")]
            {
                exec(&conn, "CREATE INDEX idx_body ON t (body)");
                assert_eq!(
                    scalar_text(&conn, "SELECT body FROM t ORDER BY body LIMIT 1"),
                    "short",
                    "index-ordered read at page_size {page_size}"
                );
            }
        }
    }

    /// Writing into a database opened from bytes must not mutate the source
    /// slice, and two opens of the same slice must be independent.
    #[test]
    fn test_open_from_bytes_source_slice_untouched_and_independent() {
        let image = build_empty_db_image(4096);
        let snapshot = image.clone();

        let db_a = Database::open_from_bytes(&image, false).expect("open a");
        let conn_a = db_a.connect().expect("connect a");
        exec(&conn_a, "CREATE TABLE t (id INTEGER PRIMARY KEY)");
        exec(&conn_a, "INSERT INTO t (id) VALUES (1), (2), (3)");
        assert_eq!(scalar_i64(&conn_a, "SELECT count(*) FROM t"), 3);

        // The source slice is unchanged by writes to db_a.
        assert_eq!(
            image, snapshot,
            "open_from_bytes must not mutate the source"
        );

        // A second open of the same slice sees none of db_a's writes.
        let db_b = Database::open_from_bytes(&image, false).expect("open b");
        let conn_b = db_b.connect().expect("connect b");
        let has_table = conn_b.prepare("SELECT count(*) FROM t").is_ok();
        assert!(
            !has_table,
            "second independent open must not see table created in the first"
        );
    }

    #[test]
    fn test_open_from_bytes_empty_is_err_not_panic() {
        assert!(matches!(
            Database::open_from_bytes(&[], false),
            Err(LimboError::NotADB)
        ));
    }

    #[test]
    fn test_open_from_bytes_truncated_header_is_err() {
        let image = build_empty_db_image(4096);
        // Fewer than DATABASE_HEADER_SIZE bytes.
        let truncated = &image[..DATABASE_HEADER_SIZE - 1];
        assert!(matches!(
            Database::open_from_bytes(truncated, false),
            Err(LimboError::NotADB)
        ));
    }

    #[test]
    fn test_open_from_bytes_bad_magic_is_err() {
        let mut image = build_empty_db_image(4096);
        image[0] = b'X';
        assert!(matches!(
            Database::open_from_bytes(&image, false),
            Err(LimboError::NotADB)
        ));
    }

    #[test]
    fn test_open_from_bytes_bad_page_size_is_err() {
        let mut image = build_empty_db_image(4096);
        // Offset 16..18 is the page size; 1000 is not a power of two.
        image[16] = 0x03;
        image[17] = 0xE8;
        assert!(matches!(
            Database::open_from_bytes(&image, false),
            Err(LimboError::Corrupt(_))
        ));
    }

    #[test]
    fn test_open_from_bytes_page_size_one_means_65536() {
        // A header page-size field of 1 encodes the 65536 maximum: it must be
        // accepted (not rejected as < MIN_PAGE_SIZE) and must open + read.
        //
        // Only the read path is exercised here: writing at a 65536 page size
        // hits a pre-existing engine limitation (usable space is a `u16` and
        // cannot represent 65536), independent of open_from_bytes.
        let image = build_empty_db_image(65536);
        assert_eq!(
            u16::from_be_bytes([image[16], image[17]]),
            1,
            "page size 65536 is encoded as 1 in the header"
        );
        let db = Database::open_from_bytes(&image, false).expect("open 65536");
        let conn = db.connect().expect("connect");
        // A fresh image has an empty schema: reading it back must work.
        assert_eq!(
            scalar_i64(&conn, "SELECT count(*) FROM sqlite_master"),
            0,
            "empty 65536 image opens and reads an empty schema"
        );
    }

    /// A non-zero `reserved_space` in the header (as real GeoPackage/OxiProj
    /// databases have, e.g. 12) must round-trip through open_from_bytes.
    #[test]
    fn test_open_from_bytes_nonzero_reserved_space() {
        let mut db_header = DatabaseHeader::default();
        db_header.update_page_size(4096);
        db_header.reserved_space = 12;

        let page1 = allocate_page(
            1,
            &Rc::new(BufferPool::new(db_header.get_page_size() as usize)),
            DATABASE_HEADER_SIZE,
        );
        let page1 = Arc::new(BTreePageInner {
            page: RefCell::new(page1),
        });
        btree_init_page(
            &page1,
            storage::sqlite3_ondisk::PageType::TableLeaf,
            DATABASE_HEADER_SIZE,
            (db_header.get_page_size() - db_header.reserved_space as u32) as u16,
        );
        let page1 = page1.get();
        let contents = page1.get().contents.as_mut().expect("page1 contents");
        contents.write_database_header(&db_header);
        let image = contents.buffer.clone().borrow().as_slice().to_vec();
        assert_eq!(image[20], 12, "reserved_space byte at offset 20");

        let db = Database::open_from_bytes(&image, false).expect("open reserved_space=12");
        let conn = db.connect().expect("connect");
        exec(&conn, "CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)");
        exec(&conn, "INSERT INTO t (id, v) VALUES (1, 'hello')");
        assert_eq!(scalar_text(&conn, "SELECT v FROM t WHERE id = 1"), "hello");
    }
}
