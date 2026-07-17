use super::{Buffer, Clock, Completion, File, OpenFlags, IO};
use crate::Result;

use crate::io::clock::Instant;
use std::{
    cell::{Cell, RefCell, UnsafeCell},
    collections::BTreeMap,
    sync::Arc,
};
use tracing::debug;

pub struct MemoryIO {}
unsafe impl Send for MemoryIO {}

// TODO: page size flag
const PAGE_SIZE: usize = 4096;
type MemPage = Box<[u8; PAGE_SIZE]>;

impl MemoryIO {
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new() -> Self {
        debug!("Using IO backend 'memory'");
        Self {}
    }
}

impl Default for MemoryIO {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for MemoryIO {
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

impl IO for MemoryIO {
    fn open_file(&self, _path: &str, _flags: OpenFlags, _direct: bool) -> Result<Arc<dyn File>> {
        Ok(Arc::new(MemoryFile {
            pages: BTreeMap::new().into(),
            size: 0.into(),
        }))
    }

    fn run_once(&self) -> Result<()> {
        // nop
        Ok(())
    }

    fn wait_for_completion(&self, c: Arc<Completion>) -> Result<()> {
        while !c.is_completed() {
            self.run_once()?;
        }
        Ok(())
    }

    fn generate_random_number(&self) -> i64 {
        let mut buf = [0u8; 8];
        getrandom::fill(&mut buf).expect("getrandom failed");
        i64::from_ne_bytes(buf)
    }

    fn get_memory_io(&self) -> Arc<MemoryIO> {
        Arc::new(MemoryIO::new())
    }
}

pub struct MemoryFile {
    pages: UnsafeCell<BTreeMap<usize, MemPage>>,
    size: Cell<usize>,
}
unsafe impl Send for MemoryFile {}
unsafe impl Sync for MemoryFile {}

impl File for MemoryFile {
    fn lock_file(&self, _exclusive: bool) -> Result<()> {
        Ok(())
    }
    fn unlock_file(&self) -> Result<()> {
        Ok(())
    }

    fn pread(&self, pos: usize, c: Arc<Completion>) -> Result<()> {
        let r = c.as_read();
        let buf_len = r.buf().len();
        if buf_len == 0 {
            c.complete(0);
            return Ok(());
        }

        let file_size = self.size.get();
        if pos >= file_size {
            c.complete(0);
            return Ok(());
        }

        let read_len = buf_len.min(file_size - pos);
        {
            let mut read_buf = r.buf_mut();
            let mut offset = pos;
            let mut remaining = read_len;
            let mut buf_offset = 0;

            while remaining > 0 {
                let page_no = offset / PAGE_SIZE;
                let page_offset = offset % PAGE_SIZE;
                let bytes_to_read = remaining.min(PAGE_SIZE - page_offset);
                if let Some(page) = self.get_page(page_no) {
                    read_buf.as_mut_slice()[buf_offset..buf_offset + bytes_to_read]
                        .copy_from_slice(&page[page_offset..page_offset + bytes_to_read]);
                } else {
                    read_buf.as_mut_slice()[buf_offset..buf_offset + bytes_to_read].fill(0);
                }

                offset += bytes_to_read;
                buf_offset += bytes_to_read;
                remaining -= bytes_to_read;
            }
        }
        c.complete(read_len as i32);
        Ok(())
    }

    fn pwrite(&self, pos: usize, buffer: Arc<RefCell<Buffer>>, c: Arc<Completion>) -> Result<()> {
        let buf = buffer.borrow();
        let buf_len = buf.len();
        if buf_len == 0 {
            c.complete(0);
            return Ok(());
        }

        let mut offset = pos;
        let mut remaining = buf_len;
        let mut buf_offset = 0;
        let data = &buf.as_slice();

        while remaining > 0 {
            let page_no = offset / PAGE_SIZE;
            let page_offset = offset % PAGE_SIZE;
            let bytes_to_write = remaining.min(PAGE_SIZE - page_offset);

            {
                let page = self.get_or_allocate_page(page_no);
                page[page_offset..page_offset + bytes_to_write]
                    .copy_from_slice(&data[buf_offset..buf_offset + bytes_to_write]);
            }

            offset += bytes_to_write;
            buf_offset += bytes_to_write;
            remaining -= bytes_to_write;
        }

        self.size
            .set(core::cmp::max(pos + buf_len, self.size.get()));

        c.complete(buf_len as i32);
        Ok(())
    }

