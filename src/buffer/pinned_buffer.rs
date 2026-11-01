use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, MutexGuard};

use crate::sync::lock;

use super::{Buffer, BufferManager};

/// Keeps a block pinned until dropped. Multiple pins share the same buffer.
/// Owns a manager reference so transactions can store their pins without self-borrows.
pub struct PinnedBuffer {
    pub(super) manager: Arc<BufferManager>,
    pub(super) buffer_index: usize,
}

impl PinnedBuffer {
    /// Logging while holding this guard is allowed, as in transaction updates.
    /// Drop it before calling buffer manager methods or locking/dropping another pin.
    pub fn lock(&self) -> io::Result<BufferGuard<'_>> {
        Ok(BufferGuard {
            pool: lock(&self.manager.pool)?,
            buffer_index: self.buffer_index,
        })
    }
}

impl Drop for PinnedBuffer {
    fn drop(&mut self) {
        // Only release the pin; normal access still rejects the poisoned pool.
        let mut pool = self
            .manager
            .pool
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pool[self.buffer_index].pin_count -= 1;
        if !pool[self.buffer_index].is_pinned() {
            self.manager.released.notify_all();
        }
    }
}

/// Locked access to a pinned buffer. Unlocks on drop without releasing the pin.
pub struct BufferGuard<'a> {
    pool: MutexGuard<'a, Vec<Buffer>>,
    buffer_index: usize,
}

impl Deref for BufferGuard<'_> {
    type Target = Buffer;

    fn deref(&self) -> &Buffer {
        &self.pool[self.buffer_index]
    }
}

impl DerefMut for BufferGuard<'_> {
    fn deref_mut(&mut self) -> &mut Buffer {
        &mut self.pool[self.buffer_index]
    }
}
