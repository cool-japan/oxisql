use std::fmt::Debug;

use crate::mvcc::database::{LogRecord, Result};
use crate::mvcc::errors::DatabaseError;

#[derive(Debug)]
pub enum Storage {
    Noop,
    /// Test-only backend whose `log_tx` can be toggled to fail on demand. This
    /// exists to deterministically exercise `MvStore::commit_tx`'s persist-before-
    /// visible/persist-before-remove ordering: see the `commit_tx_*` durability
    /// tests in `mvcc::database::tests`.
    #[cfg(test)]
    Flaky(std::sync::Arc<std::sync::atomic::AtomicBool>),
}

impl Storage {
    pub fn new_noop() -> Self {
        Self::Noop
    }

    /// Test-only: a storage backend whose `log_tx` fails with an I/O error for as
    /// long as `should_fail` holds `true` at call time, and succeeds (without
    /// actually persisting anything) otherwise. The caller keeps the `Arc` to
    /// flip the flag and simulate a transient persistence failure followed by a
    /// successful retry.
    #[cfg(test)]
    pub fn new_flaky(should_fail: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        Self::Flaky(should_fail)
    }
}

impl Storage {
    pub fn log_tx(&self, _m: LogRecord) -> Result<()> {
        match self {
            Self::Noop => (),
            #[cfg(test)]
            Self::Flaky(should_fail) => {
                if should_fail.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err(DatabaseError::Io("injected log_tx failure".to_string()));
                }
            }
        }
        Ok(())
    }

    pub fn read_tx_log(&self) -> Result<Vec<LogRecord>> {
        match self {
            Self::Noop => Err(DatabaseError::Io(
                "cannot read from Noop storage".to_string(),
            )),
            #[cfg(test)]
            Self::Flaky(_) => Err(DatabaseError::Io(
                "cannot read from Flaky storage".to_string(),
            )),
        }
    }
}
