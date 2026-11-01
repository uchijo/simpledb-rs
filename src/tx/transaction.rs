use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use super::concurrency::{LockMode, LockTable, LockTarget};
use super::log_record::{LogRecord, Value};
use crate::buffer::{BufferManager, PinnedBuffer};
use crate::file::{BlockId, FileManager};
use crate::log::LogManager;
use crate::sync::lock;

/// Chapter 5 transaction. Share the same managers and lock table within a database.
/// Call commit or rollback explicitly, including rollback after a timeout.
/// Dropping an active transaction releases its pins but leaves transaction locks held.
/// Run recover at startup before admitting other transactions.
pub struct Transaction {
    pub(super) file_manager: Arc<Mutex<FileManager>>,
    pub(super) log_manager: Arc<Mutex<LogManager>>,
    pub(super) buffer_manager: Arc<BufferManager>,
    lock_table: Arc<LockTable>,
    pub(super) transaction_id: u64,
    pins: HashMap<BlockId, Vec<PinnedBuffer>>,
    active: bool,
}

impl Transaction {
    pub fn new(
        file_manager: Arc<Mutex<FileManager>>,
        log_manager: Arc<Mutex<LogManager>>,
        buffer_manager: Arc<BufferManager>,
        lock_table: Arc<LockTable>,
    ) -> io::Result<Self> {
        static NEXT_TRANSACTION_ID: AtomicU64 = AtomicU64::new(1);
        let transaction_id = NEXT_TRANSACTION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |transaction_id| {
                transaction_id.checked_add(1)
            })
            .map_err(|_| Error::other("transaction number overflow"))?;
        let transaction = Self {
            file_manager,
            log_manager,
            buffer_manager,
            lock_table,
            transaction_id,
            pins: HashMap::new(),
            active: true,
        };
        transaction.append_record(LogRecord::Start(transaction_id))?;
        Ok(transaction)
    }

    pub fn pin(&mut self, block: &BlockId) -> io::Result<()> {
        self.check_active()?;
        let pin = self.buffer_manager.pin(block)?;
        self.pins.entry(block.clone()).or_default().push(pin);
        Ok(())
    }

    pub fn unpin(&mut self, block: &BlockId) -> io::Result<()> {
        self.check_active()?;
        let pins = self
            .pins
            .get_mut(block)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "block is not pinned"))?;
        pins.pop();
        if pins.is_empty() {
            self.pins.remove(block);
        }
        Ok(())
    }

    pub fn get_int(&self, block: &BlockId, offset: usize) -> io::Result<i32> {
        let pin = self.buffer(block)?;
        self.lock_table.acquire(
            LockTarget::Block(block.clone()),
            self.transaction_id,
            LockMode::Shared,
        )?;
        pin.lock()?.contents().get_int(offset)
    }

    pub fn get_string(&self, block: &BlockId, offset: usize) -> io::Result<String> {
        let pin = self.buffer(block)?;
        self.lock_table.acquire(
            LockTarget::Block(block.clone()),
            self.transaction_id,
            LockMode::Shared,
        )?;
        Ok(pin.lock()?.contents().get_string(offset)?.to_owned())
    }

    /// Set a value, logging its old value when `ok_to_log` is true.
    /// Use false only for initialization or restoring an undo value, as in the book.
    pub fn set_int(
        &self,
        block: &BlockId,
        offset: usize,
        value: i32,
        ok_to_log: bool,
    ) -> io::Result<()> {
        let pin = self.buffer(block)?;
        self.lock_table.acquire(
            LockTarget::Block(block.clone()),
            self.transaction_id,
            LockMode::Exclusive,
        )?;
        let mut buffer = pin.lock()?;
        buffer.contents().checked_range(offset, size_of::<i32>())?;

        let lsn = if ok_to_log {
            let old_value = Value::Int(buffer.contents().get_int(offset)?);
            Some(self.log_update(block, offset, old_value)?)
        } else {
            None
        };

        buffer.contents_mut().set_int(offset, value)?;
        buffer.set_modified(self.transaction_id, lsn);
        Ok(())
    }

    /// Set a string. `ok_to_log` has the same meaning as in `Self::set_int`.
    pub fn set_string(
        &self,
        block: &BlockId,
        offset: usize,
        value: &str,
        ok_to_log: bool,
    ) -> io::Result<()> {
        let pin = self.buffer(block)?;
        self.lock_table.acquire(
            LockTarget::Block(block.clone()),
            self.transaction_id,
            LockMode::Exclusive,
        )?;
        let mut buffer = pin.lock()?;
        let storage_size = value
            .len()
            .checked_add(size_of::<i32>())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "string size overflow"))?;
        buffer.contents().checked_range(offset, storage_size)?;

        let lsn = if ok_to_log {
            let old_value = Value::String(buffer.contents().get_string(offset)?.to_owned());
            Some(self.log_update(block, offset, old_value)?)
        } else {
            None
        };

        buffer.contents_mut().set_string(offset, value)?;
        buffer.set_modified(self.transaction_id, lsn);
        Ok(())
    }

    pub fn size(&self, filename: &str) -> io::Result<u64> {
        self.check_active()?;
        self.lock_table.acquire(
            LockTarget::FileEnd(filename.to_owned()),
            self.transaction_id,
            LockMode::Shared,
        )?;
        lock(&self.file_manager)?.length(filename)
    }

    pub fn append(&self, filename: &str) -> io::Result<BlockId> {
        self.check_active()?;
        self.lock_table.acquire(
            LockTarget::FileEnd(filename.to_owned()),
            self.transaction_id,
            LockMode::Exclusive,
        )?;
        lock(&self.file_manager)?.append(filename)
    }

    pub fn block_size(&self) -> io::Result<usize> {
        Ok(lock(&self.file_manager)?.block_size())
    }

    pub fn available_buffs(&self) -> io::Result<usize> {
        self.buffer_manager.available()
    }

    pub(super) fn append_record(&self, record: LogRecord) -> io::Result<u64> {
        lock(&self.log_manager)?.append(&record.encode()?)
    }

    pub(super) fn finish(&mut self, record: LogRecord) -> io::Result<()> {
        self.buffer_manager.flush_all(self.transaction_id)?;
        let lsn = self.append_record(record)?;
        lock(&self.log_manager)?.flush(lsn)?;
        self.lock_table.release(self.transaction_id)?;
        self.pins.clear();
        self.active = false;
        Ok(())
    }

    pub(super) fn check_active(&self) -> io::Result<()> {
        if !self.active {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "transaction already finished",
            ));
        }
        Ok(())
    }

    fn buffer(&self, block: &BlockId) -> io::Result<&PinnedBuffer> {
        self.check_active()?;
        self.pins
            .get(block)
            .and_then(|pins| pins.last())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "block is not pinned"))
    }

    fn log_update(&self, block: &BlockId, offset: usize, old_value: Value) -> io::Result<u64> {
        self.append_record(LogRecord::Set {
            transaction_id: self.transaction_id,
            block: block.clone(),
            offset,
            old_value,
        })
    }
}
