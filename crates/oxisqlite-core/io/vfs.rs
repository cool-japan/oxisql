use super::{Buffer, Completion, File, MemoryIO, OpenFlags, IO};
use crate::ext::VfsMod;
use crate::io::clock::{Clock, Instant};
use crate::{LimboError, Result};
use limbo_ext::{VfsFileImpl, VfsImpl};
use std::cell::RefCell;
use std::ffi::{c_void, CString};
use std::sync::Arc;

impl Clock for VfsMod {
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

impl IO for VfsMod {
    fn open_file(&self, path: &str, flags: OpenFlags, direct: bool) -> Result<Arc<dyn File>> {
        let c_path = CString::new(path).map_err(|_| {
            LimboError::ExtensionError("Failed to convert path to CString".to_string())
        })?;
        let ctx = self.ctx as *mut c_void;
        let vfs = unsafe { &*self.ctx };
        let file = unsafe { (vfs.open)(ctx, c_path.as_ptr(), flags.0, direct) };
        if file.is_null() {
            return Err(LimboError::ExtensionError("File not found".to_string()));
        }
        Ok(Arc::new(limbo_ext::VfsFileImpl::new(file, self.ctx)?))
    }

    fn run_once(&self) -> Result<()> {
        if self.ctx.is_null() {
            return Err(LimboError::ExtensionError("VFS is null".to_string()));
        }
        let vfs = unsafe { &*self.ctx };
        let result = unsafe { (vfs.run_once)(vfs.vfs) };
        if !result.is_ok() {
            return Err(LimboError::ExtensionError(result.to_string()));
        }
        Ok(())
    }

    fn wait_for_completion(&self, c: Arc<Completion>) -> Result<()> {
        while !c.is_completed() {
            self.run_once()?;
        }
        Ok(())
    }

    fn generate_random_number(&self) -> i64 {
        if self.ctx.is_null() {
            return -1;
        }
        let vfs = unsafe { &*self.ctx };
        unsafe { (vfs.gen_random_number)() }
    }

    fn get_memory_io(&self) -> Arc<MemoryIO> {
        Arc::new(MemoryIO::new())
    }
}

impl VfsMod {
    #[allow(dead_code)] // used in FFI call
    fn get_current_time(&self) -> String {
        if self.ctx.is_null() {
            return "".to_string();
        }
        unsafe {
            let vfs = &*self.ctx;
            let chars = (vfs.current_time)();
            let cstr = CString::from_raw(chars as *mut _);
            cstr.to_string_lossy().into_owned()
        }
    }
}

impl File for VfsFileImpl {
    fn lock_file(&self, exclusive: bool) -> Result<()> {
        let vfs = unsafe { &*self.vfs };
        let result = unsafe { (vfs.lock)(self.file, exclusive) };
        if result.is_ok() {
            return Err(LimboError::ExtensionError(result.to_string()));
        }
        Ok(())
    }

    fn unlock_file(&self) -> Result<()> {
        if self.vfs.is_null() {
            return Err(LimboError::ExtensionError("VFS is null".to_string()));
        }
        let vfs = unsafe { &*self.vfs };
        let result = unsafe { (vfs.unlock)(self.file) };
        if result.is_ok() {
            return Err(LimboError::ExtensionError(result.to_string()));
        }
        Ok(())
    }

    fn pread(&self, pos: usize, c: Arc<Completion>) -> Result<()> {
        let r = match &*c {
            Completion::Read(ref r) => r,
            _ => unreachable!(),
        };
        let result = {
            let mut buf = r.buf_mut();
            let count = buf.len();
            let vfs = unsafe { &*self.vfs };
            unsafe { (vfs.read)(self.file, buf.as_mut_ptr(), count, pos as i64) }
        };
        if result < 0 {
            Err(LimboError::ExtensionError("pread failed".to_string()))
        } else {
            c.complete(result);
            Ok(())
        }
    }

    fn pwrite(&self, pos: usize, buffer: Arc<RefCell<Buffer>>, c: Arc<Completion>) -> Result<()> {
        let buf = buffer.borrow();
        let count = buf.as_slice().len();
        if self.vfs.is_null() {
            return Err(LimboError::ExtensionError("VFS is null".to_string()));
        }
        let vfs = unsafe { &*self.vfs };
        let result = unsafe {
            (vfs.write)(
                self.file,
                buf.as_slice().as_ptr() as *mut u8,
                count,
                pos as i64,
            )
        };

        if result < 0 {
            Err(LimboError::ExtensionError("pwrite failed".to_string()))
        } else {
            c.complete(result);
            Ok(())
        }
    }

    fn sync(&self, c: Arc<Completion>) -> Result<()> {
        let vfs = unsafe { &*self.vfs };
        let result = unsafe { (vfs.sync)(self.file) };
        if result < 0 {
            Err(LimboError::ExtensionError("sync failed".to_string()))
        } else {
            c.complete(0);
            Ok(())
        }
    }

