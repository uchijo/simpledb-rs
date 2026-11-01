use std::collections::HashSet;
use std::io;

use super::Transaction;
use super::log_record::{LogRecord, Value};
use crate::sync::lock;

impl Transaction {
    pub fn commit(&mut self) -> io::Result<()> {
        self.check_active()?;
        self.finish(LogRecord::Commit(self.transaction_id))
    }

    pub fn rollback(&mut self) -> io::Result<()> {
        self.check_active()?;
        let records = lock(&self.log_manager)?.iterator()?;
        for bytes in records {
            let record = LogRecord::decode(&bytes?)?;
            if record.transaction_id() != Some(self.transaction_id) {
                continue;
            }
            if matches!(record, LogRecord::Start(_)) {
                break;
            }
            self.undo(record)?;
        }
        self.finish(LogRecord::Rollback(self.transaction_id))
    }

    /// Startup only: undo unfinished transactions up to the last checkpoint.
    pub fn recover(&mut self) -> io::Result<()> {
        self.check_active()?;
        self.buffer_manager.flush_all(self.transaction_id)?;
        let records = lock(&self.log_manager)?.iterator()?;
        let mut finished_transactions = HashSet::new();
        for bytes in records {
            let record = LogRecord::decode(&bytes?)?;
            match record {
                LogRecord::Checkpoint => break,
                LogRecord::Commit(transaction_id) | LogRecord::Rollback(transaction_id) => {
                    finished_transactions.insert(transaction_id);
                }
                LogRecord::Set { transaction_id, .. } => {
                    if finished_transactions.contains(&transaction_id) {
                        continue;
                    }
                    self.undo(record)?;
                }
                LogRecord::Start(_) => {}
            }
        }
        self.buffer_manager.flush_all(self.transaction_id)?;
        let lsn = self.append_record(LogRecord::Checkpoint)?;
        lock(&self.log_manager)?.flush(lsn)
    }

    fn undo(&mut self, record: LogRecord) -> io::Result<()> {
        let LogRecord::Set {
            block,
            offset,
            old_value,
            ..
        } = record
        else {
            return Ok(());
        };

        self.pin(&block)?;
        let undo_result = match old_value {
            Value::Int(value) => self.set_int(&block, offset, value, false),
            Value::String(value) => self.set_string(&block, offset, &value, false),
        };
        // Release the temporary pin even when restoring the value failed.
        self.unpin(&block)?;
        undo_result?;
        Ok(())
    }
}
