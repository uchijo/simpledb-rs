use std::collections::{HashMap, HashSet};
use std::io::{self, Error, ErrorKind};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::file::BlockId;
use crate::sync::lock;

// None denotes the file's end marker, which is block -1 in the book.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct Resource(pub String, pub Option<u64>);

impl From<&BlockId> for Resource {
    fn from(block: &BlockId) -> Self {
        Self(block.filename().to_owned(), Some(block.number()))
    }
}

#[derive(Default)]
struct Holders {
    readers: HashSet<u64>,
    writer: Option<u64>,
}

/// One shared lock table per database. Locks are held until commit or rollback.
#[derive(Default)]
pub struct LockTable {
    locks: Mutex<HashMap<Resource, Holders>>,
    released: Condvar,
}

impl LockTable {
    pub(super) fn acquire(&self, resource: Resource, tx: u64, exclusive: bool) -> io::Result<()> {
        let start = Instant::now();
        let mut locks = lock(&self.locks)?;
        loop {
            let holders = locks.entry(resource.clone()).or_default();
            let writer_conflicts = holders.writer.is_some_and(|writer| writer != tx);
            let readers_conflict = exclusive && holders.readers.iter().any(|&reader| reader != tx);
            if !writer_conflicts && !readers_conflict {
                if exclusive {
                    holders.writer = Some(tx);
                } else {
                    holders.readers.insert(tx);
                }
                return Ok(());
            }

            let remaining = Duration::from_secs(10).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(Error::new(
                    ErrorKind::TimedOut,
                    "transaction lock timeout; rollback required",
                ));
            }
            (locks, _) = self
                .released
                .wait_timeout(locks, remaining)
                .map_err(|_| Error::other("lock table poisoned"))?;
        }
    }

    pub(super) fn release(&self, tx: u64) -> io::Result<()> {
        lock(&self.locks)?.retain(|_, holders| {
            holders.readers.remove(&tx);
            if holders.writer == Some(tx) {
                holders.writer = None;
            }
            holders.writer.is_some() || !holders.readers.is_empty()
        });
        self.released.notify_all();
        Ok(())
    }
}
