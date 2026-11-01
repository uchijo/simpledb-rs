//! SimpleDB chapters 3–5: files, logs, buffers, and transactions.
//!
//! Use one file manager, log manager, and buffer manager per database. Pins are
//! released on drop; dirty pages are written on replacement or `flush_all`.
//! LSNs belong to the current log manager lifetime, as in the book.
//! `set_modified` uses `None` for the book's LSN -1 (an unlogged update).
//! Allocate blocks with `FileManager::append` before pinning them.
//!
//! Transaction updates can log the old value while holding a buffer guard.
//! Lock order is buffer pool → log manager → file manager. Release the log lock
//! before undoing records through buffers; the log iterator does not hold it.
//!
//! ```no_run
//! use std::io;
//! use std::sync::{Arc, Mutex};
//! use simpledb_rs::file::{FileManager, Page};
//! use simpledb_rs::log::LogManager;
//! use simpledb_rs::buffer::BufferManager;
//!
//! # fn main() -> io::Result<()> {
//! let fm = Arc::new(Mutex::new(FileManager::new("example-db", 400)?));
//! let lm = Arc::new(Mutex::new(LogManager::new(Arc::clone(&fm), "simpledb.log")?));
//! let buffers = Arc::new(BufferManager::new(Arc::clone(&fm), Arc::clone(&lm), 3)?);
//! let block = fm.lock().unwrap().append("test.tbl")?;
//!
//! let pin = buffers.pin(&block)?;
//! {
//!     let mut buffer = pin.lock()?;
//!     let mut record = Page::new(4);
//!     record.set_int(0, buffer.contents().get_int(80)?)?;
//!     let lsn = lm.lock().unwrap().append(&record.into_bytes())?;
//!     buffer.contents_mut().set_int(80, 345)?;
//!     buffer.set_modified(1, Some(lsn));
//! }
//! drop(pin); // unpin without writing the dirty page yet
//! buffers.flush_all(1)?; // force the log first, then the data page
//! for record in lm.lock().unwrap().iterator()? {
//!     println!("{}", Page::from_bytes(record?).get_int(0)?);
//! }
//! # Ok(())
//! # }
//! ```

pub mod buffer;
pub mod file;
pub mod log;
mod sync;
pub mod tx;
