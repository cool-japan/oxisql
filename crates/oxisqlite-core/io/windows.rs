use super::MemoryIO;
use crate::{Clock, Completion, File, Instant, LimboError, OpenFlags, Result, IO};
use std::cell::RefCell;
use std::io::{Read, Seek, Write};
use std::os::windows::io::AsRawHandle;
use std::sync::Arc;
use tracing::{debug, trace};
use windows_sys::Win32::Foundation::{ERROR_LOCK_VIOLATION, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    LockFileEx, UnlockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
pub struct WindowsIO {}

impl WindowsIO {
    pub fn new() -> Result<Self> {
        debug!("Using IO backend 'syscall'");
        Ok(Self {})
    }
}

unsafe impl Send for WindowsIO {}
unsafe impl Sync for WindowsIO {}

impl IO for WindowsIO {
    fn open_file(&self, path: &str, flags: OpenFlags, direct: bool) -> Result<Arc<dyn File>> {
        trace!("open_file(path = {})", path);
        let mut file = std::fs::File::options();
        file.read(true);

        if !flags.contains(OpenFlags::ReadOnly) {
            file.write(true);
            file.create(flags.contains(OpenFlags::Create));
        }

        let file = file.open(path)?;
        Ok(Arc::new(WindowsFile {
            file: RefCell::new(file),
        }))
    }

    fn wait_for_completion(&self, c: Arc<Completion>) -> Result<()> {
        while !c.is_completed() {
            self.run_once()?;
        }
        Ok(())
    }

    fn run_once(&self) -> Result<()> {
        Ok(())
    }

    fn generate_random_number(&self) -> i64 {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).unwrap();
        i64::from_ne_bytes(buf)
    }

    fn get_memory_io(&self) -> Arc<MemoryIO> {
        Arc::new(MemoryIO::new())
    }
}

impl Clock for WindowsIO {
    fn now(&self) -> Instant {
        // UTC, not Local: `timestamp()`/`timestamp_subsec_micros()` return
        // the same absolute Unix instant regardless of timezone, so there is
        // no need for the host OS's local-timezone database here.
        let now = chrono::Utc::now();
        Instant {
            secs: now.timestamp(),
            micros: now.timestamp_subsec_micros(),
        }
    }
}

pub struct WindowsFile {
    file: RefCell<std::fs::File>,
}

unsafe impl Send for WindowsFile {}
unsafe impl Sync for WindowsFile {}

impl File for WindowsFile {
    /// Acquires a whole-file advisory lock via `LockFileEx`.
    ///
    /// This mirrors the whole-file, always-non-blocking contract of the Unix
    /// backend's `fcntl_lock`-based `lock_file` (see `io/unix.rs`): the
    /// trait exposes no blocking variant, so `LOCKFILE_FAIL_IMMEDIATELY` is
    /// always set, and `exclusive` selects `LOCKFILE_EXCLUSIVE_LOCK` for a
    /// write lock vs. a shared (read) lock otherwise, exactly as `exclusive`
    /// selects between `FlockOperation::NonBlockingLockExclusive` and
    /// `NonBlockingLockShared` on Unix.
    fn lock_file(&self, exclusive: bool) -> Result<()> {
        let file = self.file.borrow();
        let handle = file.as_raw_handle() as HANDLE;

        let flags = if exclusive {
            LOCKFILE_FAIL_IMMEDIATELY | LOCKFILE_EXCLUSIVE_LOCK
        } else {
            LOCKFILE_FAIL_IMMEDIATELY
        };

        // Whole-file range starting at offset 0 (the zeroed `OVERLAPPED`
        // below): `u32::MAX` in both the low and high halves is the standard
        // Win32 idiom for "lock as much of the file as can ever be
        // addressed" (there is no dedicated "to EOF" sentinel as there is
        // for POSIX `fcntl`'s `l_len == 0`, which is what `unix.rs` relies on
        // for the same whole-file granularity). This is the same idiom used
        // by e.g. the `fs2`/`fs4` crates' Windows backends.
        let mut overlapped = OVERLAPPED::default();

        // SAFETY:
        // - `handle` is derived from `file`, a live `Ref<std::fs::File>`
        //   borrowed from `self.file` for this entire function body, so the
        //   underlying Win32 HANDLE remains open and valid for the full
        //   duration of this call.
        // - The handle was opened with at least `GENERIC_READ` by
        //   `WindowsIO::open_file` (it always calls `.read(true)`),
        //   satisfying `LockFileEx`'s documented access-right precondition.
        // - `overlapped` is a uniquely-owned, zero-initialized (via its
        //   `Default` impl, not a raw `mem::zeroed`) `OVERLAPPED` living on
        //   this stack frame for the duration of the call, satisfying the
        //   `[in, out] LPOVERLAPPED` contract; its zeroed `hEvent` is
        //   documented as an accepted value ("initialize hEvent to a valid
        //   handle or zero") for the synchronous use we make here.
        // - `std::fs::File` is not opened with `FILE_FLAG_OVERLAPPED`, and
        //   `LOCKFILE_FAIL_IMMEDIATELY` is always set, so this call is
        //   synchronous and returns immediately (no `ERROR_IO_PENDING` /
        //   `GetOverlappedResult` path is reachable here).
        let succeeded =
            unsafe { LockFileEx(handle, flags, 0, u32::MAX, u32::MAX, &mut overlapped) };

        if succeeded == 0 {
            let io_error = std::io::Error::last_os_error();
            let message = if io_error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
                "Failed locking file. File is locked by another process".to_string()
            } else {
                format!("Failed locking file, {}", io_error)
            };
            return Err(LimboError::LockingError(message));
        }

