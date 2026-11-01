use std::collections::{HashMap, HashSet};
use std::io::{self, Error, ErrorKind};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::file::BlockId;
use crate::sync::lock;

const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) enum LockTarget {
    Block(BlockId),
    // The book uses a dummy block numbered -1 for the file's end marker.
    FileEnd(String),
}

pub(super) enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Default)]
struct LockHolders {
    readers: HashSet<u64>,
    writer: Option<u64>,
}

/// One shared lock table per database. Locks are held until commit or rollback.
#[derive(Default)]
pub struct LockTable {
    locks: Mutex<HashMap<LockTarget, LockHolders>>,
    released: Condvar,
}

impl LockTable {
    pub(super) fn acquire(
        &self,
        target: LockTarget,
        transaction_id: u64,
        mode: LockMode,
    ) -> io::Result<()> {
        let wait_started = Instant::now();
        let mut locks = lock(&self.locks)?;
        loop {
            let holders = locks.entry(target.clone()).or_default();
            let writer_conflicts = holders
                .writer
                .is_some_and(|writer| writer != transaction_id);
            let readers_conflict = matches!(mode, LockMode::Exclusive)
                && holders
                    .readers
                    .iter()
                    .any(|&reader| reader != transaction_id);
            if !writer_conflicts && !readers_conflict {
                match mode {
                    LockMode::Exclusive => holders.writer = Some(transaction_id),
                    LockMode::Shared => {
                        holders.readers.insert(transaction_id);
                    }
                }
                return Ok(());
            }

            let remaining_wait = LOCK_WAIT_TIMEOUT.saturating_sub(wait_started.elapsed());
            if remaining_wait.is_zero() {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "transaction lock timeout; rollback required",
                ));
            }
            (locks, _) = self
                .released
                .wait_timeout(locks, remaining_wait)
                .map_err(|_| Error::other("lock table poisoned"))?;
        }
    }

    pub(super) fn release(&self, transaction_id: u64) -> io::Result<()> {
        lock(&self.locks)?.retain(|_, holders| {
            holders.readers.remove(&transaction_id);
            if holders.writer == Some(transaction_id) {
                holders.writer = None;
            }
            holders.writer.is_some() || !holders.readers.is_empty()
        });
        self.released.notify_all();
        Ok(())
    }
}
