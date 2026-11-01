use std::io::{self, Error, ErrorKind};
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::buffer::BufferManager;
use crate::file::FileManager;
use crate::log::LogManager;
use crate::metadata::MetadataManager;
use crate::sync::lock;
use crate::tx::{LockTable, Transaction};

// UTF-8 reserves 4 bytes per character: the 100-character view catalog needs 476 bytes.
const DEFAULT_BLOCK_SIZE: usize = 512;
const DEFAULT_BUFFER_COUNT: usize = 8;
const LOG_FILENAME: &str = "simpledb.log";

/// Owns the shared managers for one database. Open each directory only once.
pub struct SimpleDb {
    file_manager: Arc<Mutex<FileManager>>,
    log_manager: Arc<Mutex<LogManager>>,
    buffer_manager: Arc<BufferManager>,
    lock_table: Arc<LockTable>,
    metadata_manager: Arc<Mutex<MetadataManager>>,
}

impl SimpleDb {
    /// Open with 512-byte blocks and 8 buffers; recover before loading the catalogs.
    pub fn new(directory: impl AsRef<Path>) -> io::Result<Self> {
        Self::with_options(directory, DEFAULT_BLOCK_SIZE, DEFAULT_BUFFER_COUNT)
    }

    /// Use a new directory or one already initialized by this API, with the same
    /// block size on every open. Chapter 6 files have no catalogs to load.
    pub fn with_options(
        directory: impl AsRef<Path>,
        block_size: usize,
        buffer_count: usize,
    ) -> io::Result<Self> {
        MetadataManager::check_block_size(block_size)?;
        LogManager::check_block_size(block_size)?;
        if buffer_count == 0 {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "buffer pool must not be empty",
            ));
        }
        let file_manager = Arc::new(Mutex::new(FileManager::new(directory, block_size)?));
        let is_new = lock(&file_manager)?.is_new();
        let log_manager = Arc::new(Mutex::new(LogManager::new(
            Arc::clone(&file_manager),
            LOG_FILENAME,
        )?));
        let buffer_manager = Arc::new(BufferManager::new(
            Arc::clone(&file_manager),
            Arc::clone(&log_manager),
            buffer_count,
        )?);
        let lock_table = Arc::new(LockTable::default());
        let transaction = Arc::new(Mutex::new(Transaction::new(
            Arc::clone(&file_manager),
            Arc::clone(&log_manager),
            Arc::clone(&buffer_manager),
            Arc::clone(&lock_table),
        )?));
        if !is_new {
            lock(&transaction)?.recover()?;
        }
        let metadata_manager = match MetadataManager::new(is_new, Arc::clone(&transaction)) {
            Ok(manager) => manager,
            Err(error) => {
                lock(&transaction)?.rollback()?;
                return Err(error);
            }
        };
        lock(&transaction)?.commit()?;
        Ok(Self {
            file_manager,
            log_manager,
            buffer_manager,
            lock_table,
            metadata_manager: Arc::new(Mutex::new(metadata_manager)),
        })
    }

    pub fn new_transaction(&self) -> io::Result<Arc<Mutex<Transaction>>> {
        Ok(Arc::new(Mutex::new(Transaction::new(
            Arc::clone(&self.file_manager),
            Arc::clone(&self.log_manager),
            Arc::clone(&self.buffer_manager),
            Arc::clone(&self.lock_table),
        )?)))
    }

    pub fn metadata_manager(&self) -> Arc<Mutex<MetadataManager>> {
        Arc::clone(&self.metadata_manager)
    }
}
