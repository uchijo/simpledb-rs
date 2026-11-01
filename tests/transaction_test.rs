mod support;

use std::io::{self, ErrorKind};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use simpledb_rs::buffer::BufferManager;
use simpledb_rs::file::{BlockId, FileManager};
use simpledb_rs::log::LogManager;
use simpledb_rs::tx::{LockTable, Transaction};
use support::TestDirectory;

struct Database {
    files: Arc<Mutex<FileManager>>,
    log: Arc<Mutex<LogManager>>,
    buffers: Arc<BufferManager>,
    locks: Arc<LockTable>,
}

impl Database {
    fn open(directory: &TestDirectory) -> io::Result<Self> {
        let files = Arc::new(Mutex::new(FileManager::new(directory.path(), 400)?));
        let log = Arc::new(Mutex::new(LogManager::new(
            Arc::clone(&files),
            "simpledb.log",
        )?));
        let buffers = Arc::new(BufferManager::new(Arc::clone(&files), Arc::clone(&log), 2)?);
        Ok(Self {
            files,
            log,
            buffers,
            locks: Arc::new(LockTable::default()),
        })
    }

    fn transaction(&self) -> io::Result<Transaction> {
        Transaction::new(
            Arc::clone(&self.files),
            Arc::clone(&self.log),
            Arc::clone(&self.buffers),
            Arc::clone(&self.locks),
        )
    }

    fn initialize(&self) -> io::Result<BlockId> {
        let mut tx = self.transaction()?;
        let block = tx.append("test.tbl")?;
        tx.pin(&block)?;
        tx.set_int(&block, 0, 10, false)?;
        // Formatting must not interpret the previous bytes as a string.
        tx.set_int(&block, 40, -1, false)?;
        tx.set_string(&block, 40, "元の値", false)?;
        tx.commit()?;
        Ok(block)
    }
}

#[test]
fn commit_persists_and_rollback_undoes_repeated_updates() -> io::Result<()> {
    let directory = TestDirectory::new();
    let db = Database::open(&directory)?;
    let block = db.initialize()?;
    let mut tx = db.transaction()?;
    tx.pin(&block)?;
    tx.pin(&block)?;
    tx.unpin(&block)?;

    tx.set_int(&block, 0, 20, true)?;
    tx.set_int(&block, 0, 30, true)?;
    tx.set_string(&block, 40, "変更🦀", true)?;
    tx.rollback()?;

    assert_eq!(db.buffers.available()?, 2);
    assert_eq!(
        tx.get_int(&block, 0).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );

    let mut tx = db.transaction()?;
    tx.pin(&block)?;
    assert_eq!(tx.get_int(&block, 0)?, 10);
    assert_eq!(tx.get_string(&block, 40)?, "元の値");

    tx.set_int(&block, 0, 99, true)?;
    tx.commit()?;
    drop(db);

    let db = Database::open(&directory)?;
    let mut tx = db.transaction()?;
    tx.recover()?;
    tx.pin(&block)?;
    assert_eq!(tx.get_int(&block, 0)?, 99);
    tx.commit()?;
    Ok(())
}

#[test]
fn restart_undoes_flushed_uncommitted_data_and_recovery_is_repeatable() -> io::Result<()> {
    let directory = TestDirectory::new();
    let db = Database::open(&directory)?;
    let block = db.initialize()?;
    let mut tx = db.transaction()?;
    tx.pin(&block)?;
    tx.set_int(&block, 0, 20, true)?;
    tx.set_string(&block, 40, "失われる更新", true)?;
    tx.unpin(&block)?;

    // Force eviction so uncommitted data, and its undo log, reach disk.
    let other = tx.append("test.tbl")?;
    tx.pin(&other)?;
    drop(tx);
    drop(db);

    for _ in 0..2 {
        let db = Database::open(&directory)?;
        let mut tx = db.transaction()?;
        tx.recover()?;
        tx.pin(&block)?;

        assert_eq!(tx.get_int(&block, 0)?, 10);
        assert_eq!(tx.get_string(&block, 40)?, "元の値");

        tx.commit()?;
    }
    Ok(())
}

#[test]
fn writer_waits_for_reader_and_proceeds_after_commit() -> io::Result<()> {
    let directory = TestDirectory::new();
    let db = Database::open(&directory)?;
    let block = db.initialize()?;
    let mut reader = db.transaction()?;
    reader.pin(&block)?;
    assert_eq!(reader.get_int(&block, 0)?, 10);

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut writer = db.transaction()?;
        writer.pin(&block)?;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result = writer
                .set_int(&block, 0, 20, true)
                .and_then(|_| writer.commit());
            done_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        reader.commit()?;
        done_rx.recv_timeout(Duration::from_secs(2)).unwrap()?;
        Ok(())
    })?;
    Ok(())
}

#[test]
fn lock_upgrade_times_out_and_rollback_releases_the_read_lock() -> io::Result<()> {
    let directory = TestDirectory::new();
    let db = Database::open(&directory)?;
    let block = db.initialize()?;
    let mut first = db.transaction()?;
    let mut second = db.transaction()?;
    first.pin(&block)?;
    second.pin(&block)?;

    assert_eq!(first.get_int(&block, 0)?, 10);
    assert_eq!(second.get_int(&block, 0)?, 10);

    assert_eq!(
        first.set_int(&block, 0, 20, true).unwrap_err().kind(),
        ErrorKind::TimedOut
    );
    first.rollback()?;

    second.set_int(&block, 0, 30, true)?;
    second.commit()?;

    assert_eq!(db.buffers.available()?, 2);
    Ok(())
}

#[test]
fn file_size_read_prevents_append_until_transaction_finishes() -> io::Result<()> {
    let directory = TestDirectory::new();
    let db = Database::open(&directory)?;
    db.initialize()?;
    let mut reader = db.transaction()?;
    assert_eq!(reader.size("test.tbl")?, 1);

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let mut writer = db.transaction()?;
        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result = writer.append("test.tbl").and_then(|block| {
                writer.commit()?;
                Ok(block.number())
            });
            done_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        assert_eq!(reader.size("test.tbl")?, 1);

        reader.commit()?;
        assert_eq!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap()?, 1);
        Ok(())
    })?;
    Ok(())
}
