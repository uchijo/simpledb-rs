use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Mutex};

use super::{FieldType, Layout};
use crate::file::BlockId;
use crate::sync::lock;
use crate::tx::Transaction;

const EMPTY: i32 = 0;
const USED: i32 = 1;

/// One pinned block, split into fixed-size slots. Close before ending the transaction.
pub struct RecordPage {
    transaction: Arc<Mutex<Transaction>>,
    block: BlockId,
    layout: Arc<Layout>,
    slot_count: usize,
    pinned: bool,
}

impl RecordPage {
    pub fn new(
        transaction: Arc<Mutex<Transaction>>,
        block: BlockId,
        layout: Arc<Layout>,
    ) -> io::Result<Self> {
        let mut transaction_guard = lock(&transaction)?;
        let slot_count = transaction_guard.block_size()? / layout.slot_size();
        if slot_count == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "record does not fit in a block",
            ));
        }
        transaction_guard.pin(&block)?;
        drop(transaction_guard);

        Ok(Self {
            transaction,
            block,
            layout,
            slot_count,
            pinned: true,
        })
    }

    pub fn block(&self) -> &BlockId {
        &self.block
    }

    pub fn close(&mut self) -> io::Result<()> {
        if self.pinned {
            lock(&self.transaction)?.unpin(&self.block)?;
            self.pinned = false;
        }
        Ok(())
    }

    pub fn get_int(&self, slot: usize, field_name: &str) -> io::Result<i32> {
        let offset = self.int_offset(slot, field_name)?;
        lock(&self.transaction)?.get_int(&self.block, offset)
    }

    pub fn set_int(&self, slot: usize, field_name: &str, value: i32) -> io::Result<()> {
        let offset = self.int_offset(slot, field_name)?;
        lock(&self.transaction)?.set_int(&self.block, offset, value, true)
    }

    pub fn get_string(&self, slot: usize, field_name: &str) -> io::Result<String> {
        let offset = self.string_offset(slot, field_name)?;
        lock(&self.transaction)?.get_string(&self.block, offset)
    }

    pub fn set_string(&self, slot: usize, field_name: &str, value: &str) -> io::Result<()> {
        let offset = self.string_offset(slot, field_name)?;
        let max_chars = self.layout.schema().length(field_name)?;
        if value.chars().count() > max_chars {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "string exceeds declared field length",
            ));
        }
        lock(&self.transaction)?.set_string(&self.block, offset, value, true)
    }

    /// Initialize every complete slot without logging, as in the book.
    /// Only format newly allocated blocks; rollback cannot restore their previous contents.
    pub fn format(&self) -> io::Result<()> {
        self.ensure_open()?;
        let transaction = lock(&self.transaction)?;
        for slot in 0..self.slot_count {
            let slot_offset = self.checked_slot_offset(slot)?;
            transaction.set_int(&self.block, slot_offset, EMPTY, false)?;
            for field_name in self.layout.schema().fields() {
                let field_offset = slot_offset + self.layout.offset(field_name)?;
                match self.layout.schema().field_type(field_name)? {
                    FieldType::Integer => {
                        transaction.set_int(&self.block, field_offset, 0, false)?
                    }
                    FieldType::Varchar(_) => {
                        transaction.set_string(&self.block, field_offset, "", false)?
                    }
                }
            }
        }
        Ok(())
    }

    /// None is the book's slot -1, before the first slot.
    pub fn next_after(&self, slot: Option<usize>) -> io::Result<Option<usize>> {
        self.search_after(slot, USED)
    }

    pub fn insert_after(&self, slot: Option<usize>) -> io::Result<Option<usize>> {
        let available = self.search_after(slot, EMPTY)?;
        if let Some(slot) = available {
            self.set_flag(slot, USED)?;
        }
        Ok(available)
    }

    pub fn delete(&self, slot: usize) -> io::Result<()> {
        self.set_flag(slot, EMPTY)
    }

    fn ensure_open(&self) -> io::Result<()> {
        if !self.pinned {
            return Err(Error::new(ErrorKind::InvalidInput, "record page is closed"));
        }
        Ok(())
    }

    fn checked_slot_offset(&self, slot: usize) -> io::Result<usize> {
        self.ensure_open()?;
        if slot >= self.slot_count {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "record slot is out of bounds",
            ));
        }
        Ok(slot * self.layout.slot_size())
    }

    fn int_offset(&self, slot: usize, field_name: &str) -> io::Result<usize> {
        if self.layout.schema().field_type(field_name)? != FieldType::Integer {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "expected an integer field",
            ));
        }
        Ok(self.checked_slot_offset(slot)? + self.layout.offset(field_name)?)
    }

    fn string_offset(&self, slot: usize, field_name: &str) -> io::Result<usize> {
        let FieldType::Varchar(_) = self.layout.schema().field_type(field_name)? else {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "expected a string field",
            ));
        };
        Ok(self.checked_slot_offset(slot)? + self.layout.offset(field_name)?)
    }

    fn set_flag(&self, slot: usize, flag: i32) -> io::Result<()> {
        let offset = self.checked_slot_offset(slot)?;
        lock(&self.transaction)?.set_int(&self.block, offset, flag, true)
    }

    fn search_after(
        &self,
        previous_slot: Option<usize>,
        wanted_flag: i32,
    ) -> io::Result<Option<usize>> {
        self.ensure_open()?;
        let first_slot = match previous_slot {
            Some(slot) => {
                self.checked_slot_offset(slot)?;
                slot + 1
            }
            None => 0,
        };
        let transaction = lock(&self.transaction)?;
        for slot in first_slot..self.slot_count {
            let flag = transaction.get_int(&self.block, self.checked_slot_offset(slot)?)?;
            if !matches!(flag, EMPTY | USED) {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "invalid record slot flag",
                ));
            }
            if flag == wanted_flag {
                return Ok(Some(slot));
            }
        }
        Ok(None)
    }
}

impl Drop for RecordPage {
    fn drop(&mut self) {
        // Explicit close reports errors; drop only makes a best-effort unpin.
        let _ = self.close();
    }
}