    fn sync(&self, c: Arc<Completion>) -> Result<()> {
        // no-op
        c.complete(0);
        Ok(())
    }

    fn size(&self) -> Result<u64> {
        Ok(self.size.get() as u64)
    }

    fn truncate(&self, len: usize, c: Arc<Completion>) -> Result<()> {
        // Drop any pages beyond the new length and shrink size.
        let pages = unsafe { &mut *self.pages.get() };
        let keep_pages = len.div_ceil(PAGE_SIZE);
        pages.retain(|&page_no, _| page_no < keep_pages);
        self.size.set(len);
        c.complete(0);
        Ok(())
    }
}

impl Drop for MemoryFile {
    fn drop(&mut self) {
        // no-op
    }
}

impl MemoryFile {
    #[allow(clippy::mut_from_ref)]
    fn get_or_allocate_page(&self, page_no: usize) -> &mut MemPage {
        unsafe {
            let pages = &mut *self.pages.get();
            pages
                .entry(page_no)
                .or_insert_with(|| Box::new([0; PAGE_SIZE]))
        }
    }

    fn get_page(&self, page_no: usize) -> Option<&MemPage> {
        unsafe { (*self.pages.get()).get(&page_no) }
    }

    /// Construct a [`MemoryFile`] preloaded with `data`, split into fixed
    /// `PAGE_SIZE` (4096-byte) storage chunks.
    ///
    /// The input is copied in verbatim: the final partial chunk is
    /// zero-padded up to `PAGE_SIZE`, but `size` is set to `data.len()`
    /// exactly (never rounded up), so [`File::size`] reports the true image
    /// length and reads past EOF still zero-fill through the existing
    /// `file_size` check in [`MemoryFile::pread`]. This storage chunking is
    /// completely independent of the SQLite logical page size recorded in the
    /// database header, so any valid page size (512..=65536) round-trips.
    ///
    /// `data` is never mutated; the returned file owns a private copy.
    pub fn from_bytes(data: &[u8]) -> Self {
        let mut pages: BTreeMap<usize, MemPage> = BTreeMap::new();
        for (page_no, chunk) in data.chunks(PAGE_SIZE).enumerate() {
            let mut page: MemPage = Box::new([0u8; PAGE_SIZE]);
            page[..chunk.len()].copy_from_slice(chunk);
            pages.insert(page_no, page);
        }
        Self {
            pages: UnsafeCell::new(pages),
            size: Cell::new(data.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WriteCompletion;
    use std::rc::Rc;

    #[test]
    fn test_wait_for_completion_returns_ok_for_already_completed_operation() {
        let io = MemoryIO::new();
        let file = io
            .open_file("test.db", OpenFlags::Create, false)
            .expect("open_file should succeed");

        let drop_fn = Rc::new(|_buf| {});
        let buf = Arc::new(RefCell::new(Buffer::allocate(8, drop_fn)));
        let write_complete = Box::new(|_| {});
        let completion = Arc::new(Completion::Write(WriteCompletion::new(write_complete)));

        // `MemoryFile::pwrite` completes the operation synchronously before
        // returning, so `completion` is already completed at this point.
        file.pwrite(0, buf, completion.clone())
            .expect("pwrite should succeed");
        assert!(completion.is_completed());

        // Regression coverage: for an already-completed operation the
        // while-loop in `wait_for_completion` must not iterate (and must
        // not hang) -- it should observe `is_completed() == true` right
        // away and return `Ok(())` promptly.
        io.wait_for_completion(completion).expect(
            "wait_for_completion should return Ok promptly for an already-completed operation",
        );
    }

    /// Read `len` bytes at `pos` back out of a [`MemoryFile`] via the public
    /// `pread` path (the same path the storage layer uses).
    fn read_back(file: &MemoryFile, pos: usize, len: usize) -> Vec<u8> {
        let drop_fn = Rc::new(|_buf| {});
        let buf = Arc::new(RefCell::new(Buffer::allocate(len, drop_fn)));
        let read_complete = Box::new(|_res| {});
        let completion = Arc::new(Completion::Read(crate::io::ReadCompletion::new(
            buf.clone(),
            read_complete,
        )));
        file.pread(pos, completion).expect("pread should succeed");
        let out = buf.borrow().as_slice().to_vec();
        out
    }

    #[test]
    fn test_from_bytes_exact_round_trip() {
        // A payload that is an exact multiple of PAGE_SIZE.
        let data: Vec<u8> = (0..PAGE_SIZE * 3).map(|i| (i % 251) as u8).collect();
        let file = MemoryFile::from_bytes(&data);
        assert_eq!(file.size().expect("size"), data.len() as u64);
        assert_eq!(read_back(&file, 0, data.len()), data);
    }

    #[test]
    fn test_from_bytes_partial_last_chunk_padding_and_size() {
        // Not a multiple of PAGE_SIZE: the last chunk is partial.
        let len = PAGE_SIZE * 2 + 123;
        let data: Vec<u8> = (0..len).map(|i| (i % 97) as u8).collect();
        let file = MemoryFile::from_bytes(&data);
        // size() must be exact, not rounded up to a chunk boundary.
        assert_eq!(file.size().expect("size"), len as u64);
        assert_eq!(read_back(&file, 0, len), data);
        // Reads past EOF zero-fill and never exceed the real size.
        let over = read_back(&file, 0, len + PAGE_SIZE);
        assert_eq!(&over[..len], &data[..]);
    }

    #[test]
    fn test_from_bytes_page_boundary_crossing_read() {
        let len = PAGE_SIZE * 2;
        let data: Vec<u8> = (0..len).map(|i| (i % 256) as u8).collect();
        let file = MemoryFile::from_bytes(&data);
        // A read that straddles the 4096-byte storage-chunk boundary.
        let start = PAGE_SIZE - 10;
        let read = read_back(&file, start, 20);
        assert_eq!(read, data[start..start + 20]);
    }

    #[test]
    fn test_from_bytes_empty() {
        let file = MemoryFile::from_bytes(&[]);
        assert_eq!(file.size().expect("size"), 0);
        // A read against an empty file returns all zeros (complete(0)).
        assert_eq!(read_back(&file, 0, 16), vec![0u8; 16]);
    }

    #[test]
    fn test_from_bytes_does_not_alias_source() {
        // Two files built from the same slice are independent copies: writing
        // into one via pwrite must not affect the other or the source slice.
        let data = vec![0xAAu8; PAGE_SIZE + 5];
        let file_a = MemoryFile::from_bytes(&data);
        let file_b = MemoryFile::from_bytes(&data);

        let drop_fn = Rc::new(|_buf| {});
        let write = vec![0x55u8; 4];
        let buf = Arc::new(RefCell::new(Buffer::allocate(write.len(), drop_fn)));
        buf.borrow_mut().as_mut_slice().copy_from_slice(&write);
        let write_complete = Box::new(|_| {});
        let completion = Arc::new(Completion::Write(WriteCompletion::new(write_complete)));
        file_a
            .pwrite(0, buf, completion)
            .expect("pwrite should succeed");

        assert_eq!(read_back(&file_a, 0, 4), write);
        // file_b is untouched.
        assert_eq!(read_back(&file_b, 0, 4), vec![0xAAu8; 4]);
        // Source slice is untouched.
        assert_eq!(&data[0..4], &[0xAAu8; 4]);
    }
}
