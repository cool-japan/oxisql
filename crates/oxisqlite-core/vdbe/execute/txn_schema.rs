#![allow(unused_variables)]
use super::super::{insn::Cookie, CommitState};
use super::super::{Program, ProgramState, Register};
use crate::error::{
    LimboError, SQLITE_CONSTRAINT_NOTNULL, SQLITE_CONSTRAINT_PRIMARYKEY, SQLITE_CONSTRAINT_TRIGGER,
};
use crate::result::LimboResult;
use crate::schema::Schema;
use crate::storage::btree::{integrity_check, IntegrityCheckError, IntegrityCheckState};
use crate::types::CursorResult;
use crate::vdbe::builder::CursorType;
use crate::{must_be_btree_cursor, MvStore, Pager, Result};
use crate::{
    storage::wal::CheckpointResult,
    types::Value,
    util::parse_schema_rows,
    vdbe::insn::{Insn, SavepointOp},
};
use crate::{OpenFlags, StepResult, TransactionState};
use std::rc::Rc;

use super::InsnFunctionStepResult;

#[derive(Debug)]
pub enum OpIntegrityCheckState {
    Start,
    Checking {
        errors: Vec<IntegrityCheckError>,
        current_root_idx: usize,
        state: IntegrityCheckState,
    },
}
pub fn op_checkpoint(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Checkpoint {
        database: _,
        checkpoint_mode,
        dest,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let result = pager.wal_checkpoint_mode(*checkpoint_mode);
    match result {
        Ok(CheckpointResult {
            num_wal_frames: num_wal_pages,
            num_checkpointed_frames: num_checkpointed_pages,
            ..
        }) => {
            state.registers[*dest] = Register::Value(Value::Integer(0));
            state.registers[*dest + 1] = Register::Value(Value::Integer(num_wal_pages as i64));
            state.registers[*dest + 2] =
                Register::Value(Value::Integer(num_checkpointed_pages as i64));
        }
        Err(_err) => state.registers[*dest] = Register::Value(Value::Integer(1)),
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn halt(
    program: &Program,
    state: &mut ProgramState,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
    err_code: usize,
    description: &str,
) -> Result<InsnFunctionStepResult> {
    if err_code > 0 {
        pager.clear_page_cache();
    }
    match err_code {
        0 => {}
        SQLITE_CONSTRAINT_PRIMARYKEY => {
            return Err(LimboError::Constraint(format!(
                "UNIQUE constraint failed: {} (19)",
                description
            )));
        }
        SQLITE_CONSTRAINT_NOTNULL => {
            return Err(LimboError::Constraint(format!(
                "NOT NULL constraint failed: {} (19)",
                description
            )));
        }
        _ => {
            return Err(LimboError::Constraint(format!(
                "undocumented halt error code {}",
                description
            )));
        }
    }
    match program.commit_txn(pager.clone(), state, mv_store)? {
        StepResult::Done => Ok(InsnFunctionStepResult::Done),
        StepResult::IO => Ok(InsnFunctionStepResult::IO),
        StepResult::Row => Ok(InsnFunctionStepResult::Row),
        StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
        StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
    }
}
pub fn op_halt(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Halt {
        err_code,
        description,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *err_code > 0 {
        pager.clear_page_cache();
    }
    match *err_code {
        0 => {}
        SQLITE_CONSTRAINT_PRIMARYKEY => {
            return Err(LimboError::Constraint(format!(
                "UNIQUE constraint failed: {} (19)",
                description
            )));
        }
        SQLITE_CONSTRAINT_NOTNULL => {
            return Err(LimboError::Constraint(format!(
                "NOTNULL constraint failed: {} (19)",
                description
            )));
        }
        // `RAISE(ABORT|FAIL|ROLLBACK, 'msg')` inside a trigger body. SQLite
        // surfaces the trigger's message verbatim, so no prefix is added.
        SQLITE_CONSTRAINT_TRIGGER => {
            return Err(LimboError::Constraint(description.clone()));
        }
        _ => {
            return Err(LimboError::Constraint(format!(
                "undocumented halt error code {}",
                description
            )));
        }
    }
    match program.commit_txn(pager.clone(), state, mv_store)? {
        StepResult::Done => Ok(InsnFunctionStepResult::Done),
        StepResult::IO => Ok(InsnFunctionStepResult::IO),
        StepResult::Row => Ok(InsnFunctionStepResult::Row),
        StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
        StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
    }
}
pub fn op_halt_if_null(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::HaltIfNull {
        target_reg,
        err_code,
        description,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if state.registers[*target_reg].get_owned_value() == &Value::Null {
        halt(program, state, pager, mv_store, *err_code, &description)
    } else {
        state.pc += 1;
        Ok(InsnFunctionStepResult::Step)
    }
}
pub fn op_transaction(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Transaction {
        write,
        schema_cookie,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if *schema_cookie != pager.db_header.lock().schema_cookie {
        return Err(LimboError::SchemaChanged);
    }
    let connection = program.connection.clone();
    if *write && connection._db.open_flags.contains(OpenFlags::ReadOnly) {
        return Err(LimboError::ReadOnly);
    }
    if let Some(mv_store) = &mv_store {
        if state.mv_tx_id.is_none() {
            let tx_id = mv_store.begin_tx();
            connection.mv_transactions.borrow_mut().push(tx_id);
            state.mv_tx_id = Some(tx_id);
        }
    } else {
        let current_state = connection.transaction_state.get();
        let (new_transaction_state, updated) = match (current_state, write) {
            (TransactionState::Write, true) => (TransactionState::Write, false),
            (TransactionState::Write, false) => (TransactionState::Write, false),
            (TransactionState::Read, true) => (TransactionState::Write, true),
            (TransactionState::Read, false) => (TransactionState::Read, false),
            (TransactionState::None, true) => (TransactionState::Write, true),
            (TransactionState::None, false) => (TransactionState::Read, true),
        };
        if updated && matches!(current_state, TransactionState::None) {
            if let LimboResult::Busy = pager.begin_read_tx()? {
                return Ok(InsnFunctionStepResult::Busy);
            }
        }
        if updated && matches!(new_transaction_state, TransactionState::Write) {
            if let LimboResult::Busy = pager.begin_write_tx()? {
                tracing::trace!("begin_write_tx busy");
                return Ok(InsnFunctionStepResult::Busy);
            }
        }
        if updated {
            connection.transaction_state.replace(new_transaction_state);
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_auto_commit(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::AutoCommit {
        auto_commit,
        rollback,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let conn = program.connection.clone();
    if state.commit_state == CommitState::Committing {
        return match program.commit_txn(pager.clone(), state, mv_store)? {
            StepResult::Done => Ok(InsnFunctionStepResult::Done),
            StepResult::IO => Ok(InsnFunctionStepResult::IO),
            StepResult::Row => Ok(InsnFunctionStepResult::Row),
            StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
            StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
        };
    }
    if *auto_commit != conn.auto_commit.get() {
        if *rollback {
            pager.rollback();
            // Auxiliary databases roll back with `main`: a statement that wrote
            // to `temp` or an attached database inside the aborted transaction
            // must not keep those writes.
            conn.rollback_aux_txns();
            conn.transaction_state.replace(TransactionState::None);
            conn.auto_commit.replace(true);
            return Ok(InsnFunctionStepResult::Done);
        } else {
            conn.auto_commit.replace(*auto_commit);
        }
    } else if !*auto_commit {
        return Err(LimboError::TxError(
            "cannot start a transaction within a transaction".to_string(),
        ));
    } else if *rollback {
        return Err(LimboError::TxError(
            "cannot rollback - no transaction is active".to_string(),
        ));
    } else {
        return Err(LimboError::TxError(
            "cannot commit - no transaction is active".to_string(),
        ));
    }
    return match program.commit_txn(pager.clone(), state, mv_store)? {
        StepResult::Done => Ok(InsnFunctionStepResult::Done),
        StepResult::IO => Ok(InsnFunctionStepResult::IO),
        StepResult::Row => Ok(InsnFunctionStepResult::Row),
        StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
        StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
    };
}
/// Execute a SAVEPOINT / RELEASE / ROLLBACK TO SAVEPOINT instruction.
///
/// # SAVEPOINT Begin
/// If in autocommit mode, eagerly starts a write transaction and opens the
/// savepoint as the "transaction owner".  Inside an explicit transaction,
/// just opens the savepoint.
///
/// # RELEASE
/// Removes the named savepoint and all nested ones.  When the released
/// savepoint is the transaction owner (autocommit-started), the surrounding
/// write transaction is committed via the same async state machine used by
/// AutoCommit.
///
/// # ROLLBACK TO
/// Prunes WAL frames appended after the savepoint, clears the page cache, and
/// resets the flush state machines.  The transaction remains active.
pub fn op_savepoint(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    if state.commit_state == CommitState::Committing {
        return match program.commit_txn(pager.clone(), state, mv_store)? {
            StepResult::Done => Ok(InsnFunctionStepResult::Done),
            StepResult::IO => Ok(InsnFunctionStepResult::IO),
            StepResult::Row => Ok(InsnFunctionStepResult::Row),
            StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
            StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
        };
    }
    let Insn::Savepoint { op, name } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let conn = program.connection.clone();
    match op {
        SavepointOp::Begin => {
            let is_autocommit = conn.auto_commit.get();
            if is_autocommit {
                let current_state = conn.transaction_state.get();
                if matches!(current_state, TransactionState::None) {
                    if let LimboResult::Busy = pager.begin_read_tx()? {
                        return Ok(InsnFunctionStepResult::Busy);
                    }
                }
                if !matches!(current_state, TransactionState::Write) {
                    if let LimboResult::Busy = pager.begin_write_tx()? {
                        return Ok(InsnFunctionStepResult::Busy);
                    }
                }
                conn.transaction_state.replace(TransactionState::Write);
                conn.auto_commit.replace(false);
                pager.open_savepoint(name.clone(), true)?;
            } else {
                pager.open_savepoint(name.clone(), false)?;
            }
            state.pc += 1;
            Ok(InsnFunctionStepResult::Step)
        }
        SavepointOp::Release => {
            let is_owner = pager.release_savepoint(name)?;
            if is_owner {
                conn.auto_commit.replace(true);
                return match program.commit_txn(pager.clone(), state, mv_store)? {
                    StepResult::Done => Ok(InsnFunctionStepResult::Done),
                    StepResult::IO => Ok(InsnFunctionStepResult::IO),
                    StepResult::Row => Ok(InsnFunctionStepResult::Row),
                    StepResult::Interrupt => Ok(InsnFunctionStepResult::Interrupt),
                    StepResult::Busy => Ok(InsnFunctionStepResult::Busy),
                };
            }
            Ok(InsnFunctionStepResult::Done)
        }
        SavepointOp::RollbackTo => {
            pager.rollback_to_savepoint(name)?;
            state.pc += 1;
            Ok(InsnFunctionStepResult::Step)
        }
    }
}
pub fn op_page_count(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::PageCount { db, dest } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let pager = &super::pager_for_db(program, pager, *db)?;
    let count = pager.db_header.lock().database_size.into();
    state.registers[*dest] = Register::Value(Value::Integer(count));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_parse_schema(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ParseSchema { db, where_clause } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let conn = program.connection.clone();
    if *db != crate::multidb::DB_MAIN {
        // `temp` / attached catalogs are re-read wholesale from their own
        // `sqlite_schema` (and re-tagged with their registry index) rather than
        // incrementally: they are small, per-connection, and the incremental
        // `where_clause` path below can only address `main`'s catalog.
        conn.reparse_aux_schema(*db, state.mv_tx_id)?;
        state.pc += 1;
        return Ok(InsnFunctionStepResult::Step);
    }
    if let Some(where_clause) = where_clause {
        let stmt = conn.prepare(format!(
            "SELECT * FROM sqlite_schema WHERE {}",
            where_clause
        ))?;
        let mut schema = conn.schema.write();
        {
            parse_schema_rows(
                Some(stmt),
                &mut schema,
                conn.pager.io.clone(),
                &conn.syms.borrow(),
                state.mv_tx_id,
            )?;
        }
    } else {
        let stmt = conn.prepare("SELECT * FROM sqlite_schema")?;
        let mut new = Schema::new();
        {
            parse_schema_rows(
                Some(stmt),
                &mut new,
                conn.pager.io.clone(),
                &conn.syms.borrow(),
                state.mv_tx_id,
            )?;
        }
        {
            let mut schema = conn.schema.write();
            *schema = new;
        }
        let has_stat1 = conn.schema.read().get_table("sqlite_stat1").is_some();
        if has_stat1 {
            let stat_stmt = conn.prepare("SELECT tbl, idx, stat FROM sqlite_stat1")?;
            let mut schema = conn.schema.write();
            schema.stats.clear();
            crate::util::load_stat1(
                Some(stat_stmt),
                &mut schema,
                conn.pager.io.clone(),
                state.mv_tx_id,
            )?;
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
/// `ATTACH DATABASE <path> AS <alias>`.
///
/// Opens the database named by `path_reg` and registers it on the connection
/// under the alias in `alias_reg`. Both operands are evaluated at run time so
/// `ATTACH ? AS ?` and expression paths behave like upstream's `OP_Function`
/// based implementation.
pub fn op_attach(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Attach {
        path_reg,
        alias_reg,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let path = value_as_db_text(&state.registers[*path_reg], "ATTACH", "filename")?;
    let alias = value_as_db_text(&state.registers[*alias_reg], "ATTACH", "schema name")?;
    program.connection.attach_db(&path, &alias)?;
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}

/// `DETACH DATABASE <alias>`.
pub fn op_detach(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::Detach { alias_reg } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let alias = value_as_db_text(&state.registers[*alias_reg], "DETACH", "schema name")?;
    program.connection.detach_db(&alias)?;
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}

/// Read an `ATTACH`/`DETACH` operand as text, with a typed error for every
/// other value kind (upstream rejects non-text names the same way).
fn value_as_db_text(register: &Register, stmt: &str, what: &str) -> Result<String> {
    match register.get_owned_value() {
        Value::Text(text) => Ok(text.as_str().to_string()),
        other => Err(LimboError::InvalidArgument(format!(
            "{stmt}: {what} must be a text value, got {}",
            other.value_type()
        ))),
    }
}

pub fn op_read_cookie(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::ReadCookie { db, dest, cookie } = insn else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let pager = &super::pager_for_db(program, pager, *db)?;
    let cookie_value = match cookie {
        Cookie::UserVersion => pager.db_header.lock().user_version.into(),
        Cookie::SchemaVersion => pager.db_header.lock().schema_cookie.into(),
        Cookie::LargestRootPageNumber => {
            pager.db_header.lock().vacuum_mode_largest_root_page.into()
        }
        Cookie::ApplicationId => pager.db_header.lock().application_id as i32 as i64,
        Cookie::DatabaseFormat => pager.db_header.lock().schema_format.into(),
        Cookie::DefaultPageCacheSize => pager.db_header.lock().default_page_cache_size.into(),
        Cookie::DatabaseTextEncoding => pager.db_header.lock().text_encoding.into(),
        Cookie::IncrementalVacuum => pager.db_header.lock().incremental_vacuum_enabled.into(),
    };
    state.registers[*dest] = Register::Value(Value::Integer(cookie_value));
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_set_cookie(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::SetCookie {
        db,
        cookie,
        value,
        p5,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    if super::begin_db_txn(program, *db, true)? {
        return Ok(InsnFunctionStepResult::Busy);
    }
    let pager = &super::pager_for_db(program, pager, *db)?;
    match cookie {
        Cookie::UserVersion => {
            let mut header_guard = pager.db_header.lock();
            header_guard.user_version = *value;
            pager.write_database_header(&*header_guard)?;
        }
        Cookie::LargestRootPageNumber => {
            let mut header_guard = pager.db_header.lock();
            header_guard.vacuum_mode_largest_root_page = *value as u32;
            pager.write_database_header(&*header_guard)?;
        }
        Cookie::IncrementalVacuum => {
            let mut header_guard = pager.db_header.lock();
            header_guard.incremental_vacuum_enabled = *value as u32;
            pager.write_database_header(&*header_guard)?;
        }
        Cookie::ApplicationId => {
            let mut header_guard = pager.db_header.lock();
            header_guard.application_id = *value as u32;
            pager.write_database_header(&*header_guard)?;
        }
        Cookie::SchemaVersion => {
            let mut header_guard = pager.db_header.lock();
            header_guard.schema_cookie = *value as u32;
            pager.write_database_header(&*header_guard)?;
        }
        cookie => {
            return Err(LimboError::InternalError(format!(
                "{cookie:?} is not yet implemented for SetCookie"
            )));
        }
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_idx_stat(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IdxStat {
        cursor_id,
        num_cols,
        dest,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    let (n, distinct) = {
        let mut cursor = must_be_btree_cursor!(*cursor_id, program.cursor_ref, state, "IdxStat");
        let cursor = cursor.as_btree_mut();
        return_if_io!(cursor.index_stat(*num_cols))
    };
    if n == 0 {
        state.registers[*dest] = Register::Value(Value::Null);
    } else {
        let mut stat = n.to_string();
        for &dist in &distinct {
            let d1 = dist + 1;
            let mut a = (n + d1 - 1) / d1;
            if a == 2 && n * 10 <= d1 * 11 {
                a = 1;
            }
            stat.push(' ');
            stat.push_str(&a.to_string());
        }
        state.registers[*dest] = Register::Value(Value::build_text(stat));
    }
    state.pc += 1;
    Ok(InsnFunctionStepResult::Step)
}
pub fn op_integrity_check(
    program: &Program,
    state: &mut ProgramState,
    insn: &Insn,
    pager: &Rc<Pager>,
    mv_store: Option<&Rc<MvStore>>,
) -> Result<InsnFunctionStepResult> {
    let Insn::IntegrityCk {
        max_errors,
        roots,
        message_register,
    } = insn
    else {
        unreachable!("unexpected Insn {:?}", insn)
    };
    match &mut state.op_integrity_check_state {
        OpIntegrityCheckState::Start => {
            state.op_integrity_check_state = OpIntegrityCheckState::Checking {
                errors: Vec::new(),
                current_root_idx: 0,
                state: IntegrityCheckState::new(roots[0]),
            };
        }
        OpIntegrityCheckState::Checking {
            errors,
            current_root_idx,
            state: integrity_check_state,
        } => {
            return_if_io!(integrity_check(integrity_check_state, errors, pager));
            *current_root_idx += 1;
            if *current_root_idx < roots.len() {
                *integrity_check_state = IntegrityCheckState::new(roots[*current_root_idx]);
                return Ok(InsnFunctionStepResult::Step);
            } else {
                let message = if errors.is_empty() {
                    "ok".to_string()
                } else {
                    errors
                        .iter()
                        .map(|e| e.to_string())
                        .collect::<Vec<String>>()
                        .join("\n")
                };
                state.registers[*message_register] = Register::Value(Value::build_text(message));
                state.op_integrity_check_state = OpIntegrityCheckState::Start;
                state.pc += 1;
            }
        }
    }
    Ok(InsnFunctionStepResult::Step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, MemoryIO, IO};
    use std::sync::Arc;

    /// Build a throwaway in-memory `Program` + `Pager` pair by preparing a
    /// trivial statement on a fresh `:memory:` connection.
    ///
    /// `op_read_cookie` never reads or writes `program` (it sources every
    /// cookie value from the `pager` argument alone), so any well-formed
    /// `Program` works here; obtaining one from a real connection is simpler
    /// and less brittle than hand-assembling the `Program` struct.
    fn program_and_pager() -> (Rc<Program>, Rc<Pager>) {
        let io: Arc<dyn IO> = Arc::new(MemoryIO::new());
        let db = Database::open_file(io, ":memory:", false).expect("open in-memory db");
        let conn = db.connect().expect("connect");
        let stmt = conn.prepare("SELECT 1").expect("prepare trivial statement");
        (stmt.program.clone(), stmt.pager.clone())
    }

    /// Invoke `op_read_cookie` directly for `cookie` and return the integer
    /// value it wrote to the destination register.
    ///
    /// This bypasses the SQL/PRAGMA layer entirely: none of the four cookies
    /// under test (`DatabaseFormat`, `DefaultPageCacheSize`,
    /// `DatabaseTextEncoding`, `IncrementalVacuum`) is reachable from a
    /// PRAGMA or other opcode-emission site yet.
    fn read_cookie_value(pager: &Rc<Pager>, program: &Program, cookie: Cookie) -> i64 {
        let mut state = ProgramState::new(1, 0);
        let insn = Insn::ReadCookie {
            db: 0,
            dest: 0,
            cookie,
        };
        op_read_cookie(program, &mut state, &insn, pager, None).expect("op_read_cookie");
        match state.registers[0].get_owned_value() {
            Value::Integer(i) => *i,
            other => panic!("expected Value::Integer, got {other:?}"),
        }
    }

    #[test]
    fn read_cookie_database_format() {
        let (program, pager) = program_and_pager();
        pager.db_header.lock().schema_format = 4;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DatabaseFormat),
            4
        );
        pager.db_header.lock().schema_format = 2;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DatabaseFormat),
            2
        );
    }

    #[test]
    fn read_cookie_default_page_cache_size() {
        let (program, pager) = program_and_pager();
        // SQLite historically stores the default cache size as a negative
        // number of KiB (see `DEFAULT_CACHE_SIZE`); make sure the sign
        // survives the round trip through the i32 header field.
        pager.db_header.lock().default_page_cache_size = -2000;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DefaultPageCacheSize),
            -2000
        );
        pager.db_header.lock().default_page_cache_size = 512;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DefaultPageCacheSize),
            512
        );
    }

    #[test]
    fn read_cookie_database_text_encoding() {
        let (program, pager) = program_and_pager();
        pager.db_header.lock().text_encoding = 1;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DatabaseTextEncoding),
            1
        );
        pager.db_header.lock().text_encoding = 3;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::DatabaseTextEncoding),
            3
        );
    }

    #[test]
    fn read_cookie_incremental_vacuum() {
        let (program, pager) = program_and_pager();
        pager.db_header.lock().incremental_vacuum_enabled = 0;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::IncrementalVacuum),
            0
        );
        pager.db_header.lock().incremental_vacuum_enabled = 1;
        assert_eq!(
            read_cookie_value(&pager, &program, Cookie::IncrementalVacuum),
            1
        );
    }
}
