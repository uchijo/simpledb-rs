use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use super::concurrency::{LockTable, Resource};
use super::log_record::{LogRecord, Value};
use crate::buffer::{BufferManager, PinnedBuffer};
use crate::file::{BlockId, FileManager};
use crate::log::LogManager;
use crate::sync::lock;

/// Chapter 5 transaction. Share the same managers and lock table within a database.
/// Call commit or rollback explicitly, including rollback after a timeout.
/// Run recover at startup before admitting other transactions.
pub struct Transaction {
    pub(super) files: Arc<Mutex<FileManager>>,
    pub(super) log: Arc<Mutex<LogManager>>,
    pub(super) buffers: Arc<BufferManager>,
    locks: Arc<LockTable>,
    pub(super) number: u64,
    pins: HashMap<BlockId, Vec<PinnedBuffer>>,
    active: bool,
}

impl Transaction {
    pub fn new(
        files: Arc<Mutex<FileManager>>,
        log: Arc<Mutex<LogManager>>,
        buffers: Arc<BufferManager>,
        locks: Arc<LockTable>,
    ) -> io::Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let number = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |number| {
                number.checked_add(1)
            })
            .map_err(|_| Error::other("transaction number overflow"))?;
        let tx = Self {
            files,
            log,
            buffers,
            locks,
            number,
            pins: HashMap::new(),
            active: true,
        };
        tx.append_record(LogRecord::Start(number))?;
        Ok(tx)
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

    pub fn pin(&mut self, block: &BlockId) -> io::Result<()> {
        self.check_active()?;
        let pin = self.buffers.pin(block)?;
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

    fn buffer(&self, block: &BlockId) -> io::Result<&PinnedBuffer> {
        self.check_active()?;
        self.pins
            .get(block)
            .and_then(|pins| pins.last())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "block is not pinned"))
    }

    pub fn get_int(&self, block: &BlockId, offset: usize) -> io::Result<i32> {
        let pin = self.buffer(block)?;
        self.locks.acquire(block.into(), self.number, false)?;
        pin.lock()?.contents().get_int(offset)
    }

    pub fn get_string(&self, block: &BlockId, offset: usize) -> io::Result<String> {
        let pin = self.buffer(block)?;
        self.locks.acquire(block.into(), self.number, false)?;
        Ok(pin.lock()?.contents().get_string(offset)?.to_owned())
    }

    pub fn set_int(
        &self,
        block: &BlockId,
        offset: usize,
        value: i32,
        ok_to_log: bool,
    ) -> io::Result<()> {
        self.set(block, offset, Value::Int(value), ok_to_log)
    }

    pub fn set_string(
        &self,
        block: &BlockId,
        offset: usize,
        value: &str,
        ok_to_log: bool,
    ) -> io::Result<()> {
        self.set(block, offset, Value::String(value.to_owned()), ok_to_log)
    }

    fn set(&self, block: &BlockId, offset: usize, value: Value, ok_to_log: bool) -> io::Result<()> {
        let pin = self.buffer(block)?;
        self.locks.acquire(block.into(), self.number, true)?;
        let mut buffer = pin.lock()?;
        let payload_size = match &value {
            Value::Int(_) => 0,
            Value::String(value) => value.len(),
        };
        if offset
            .checked_add(4)
            .and_then(|offset| offset.checked_add(payload_size))
            .is_none_or(|end| end > buffer.contents().contents().len())
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "value exceeds page bounds",
            ));
        }

        let lsn = if ok_to_log {
            let old_value = match &value {
                Value::Int(_) => Value::Int(buffer.contents().get_int(offset)?),
                Value::String(_) => Value::String(buffer.contents().get_string(offset)?.to_owned()),
            };
            Some(self.append_record(LogRecord::Set {
                tx: self.number,
                block: block.clone(),
                offset,
                value: old_value,
            })?)
        } else {
            None
        };

        match value {
            Value::Int(value) => buffer.contents_mut().set_int(offset, value)?,
            Value::String(value) => buffer.contents_mut().set_string(offset, &value)?,
        }
        buffer.set_modified(self.number, lsn);
        Ok(())
    }

    pub fn size(&self, filename: &str) -> io::Result<u64> {
        self.check_active()?;
        self.locks
            .acquire(Resource(filename.to_owned(), None), self.number, false)?;
        lock(&self.files)?.length(filename)
    }

    pub fn append(&self, filename: &str) -> io::Result<BlockId> {
        self.check_active()?;
        self.locks
            .acquire(Resource(filename.to_owned(), None), self.number, true)?;
        lock(&self.files)?.append(filename)
    }

    pub fn block_size(&self) -> io::Result<usize> {
        Ok(lock(&self.files)?.block_size())
    }
    pub fn available_buffs(&self) -> io::Result<usize> {
        self.buffers.available()
    }

    pub(super) fn append_record(&self, record: LogRecord) -> io::Result<u64> {
        lock(&self.log)?.append(&record.encode()?)
    }

    pub(super) fn finish(&mut self, record: LogRecord) -> io::Result<()> {
        self.buffers.flush_all(self.number)?;
        let lsn = self.append_record(record)?;
        lock(&self.log)?.flush(lsn)?;
        self.locks.release(self.number)?;
        self.pins.clear();
        self.active = false;
        Ok(())
    }
}
