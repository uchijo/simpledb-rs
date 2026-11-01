use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use super::{Layout, RecordPage, Rid};
use crate::file::BlockId;
use crate::sync::lock;
use crate::tx::Transaction;

#[derive(Clone, Copy)]
enum CursorPosition {
    BeforeFirst,
    AtSlot(usize),
    AfterLast,
}

/// A table cursor. Owns one record page at a time; the transaction owns its locks.
pub struct TableScan {
    transaction: Arc<Mutex<Transaction>>,
    layout: Arc<Layout>,
    filename: String,
    page: Option<RecordPage>,
    position: CursorPosition,
}

impl TableScan {
    pub fn new(
        transaction: Arc<Mutex<Transaction>>,
        table_name: &str,
        layout: Arc<Layout>,
    ) -> io::Result<Self> {
        if layout.slot_size() > lock(&transaction)?.block_size()? {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "record does not fit in a block",
            ));
        }
        let mut scan = Self {
            transaction,
            layout,
            filename: format!("{table_name}.tbl"),
            page: None,
            position: CursorPosition::BeforeFirst,
        };
        let block_count = lock(&scan.transaction)?.size(&scan.filename)?;
        if block_count == 0 {
            scan.move_to_new_block()?;
        } else {
            scan.move_to_block(0)?;
        }
        Ok(scan)
    }

    /// Retain the page on failure so closing can be retried.
    pub fn close(&mut self) -> io::Result<()> {
        if let Some(page) = self.page.as_mut() {
            page.close()?;
        }
        self.page = None;
        self.position = CursorPosition::AfterLast;
        Ok(())
    }

    pub fn has_field(&self, field_name: &str) -> bool {
        self.layout.schema().has_field(field_name)
    }

    pub fn before_first(&mut self) -> io::Result<()> {
        self.move_to_block(0)
    }

    // Matches the book's cursor API; the current record stays in the pinned page.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> io::Result<bool> {
        self.page()?;
        if matches!(self.position, CursorPosition::AfterLast) {
            return Ok(false);
        }
        loop {
            let previous_slot = match self.position {
                CursorPosition::AtSlot(slot) => Some(slot),
                _ => None,
            };
            if let Some(slot) = self.page()?.next_after(previous_slot)? {
                self.position = CursorPosition::AtSlot(slot);
                return Ok(true);
            }
            self.position = CursorPosition::BeforeFirst;
            if self.at_last_block()? {
                self.position = CursorPosition::AfterLast;
                return Ok(false);
            }
            self.move_to_block(self.page()?.block().number() + 1)?;
        }
    }

    pub fn get_int(&self, field_name: &str) -> io::Result<i32> {
        self.page()?.get_int(self.current_slot()?, field_name)
    }

    pub fn get_string(&self, field_name: &str) -> io::Result<String> {
        self.page()?.get_string(self.current_slot()?, field_name)
    }

    pub fn set_int(&self, field_name: &str, value: i32) -> io::Result<()> {
        self.page()?
            .set_int(self.current_slot()?, field_name, value)
    }

    pub fn set_string(&self, field_name: &str, value: &str) -> io::Result<()> {
        self.page()?
            .set_string(self.current_slot()?, field_name, value)
    }

    pub fn delete(&self) -> io::Result<()> {
        self.page()?.delete(self.current_slot()?)
    }

    pub fn insert(&mut self) -> io::Result<()> {
        loop {
            let previous_slot = match self.position {
                CursorPosition::AtSlot(slot) => Some(slot),
                _ => None,
            };
            if let Some(slot) = self.page()?.insert_after(previous_slot)? {
                self.position = CursorPosition::AtSlot(slot);
                return Ok(());
            }
            self.position = CursorPosition::BeforeFirst;
            if self.at_last_block()? {
                self.move_to_new_block()?;
            } else {
                self.move_to_block(self.page()?.block().number() + 1)?;
            }
        }
    }

    pub fn current_rid(&self) -> io::Result<Rid> {
        Ok(Rid::new(
            self.page()?.block().number(),
            self.current_slot()?,
        ))
    }

    pub fn move_to_rid(&mut self, rid: Rid) -> io::Result<()> {
        let block_size = lock(&self.transaction)?.block_size()?;
        if rid.slot() >= block_size / self.layout.slot_size() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "RID slot is out of bounds",
            ));
        }
        self.move_to_block(rid.block_number())?;
        self.position = CursorPosition::AtSlot(rid.slot());
        Ok(())
    }

    fn page(&self) -> io::Result<&RecordPage> {
        self.page
            .as_ref()
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "table scan is closed"))
    }

    fn current_slot(&self) -> io::Result<usize> {
        match self.position {
            CursorPosition::AtSlot(slot) => Ok(slot),
            _ => Err(Error::new(ErrorKind::InvalidInput, "no current record")),
        }
    }

    fn move_to_block(&mut self, block_number: u64) -> io::Result<()> {
        self.close()?;
        let block = BlockId::new(&self.filename, block_number);
        self.page = Some(RecordPage::new(
            Arc::clone(&self.transaction),
            block,
            Arc::clone(&self.layout),
        )?);
        self.position = CursorPosition::BeforeFirst;
        Ok(())
    }

    fn move_to_new_block(&mut self) -> io::Result<()> {
        self.close()?;
        let block = lock(&self.transaction)?.append(&self.filename)?;
        let page = RecordPage::new(
            Arc::clone(&self.transaction),
            block,
            Arc::clone(&self.layout),
        )?;
        page.format()?;
        self.page = Some(page);
        self.position = CursorPosition::BeforeFirst;
        Ok(())
    }

    fn at_last_block(&self) -> io::Result<bool> {
        Ok(self.page()?.block().number() + 1 == lock(&self.transaction)?.size(&self.filename)?)
    }
}
