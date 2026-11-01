use std::io;
use std::sync::Mutex;

use crate::file::{BlockId, FileManager, Page};
use crate::log::LogManager;
use crate::sync::lock;

pub struct Buffer {
    contents: Page,
    block: Option<BlockId>,
    pub(super) pin_count: usize,
    modifying_transaction_id: Option<u64>,
    last_update_lsn: Option<u64>,
}

impl Buffer {
    pub(super) fn new(block_size: usize) -> Self {
        Self {
            contents: Page::new(block_size),
            block: None,
            pin_count: 0,
            modifying_transaction_id: None,
            last_update_lsn: None,
        }
    }

    pub fn contents(&self) -> &Page {
        &self.contents
    }

    pub fn contents_mut(&mut self) -> &mut Page {
        &mut self.contents
    }

    pub fn block(&self) -> Option<&BlockId> {
        self.block.as_ref()
    }

    pub fn is_pinned(&self) -> bool {
        self.pin_count > 0
    }

    pub fn modifying_tx(&self) -> Option<u64> {
        self.modifying_transaction_id
    }

    /// Call after changing the page. `None` means this update has no log record.
    pub fn set_modified(&mut self, transaction_id: u64, lsn: Option<u64>) {
        self.modifying_transaction_id = Some(transaction_id);
        if let Some(lsn) = lsn {
            self.last_update_lsn = Some(
                self.last_update_lsn
                    .map_or(lsn, |previous| previous.max(lsn)),
            );
        }
    }

    pub(super) fn assign_to_block(
        &mut self,
        block: &BlockId,
        file_manager: &Mutex<FileManager>,
        log_manager: &Mutex<LogManager>,
    ) -> io::Result<()> {
        self.flush(file_manager, log_manager)?;
        // A failed read can partially change the page, so invalidate the old block first.
        self.block = None;
        lock(file_manager)?.read(block, &mut self.contents)?;
        self.block = Some(block.clone());
        Ok(())
    }

    pub(super) fn flush(
        &mut self,
        file_manager: &Mutex<FileManager>,
        log_manager: &Mutex<LogManager>,
    ) -> io::Result<()> {
        if self.modifying_transaction_id.is_none() {
            return Ok(());
        }

        // Write-ahead logging: the log must reach disk before the data page.
        if let Some(lsn) = self.last_update_lsn {
            lock(log_manager)?.flush(lsn)?;
        }
        lock(file_manager)?.write(
            self.block.as_ref().expect("dirty buffer has a block"),
            &self.contents,
        )?;

        self.modifying_transaction_id = None;
        self.last_update_lsn = None;
        Ok(())
    }
}
