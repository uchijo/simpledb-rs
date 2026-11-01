use std::io;
use std::sync::{Mutex, MutexGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> io::Result<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| io::Error::other("database lock poisoned"))
}
