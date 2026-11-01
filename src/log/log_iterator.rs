use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use crate::file::{BlockId, FileManager, Page};
use crate::sync::lock;

pub(super) const BOUNDARY_OFFSET: usize = 0;
pub(super) const BOUNDARY_BYTES: usize = size_of::<i32>();
pub(super) const RECORD_LENGTH_BYTES: usize = size_of::<i32>();

// The first integer points to the newest record; records grow toward the page start.
pub(super) fn read_boundary(page: &Page) -> io::Result<usize> {
    let next_record_offset = usize::try_from(page.get_int(BOUNDARY_OFFSET)?)
        .map_err(|_| Error::new(ErrorKind::InvalidData, "negative log boundary"))?;
    if !(BOUNDARY_BYTES..=page.contents().len()).contains(&next_record_offset) {
        return Err(Error::new(
            ErrorKind::InvalidData,
            "log boundary exceeds page bounds",
        ));
    }
    Ok(next_record_offset)
}

pub struct LogIterator {
    file_manager: Arc<Mutex<FileManager>>,
    block: BlockId,
    page: Page,
    next_record_offset: usize,
    done: bool,
}

impl LogIterator {
    pub(super) fn new(
        file_manager: Arc<Mutex<FileManager>>,
        block: BlockId,
        page: Page,
    ) -> io::Result<Self> {
        let next_record_offset = read_boundary(&page)?;
        Ok(Self {
            file_manager,
            block,
            page,
            next_record_offset,
            done: false,
        })
    }

    fn next_record(&mut self) -> io::Result<Option<Vec<u8>>> {
        while self.next_record_offset == self.page.contents().len() {
            if self.block.number() == 0 {
                return Ok(None);
            }
            self.block = BlockId::new(self.block.filename(), self.block.number() - 1);
            lock(&self.file_manager)?.read(&self.block, &mut self.page)?;
            self.next_record_offset = read_boundary(&self.page)?;
        }
        let record = self
            .page
            .get_bytes(self.next_record_offset)
            .map_err(|error| Error::new(ErrorKind::InvalidData, error))?
            .to_vec();
        self.next_record_offset += RECORD_LENGTH_BYTES + record.len();
        Ok(Some(record))
    }
}

impl Iterator for LogIterator {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        match self.next_record() {
            Ok(Some(record)) => Some(Ok(record)),
            Ok(None) => {
                self.done = true;
                None
            }
            Err(error) => {
                self.done = true;
                Some(Err(error))
            }
        }
    }
}