        Ok(())
    }

    /// Releases the whole-file lock taken out by `lock_file`, via
    /// `UnlockFileEx` over the exact same byte range that was locked. Win32
    /// requires this: "The region to unlock must correspond exactly to an
    /// existing locked region."
    fn unlock_file(&self) -> Result<()> {
        let file = self.file.borrow();
        let handle = file.as_raw_handle() as HANDLE;

        let mut overlapped = OVERLAPPED::default();

        // SAFETY: see `lock_file` above -- the same handle-validity and
        // `OVERLAPPED`-lifetime argument applies unchanged: `handle` stays
        // valid because `file` (the borrowed `Ref`) is alive for the whole
        // function body, and `overlapped` is a uniquely-owned,
        // zero-initialized, stack-local `OVERLAPPED` valid for the duration
        // of this call.
        let succeeded = unsafe { UnlockFileEx(handle, 0, u32::MAX, u32::MAX, &mut overlapped) };

        if succeeded == 0 {
            let io_error = std::io::Error::last_os_error();
            return Err(LimboError::LockingError(format!(
                "Failed to release file lock: {}",
                io_error
            )));
        }

        Ok(())
    }

    fn pread(&self, pos: usize, c: Arc<Completion>) -> Result<()> {
        let mut file = self.file.borrow_mut();
        file.seek(std::io::SeekFrom::Start(pos as u64))?;
        {
            let r = c.as_read();
            let mut buf = r.buf_mut();
            let buf = buf.as_mut_slice();
            file.read_exact(buf)?;
        }
        c.complete(0);
        Ok(())
    }

    fn pwrite(
        &self,
        pos: usize,
        buffer: Arc<RefCell<crate::Buffer>>,
        c: Arc<Completion>,
    ) -> Result<()> {
        let mut file = self.file.borrow_mut();
        file.seek(std::io::SeekFrom::Start(pos as u64))?;
        let buf = buffer.borrow();
        let buf = buf.as_slice();
        file.write_all(buf)?;
        c.complete(buffer.borrow().len() as i32);
        Ok(())
    }

    fn sync(&self, c: Arc<Completion>) -> Result<()> {
        let file = self.file.borrow_mut();
        file.sync_all().map_err(LimboError::IOError)?;
        c.complete(0);
        Ok(())
    }

    fn size(&self) -> Result<u64> {
        let file = self.file.borrow();
        Ok(file.metadata().unwrap().len())
    }

    fn truncate(&self, len: usize, c: Arc<Completion>) -> Result<()> {
        let file = self.file.borrow_mut();
        file.set_len(len as u64).map_err(LimboError::IOError)?;
        c.complete(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Opens `path` through the public `IO`/`File` trait surface (the same
    /// path the rest of the engine uses), producing a fresh, independent
    /// Win32 `HANDLE` on every call. `std::fs::OpenOptions` defaults to
    /// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` on Windows,
    /// so opening the same path more than once concurrently (as these tests
    /// do, to simulate two independent lock holders within a single test
    /// process) does not itself fail with a sharing violation.
    fn open_handle(io: &WindowsIO, path: &std::path::Path) -> Arc<dyn File> {
        let path = path.to_str().expect("temp file path must be valid UTF-8");
        io.open_file(path, OpenFlags::None, false)
            .expect("failed to open file")
    }

    #[test]
    fn test_exclusive_lock_blocks_second_handle_non_blocking() {
        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let io = WindowsIO::new().expect("failed to create WindowsIO");

        let file1 = open_handle(&io, tmp.path());
        let file2 = open_handle(&io, tmp.path());

        file1
            .lock_file(true)
            .expect("first exclusive lock should succeed");

        // Per LockFileEx's documented semantics, if the locking process
        // opens the file a second time it cannot access the locked region
        // through the second handle until the first handle unlocks -- and
        // with `LOCKFILE_FAIL_IMMEDIATELY` always set, this must fail
        // immediately rather than block.
        assert!(
            file2.lock_file(true).is_err(),
            "a second exclusive lock from another handle must fail non-blockingly \
             while the first handle's exclusive lock is held"
        );

        file1
            .unlock_file()
            .expect("releasing the first exclusive lock should succeed");
    }

    #[test]
    fn test_lock_then_unlock_allows_reacquire() {
        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let io = WindowsIO::new().expect("failed to create WindowsIO");

        let file1 = open_handle(&io, tmp.path());
        let file2 = open_handle(&io, tmp.path());

        file1
            .lock_file(true)
            .expect("first exclusive lock should succeed");
        file1
            .unlock_file()
            .expect("releasing the lock should succeed");

        // Now that file1 released its lock, a different handle must be able
        // to acquire a fresh exclusive lock over the same (whole-file)
        // range.
        file2
            .lock_file(true)
            .expect("re-acquiring the lock via a different handle should succeed after unlock");
        file2
            .unlock_file()
            .expect("releasing the second lock should succeed");
    }

    #[test]
    fn test_shared_locks_do_not_conflict_with_each_other() {
        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let io = WindowsIO::new().expect("failed to create WindowsIO");

        let file1 = open_handle(&io, tmp.path());
        let file2 = open_handle(&io, tmp.path());

        file1
            .lock_file(false)
            .expect("first shared lock should succeed");
        // Shared locks can overlap a locked region provided the existing
        // lock(s) held on it are also shared.
        file2
            .lock_file(false)
            .expect("a second shared lock from another handle should also succeed");

        file1
            .unlock_file()
            .expect("releasing the first shared lock should succeed");
        file2
            .unlock_file()
            .expect("releasing the second shared lock should succeed");
    }

    #[test]
    fn test_shared_lock_blocks_exclusive_lock_from_other_handle() {
        let tmp = NamedTempFile::new().expect("failed to create temp file");
        let io = WindowsIO::new().expect("failed to create WindowsIO");

        let file1 = open_handle(&io, tmp.path());
        let file2 = open_handle(&io, tmp.path());

        file1.lock_file(false).expect("shared lock should succeed");

        assert!(
            file2.lock_file(true).is_err(),
            "an exclusive lock from another handle must fail non-blockingly \
             while a shared lock is held"
        );

        file1
            .unlock_file()
            .expect("releasing the shared lock should succeed");

        // Once the shared lock is released, an exclusive lock from the
        // other handle must now succeed.
        file2
            .lock_file(true)
            .expect("exclusive lock should succeed once the shared lock is released");
        file2
            .unlock_file()
            .expect("releasing the exclusive lock should succeed");
    }
}