    fn size(&self) -> Result<u64> {
        let vfs = unsafe { &*self.vfs };
        let result = unsafe { (vfs.size)(self.file) };
        if result < 0 {
            Err(LimboError::ExtensionError("size failed".to_string()))
        } else {
            Ok(result as u64)
        }
    }

    fn truncate(&self, _len: usize, c: Arc<Completion>) -> Result<()> {
        // External VFS plugins do not expose a truncate hook in the current ABI;
        // completing without shrinking is safe because the WAL header rewrite
        // already invalidates leftover frames via salt mismatch.
        c.complete(0);
        Ok(())
    }
}

impl Drop for VfsMod {
    fn drop(&mut self) {
        if self.ctx.is_null() {
            return;
        }
        unsafe {
            let _ = Box::from_raw(self.ctx as *mut VfsImpl);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteCompletion;
    use limbo_ext::ResultCode;
    use std::ffi::c_char;
    use std::rc::Rc;

    /// Sentinel "file handle" returned by [`stub_open`]. None of the stub
    /// callbacks below ever dereference it, so any non-null value is safe
    /// to hand back across this fake FFI boundary.
    const FAKE_FILE_HANDLE: usize = 0xdead_beef;

    extern "C" fn stub_open(
        _ctx: *const c_void,
        _path: *const c_char,
        _flags: i32,
        _direct: bool,
    ) -> *const c_void {
        FAKE_FILE_HANDLE as *const c_void
    }

    extern "C" fn stub_close(_file: *const c_void) -> ResultCode {
        ResultCode::OK
    }

    extern "C" fn stub_read(
        _file: *const c_void,
        _buf: *mut u8,
        count: usize,
        _offset: i64,
    ) -> i32 {
        count as i32
    }

    extern "C" fn stub_write(
        _file: *const c_void,
        _buf: *const u8,
        count: usize,
        _offset: i64,
    ) -> i32 {
        count as i32
    }

    extern "C" fn stub_sync(_file: *const c_void) -> i32 {
        0
    }

    extern "C" fn stub_lock(_file: *const c_void, _exclusive: bool) -> ResultCode {
        ResultCode::OK
    }

    extern "C" fn stub_unlock(_file: *const c_void) -> ResultCode {
        ResultCode::OK
    }

    extern "C" fn stub_size(_file: *const c_void) -> i64 {
        0
    }

    extern "C" fn stub_run_once(_vfs: *const c_void) -> ResultCode {
        ResultCode::OK
    }

    extern "C" fn stub_current_time() -> *const c_char {
        std::ptr::null()
    }

    extern "C" fn stub_gen_random_number() -> i64 {
        0
    }

    /// Builds a `VfsMod` around an all-stub `VfsImpl` vtable, mirroring how
    /// `register_vfs`/`add_builtin_vfs_extensions` build one from a real
    /// extension's FFI vtable (see `ext/dynamic.rs`). Ownership of the boxed
    /// `VfsImpl` transfers to the returned `VfsMod`, which frees it on drop
    /// (see `impl Drop for VfsMod` above).
    fn make_test_vfs_mod() -> VfsMod {
        let vfs_impl = Box::new(VfsImpl {
            name: std::ptr::null(),
            vfs: std::ptr::null(),
            open: stub_open,
            close: stub_close,
            read: stub_read,
            write: stub_write,
            sync: stub_sync,
            lock: stub_lock,
            unlock: stub_unlock,
            size: stub_size,
            run_once: stub_run_once,
            current_time: stub_current_time,
            gen_random_number: stub_gen_random_number,
        });
        VfsMod {
            ctx: Box::into_raw(vfs_impl),
        }
    }

    #[test]
    fn test_wait_for_completion_returns_ok_for_already_completed_operation() {
        let vfs_mod = make_test_vfs_mod();
        let file = vfs_mod
            .open_file("test-file", OpenFlags::Create, false)
            .expect("open_file should succeed against the stub VFS");

        let drop_fn = Rc::new(|_buf| {});
        let buf = Arc::new(RefCell::new(Buffer::allocate(8, drop_fn)));
        let write_complete = Box::new(|_| {});
        let completion = Arc::new(Completion::Write(WriteCompletion::new(write_complete)));

        // `VfsFileImpl::pwrite` completes the operation synchronously before
        // returning, so `completion` is already completed at this point.
        file.pwrite(0, buf, completion.clone())
            .expect("pwrite should succeed");
        assert!(completion.is_completed());

        // Regression coverage: for an already-completed operation the
        // while-loop in `wait_for_completion` must not iterate (and must
        // not hang) -- it should observe `is_completed() == true` right
        // away and return `Ok(())` promptly.
        vfs_mod.wait_for_completion(completion).expect(
            "wait_for_completion should return Ok promptly for an already-completed operation",
        );
    }
}
