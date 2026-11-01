use std::collections::HashSet;
use std::io;

use super::Transaction;
use super::log_record::{LogRecord, Value};
use crate::sync::lock;

impl Transaction {
    pub fn commit(&mut self) -> io::Result<()> {
        self.check_active()?;
        self.finish(LogRecord::Commit(self.number))
    }

    pub fn rollback(&mut self) -> io::Result<()> {
        self.check_active()?;
        let records = lock(&self.log)?.iterator()?;
        for bytes in records {
            let record = LogRecord::decode(&bytes?)?;
            if record.tx() == Some(self.number) {
                if matches!(record, LogRecord::Start(_)) {
                    break;
                }
                self.undo(record)?;
            }
        }
        self.finish(LogRecord::Rollback(self.number))
    }

    /// Startup only: undo unfinished transactions up to the last checkpoint.
    pub fn recover(&mut self) -> io::Result<()> {
        self.check_active()?;
        self.buffers.flush_all(self.number)?;
        let records = lock(&self.log)?.iterator()?;
        let mut finished = HashSet::new();
        for bytes in records {
            let record = LogRecord::decode(&bytes?)?;
            match record {
                LogRecord::Checkpoint => break,
                LogRecord::Commit(tx) | LogRecord::Rollback(tx) => {
                    finished.insert(tx);
                }
                record => {
                    if !record.tx().is_some_and(|tx| finished.contains(&tx)) {
                        self.undo(record)?;
                    }
                }
            }
        }
        self.buffers.flush_all(self.number)?;
        let lsn = self.append_record(LogRecord::Checkpoint)?;
        lock(&self.log)?.flush(lsn)
    }

    fn undo(&mut self, record: LogRecord) -> io::Result<()> {
        if let LogRecord::Set {
            block,
            offset,
            value,
            ..
        } = record
        {
            self.pin(&block)?;
            let result = match value {
                Value::Int(value) => self.set_int(&block, offset, value, false),
                Value::String(value) => self.set_string(&block, offset, &value, false),
            };
            self.unpin(&block)?;
            result?;
        }
        Ok(())
    }
}
