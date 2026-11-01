use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, MutexGuard};

use crate::sync::lock;

use super::{Buffer, BufferManager};

/// Keeps a block pinned until dropped. Multiple pins share the same buffer.
/// Owns a manager reference so transactions can store their pins without self-borrows.
pub struct PinnedBuffer {
    pub(super) manager: Arc<BufferManager>,
    pub(super) index: usize,
}

impl PinnedBuffer {
    /// Logging while holding this guard is allowed, as in transaction updates.
    /// Drop it before calling buffer manager methods or locking/dropping another pin.
    pub fn lock(&self) -> io::Result<BufferGuard<'_>> {
        Ok(BufferGuard {
            pool: lock(&self.manager.pool)?,
            index: self.index,
        })
    }
}

impl Drop for PinnedBuffer {
    fn drop(&mut self) {
        let mut pool = self
            .manager
            .pool
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        pool[self.index].pins -= 1;
        if !pool[self.index].is_pinned() {
            self.manager.released.notify_all();
        }
    }
}

/// Locked access to a pinned buffer. Unlocks on drop without releasing the pin.
pub struct BufferGuard<'a> {
    pool: MutexGuard<'a, Vec<Buffer>>,
    index: usize,
}

impl Deref for BufferGuard<'_> {
    type Target = Buffer;

    fn deref(&self) -> &Buffer {
        &self.pool[self.index]
    }
}

impl DerefMut for BufferGuard<'_> {
    fn deref_mut(&mut self) -> &mut Buffer {
        &mut self.pool[self.index]
    }
}
