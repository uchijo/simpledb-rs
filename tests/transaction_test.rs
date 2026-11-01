mod support;

use std::io::{self, ErrorKind};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use simpledb_rs::buffer::BufferManager;
use simpledb_rs::file::{BlockId, FileManager};
use simpledb_rs::log::LogManager;
use simpledb_rs::tx::{LockTable, Transaction};
use support::TestDirectory;

const BLOCK_SIZE: usize = 400;
const BUFFER_COUNT: usize = 2;
const NUMBER_OFFSET: usize = 0;
const TEXT_OFFSET: usize = 40;

struct Database {
    file_manager: Arc<Mutex<FileManager>>,
    log_manager: Arc<Mutex<LogManager>>,
    buffer_manager: Arc<BufferManager>,
    lock_table: Arc<LockTable>,
}

impl Database {
    fn open(directory: &TestDirectory) -> io::Result<Self> {
        let file_manager = Arc::new(Mutex::new(FileManager::new(directory.path(), BLOCK_SIZE)?));
        let log_manager = Arc::new(Mutex::new(LogManager::new(
            Arc::clone(&file_manager),
            "simpledb.log",
        )?));
        let buffer_manager = Arc::new(BufferManager::new(
            Arc::clone(&file_manager),
            Arc::clone(&log_manager),
            BUFFER_COUNT,
        )?);
        Ok(Self {
            file_manager,
            log_manager,
            buffer_manager,
            lock_table: Arc::new(LockTable::default()),
        })
    }

    fn begin_transaction(&self) -> io::Result<Transaction> {
        Transaction::new(
            Arc::clone(&self.file_manager),
            Arc::clone(&self.log_manager),
            Arc::clone(&self.buffer_manager),
            Arc::clone(&self.lock_table),
        )
    }

    fn create_initialized_block(&self) -> io::Result<BlockId> {
        let mut transaction = self.begin_transaction()?;
        let block = transaction.append("test.tbl")?;
        transaction.pin(&block)?;
        transaction.set_int(&block, NUMBER_OFFSET, 10, false)?;
        // Formatting must not interpret the previous bytes as a string.
        transaction.set_int(&block, TEXT_OFFSET, -1, false)?;
        transaction.set_string(&block, TEXT_OFFSET, "元の値", false)?;
        transaction.commit()?;
        Ok(block)
    }
}

#[test]
fn commit_persists_and_rollback_undoes_repeated_updates() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = Database::open(&directory)?;
    let block = database.create_initialized_block()?;
    let mut transaction = database.begin_transaction()?;
    transaction.pin(&block)?;
    transaction.pin(&block)?;
    transaction.unpin(&block)?;

    transaction.set_int(&block, NUMBER_OFFSET, 20, true)?;
    transaction.set_int(&block, NUMBER_OFFSET, 30, true)?;
    transaction.set_string(&block, TEXT_OFFSET, "変更🦀", true)?;
    transaction.rollback()?;

    assert_eq!(database.buffer_manager.available()?, BUFFER_COUNT);
    assert_eq!(
        transaction
            .get_int(&block, NUMBER_OFFSET)
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );

    let mut transaction = database.begin_transaction()?;
    transaction.pin(&block)?;
    assert_eq!(transaction.get_int(&block, NUMBER_OFFSET)?, 10);
    assert_eq!(transaction.get_string(&block, TEXT_OFFSET)?, "元の値");

    transaction.set_int(&block, NUMBER_OFFSET, 99, true)?;
    transaction.commit()?;
    drop(database);

    let database = Database::open(&directory)?;
    let mut transaction = database.begin_transaction()?;
    transaction.recover()?;
    transaction.pin(&block)?;
    assert_eq!(transaction.get_int(&block, NUMBER_OFFSET)?, 99);
    transaction.commit()?;

    Ok(())
}

#[test]
fn restart_undoes_flushed_uncommitted_data_and_recovery_is_repeatable() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = Database::open(&directory)?;
    let block = database.create_initialized_block()?;
    let mut transaction = database.begin_transaction()?;
    transaction.pin(&block)?;
    transaction.set_int(&block, NUMBER_OFFSET, 20, true)?;
    transaction.set_string(&block, TEXT_OFFSET, "失われる更新", true)?;
    transaction.unpin(&block)?;

    // Force eviction so uncommitted data, and its undo log, reach disk.
    let other = transaction.append("test.tbl")?;
    transaction.pin(&other)?;
    drop(transaction);
    drop(database);

    for _ in 0..2 {
        let database = Database::open(&directory)?;
        let mut transaction = database.begin_transaction()?;
        transaction.recover()?;
        transaction.pin(&block)?;

        assert_eq!(transaction.get_int(&block, NUMBER_OFFSET)?, 10);
        assert_eq!(transaction.get_string(&block, TEXT_OFFSET)?, "元の値");

        transaction.commit()?;
    }

    Ok(())
}

#[test]
fn writer_waits_for_reader_and_proceeds_after_commit() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = Database::open(&directory)?;
    let block = database.create_initialized_block()?;
    let mut reader = database.begin_transaction()?;
    reader.pin(&block)?;
    assert_eq!(reader.get_int(&block, NUMBER_OFFSET)?, 10);

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let mut writer = database.begin_transaction()?;
        writer.pin(&block)?;
        scope.spawn(move || {
            started_sender.send(()).unwrap();
            let result = writer
                .set_int(&block, NUMBER_OFFSET, 20, true)
                .and_then(|_| writer.commit());
            finished_sender.send(result).unwrap();
        });

        started_receiver.recv().unwrap();
        assert!(matches!(
            finished_receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        reader.commit()?;
        finished_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()?;

        Ok(())
    })?;

    Ok(())
}

#[test]
fn lock_upgrade_times_out_and_rollback_releases_the_read_lock() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = Database::open(&directory)?;
    let block = database.create_initialized_block()?;
    let mut first = database.begin_transaction()?;
    let mut second = database.begin_transaction()?;
    first.pin(&block)?;
    second.pin(&block)?;

    assert_eq!(first.get_int(&block, NUMBER_OFFSET)?, 10);
    assert_eq!(second.get_int(&block, NUMBER_OFFSET)?, 10);

    assert_eq!(
        first
            .set_int(&block, NUMBER_OFFSET, 20, true)
            .unwrap_err()
            .kind(),
        ErrorKind::TimedOut
    );
    first.rollback()?;

    second.set_int(&block, NUMBER_OFFSET, 30, true)?;
    second.commit()?;

    assert_eq!(database.buffer_manager.available()?, BUFFER_COUNT);

    Ok(())
}

#[test]
fn file_size_read_prevents_append_until_transaction_finishes() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = Database::open(&directory)?;
    database.create_initialized_block()?;
    let mut reader = database.begin_transaction()?;
    assert_eq!(reader.size("test.tbl")?, 1);

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let mut writer = database.begin_transaction()?;
        scope.spawn(move || {
            started_sender.send(()).unwrap();
            let result = writer.append("test.tbl").and_then(|block| {
                writer.commit()?;
                Ok(block.number())
            });
            finished_sender.send(result).unwrap();
        });

        started_receiver.recv().unwrap();
        assert!(matches!(
            finished_receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(reader.size("test.tbl")?, 1);

        reader.commit()?;
        assert_eq!(
            finished_receiver
                .recv_timeout(Duration::from_secs(2))
                .unwrap()?,
            1
        );

        Ok(())
    })?;

    Ok(())
}
