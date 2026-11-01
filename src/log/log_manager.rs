use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use crate::file::{BlockId, FileManager, Page};
use crate::sync::lock;

use super::log_iterator::{BOUNDARY_BYTES, LogIterator, RECORD_LENGTH_BYTES, boundary};

const MIN_BLOCK_SIZE: usize = BOUNDARY_BYTES + RECORD_LENGTH_BYTES;

/// One in-memory log page. LSNs start at 1 for each manager lifetime.
/// Share through `Arc<Mutex<LogManager>>` when used by multiple threads.
pub struct LogManager {
    file_manager: Arc<Mutex<FileManager>>,
    page: Page,
    block: BlockId,
    latest_lsn: u64,
    saved_lsn: u64,
}

impl LogManager {
    pub fn new(file_manager: Arc<Mutex<FileManager>>, filename: &str) -> io::Result<Self> {
        let mut files = lock(&file_manager)?;
        let size = files.block_size();
        if !(MIN_BLOCK_SIZE..=i32::MAX as usize).contains(&size) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "invalid log block size",
            ));
        }
        let length = files.length(filename)?;
        let (block, page) = if length == 0 {
            append_new_block(&mut files, filename)?
        } else {
            let block = BlockId::new(filename, length - 1);
            let mut page = Page::new(size);
            files.read(&block, &mut page)?;
            boundary(&page)?;
            (block, page)
        };
        drop(files);
        Ok(Self {
            file_manager,
            page,
            block,
            latest_lsn: 0,
            saved_lsn: 0,
        })
    }

    pub fn append(&mut self, record: &[u8]) -> io::Result<u64> {
        let size = self.page.contents().len();
        if record.len() > size - MIN_BLOCK_SIZE {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "log record exceeds block capacity",
            ));
        }
        let next_lsn = self
            .latest_lsn
            .checked_add(1)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "LSN overflow"))?;
        let needed = RECORD_LENGTH_BYTES + record.len();
        let mut position = boundary(&self.page)?;
        if needed > position - BOUNDARY_BYTES {
            self.flush(self.latest_lsn)?;
            let mut files = lock(&self.file_manager)?;
            (self.block, self.page) = append_new_block(&mut files, self.block.filename())?;
            position = size;
        }
        position -= needed;
        self.page.set_bytes(position, record)?;
        self.page.set_int(0, position as i32)?;
        self.latest_lsn = next_lsn;
        Ok(next_lsn)
    }

    /// Persist the requested record and all preceding records. Zero is a no-op.
    pub fn flush(&mut self, lsn: u64) -> io::Result<()> {
        if lsn > self.latest_lsn {
            return Err(Error::new(ErrorKind::InvalidInput, "unknown LSN"));
        }
        if lsn > self.saved_lsn {
            lock(&self.file_manager)?.write(&self.block, &self.page)?;
            self.saved_lsn = self.latest_lsn;
        }
        Ok(())
    }

    /// Flush and iterate newest first, using one page of memory.
    pub fn iterator(&mut self) -> io::Result<LogIterator> {
        self.flush(self.latest_lsn)?;
        LogIterator::new(
            Arc::clone(&self.file_manager),
            self.block.clone(),
            self.page.clone(),
        )
    }
}

fn append_new_block(files: &mut FileManager, filename: &str) -> io::Result<(BlockId, Page)> {
    let size = files.block_size();
    let mut page = Page::new(size);
    page.set_int(0, size as i32)?;
    let block = files.append(filename)?;
    files.write(&block, &page)?;
    Ok((block, page))
}
