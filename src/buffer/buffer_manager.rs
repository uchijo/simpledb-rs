use std::io::{self, Error, ErrorKind};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::file::{BlockId, FileManager};
use crate::log::LogManager;
use crate::sync::lock;

use super::{Buffer, PinnedBuffer};

/// Fixed-size pool with first-unpinned replacement and a ten-second pin timeout.
/// Share through `Arc` so transactions can own their pins independently.
/// Lock order is buffer pool, log manager, then file manager.
pub struct BufferManager {
    file_manager: Arc<Mutex<FileManager>>,
    log_manager: Arc<Mutex<LogManager>>,
    // shortcut: one lock serializes page access; split per buffer when parallel page work is needed.
    pub(super) pool: Mutex<Vec<Buffer>>,
    pub(super) released: Condvar,
}

impl BufferManager {
    pub fn new(
        file_manager: Arc<Mutex<FileManager>>,
        log_manager: Arc<Mutex<LogManager>>,
        buffer_count: usize,
    ) -> io::Result<Self> {
        if buffer_count == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "buffer pool must not be empty",
            ));
        }
        let size = lock(&file_manager)?.block_size();
        let pool = (0..buffer_count).map(|_| Buffer::new(size)).collect();
        Ok(Self {
            file_manager,
            log_manager,
            pool: Mutex::new(pool),
            released: Condvar::new(),
        })
    }

    pub fn available(&self) -> io::Result<usize> {
        let pool = lock(&self.pool)?;
        Ok(pool.iter().filter(|buffer| !buffer.is_pinned()).count())
    }

    pub fn flush_all(&self, transaction: u64) -> io::Result<()> {
        let mut pool = lock(&self.pool)?;
        for buffer in pool
            .iter_mut()
            .filter(|buffer| buffer.modifying_tx() == Some(transaction))
        {
            buffer.flush(&self.file_manager, &self.log_manager)?;
        }
        Ok(())
    }

    pub fn pin(self: &Arc<Self>, block: &BlockId) -> io::Result<PinnedBuffer> {
        let start = Instant::now();
        let mut pool = lock(&self.pool)?;
        loop {
            let index = pool
                .iter()
                .position(|buffer| buffer.block() == Some(block))
                .or_else(|| pool.iter().position(|buffer| !buffer.is_pinned()));
            if let Some(index) = index {
                let buffer = &mut pool[index];
                if buffer.block() != Some(block) {
                    buffer.assign_to_block(block, &self.file_manager, &self.log_manager)?;
                }
                buffer.pins += 1;
                return Ok(PinnedBuffer {
                    manager: Arc::clone(self),
                    index,
                });
            }
            let remaining = Duration::from_secs(10).saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(Error::new(ErrorKind::TimedOut, "no available buffer"));
            }
            (pool, _) = self
                .released
                .wait_timeout(pool, remaining)
                .map_err(|_| Error::other("buffer pool poisoned"))?;
        }
    }
}
