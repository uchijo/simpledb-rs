mod support;

use std::fs;
use std::io::{self, ErrorKind};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use simpledb_rs::buffer::BufferManager;
use simpledb_rs::file::{BlockId, FileManager, Page};
use simpledb_rs::log::LogManager;
use support::TestDirectory;

const BLOCK_SIZE: usize = 64;

struct Database {
    directory: TestDirectory,
    file_manager: Arc<Mutex<FileManager>>,
    log_manager: Arc<Mutex<LogManager>>,
}

impl Database {
    fn new() -> io::Result<Self> {
        let directory = TestDirectory::new();
        let file_manager = Arc::new(Mutex::new(FileManager::new(directory.path(), BLOCK_SIZE)?));
        let log_manager = Arc::new(Mutex::new(LogManager::new(
            Arc::clone(&file_manager),
            "simpledb.log",
        )?));

        Ok(Self {
            directory,
            file_manager,
            log_manager,
        })
    }

    fn new_buffer_pool(&self, count: usize) -> io::Result<Arc<BufferManager>> {
        Ok(Arc::new(BufferManager::new(
            Arc::clone(&self.file_manager),
            Arc::clone(&self.log_manager),
            count,
        )?))
    }

    fn append_block(&self) -> io::Result<BlockId> {
        self.file_manager.lock().unwrap().append("test.tbl")
    }

    fn read_disk_int(&self, block: &BlockId) -> io::Result<i32> {
        let mut page = Page::new(BLOCK_SIZE);
        self.file_manager.lock().unwrap().read(block, &mut page)?;
        page.get_int(0)
    }
}

#[test]
fn log_records_cross_blocks_and_survive_reopening_in_reverse_order() -> io::Result<()> {
    let database = Database::new()?;
    let records: Vec<_> = (0..70_i32)
        .map(|value| value.to_le_bytes().to_vec())
        .collect();
    let mut log = database.log_manager.lock().unwrap();

    assert_eq!(log.iterator()?.count(), 0);

    for (index, record) in records.iter().enumerate() {
        assert_eq!(log.append(record)?, index as u64 + 1);
    }
    log.flush(65)?;

    let expected: Vec<_> = records.into_iter().rev().collect();
    assert_eq!(log.iterator()?.collect::<io::Result<Vec<_>>>()?, expected);

    drop(log);

    let reopened_files = Arc::new(Mutex::new(FileManager::new(
        database.directory.path(),
        BLOCK_SIZE,
    )?));
    let mut reopened = LogManager::new(reopened_files, "simpledb.log")?;

    assert_eq!(
        reopened.iterator()?.collect::<io::Result<Vec<_>>>()?,
        expected
    );

    reopened.append(b"after restart")?;
    let mut records_iter = reopened.iterator()?;

    assert_eq!(records_iter.next().unwrap()?, b"after restart");
    assert_eq!(records_iter.collect::<io::Result<Vec<_>>>()?, expected);

    Ok(())
}

#[test]
fn log_flushes_only_when_needed_and_iterator_keeps_its_start_position() -> io::Result<()> {
    let database = Database::new()?;
    let path = database.directory.path().join("simpledb.log");
    let initial = fs::read(&path)?;
    let mut log = database.log_manager.lock().unwrap();

    let first = log.append(b"first")?;

    assert_eq!(fs::read(&path)?, initial);

    log.flush(first)?;

    let saved = fs::read(&path)?;
    assert_ne!(saved, initial);

    let snapshot = log.iterator()?;
    log.append(b"second")?;
    log.flush(first)?;

    assert_eq!(fs::read(&path)?, saved);
    assert_eq!(
        snapshot.collect::<io::Result<Vec<_>>>()?,
        [b"first".to_vec()]
    );
    assert_eq!(
        log.iterator()?.collect::<io::Result<Vec<_>>>()?,
        [b"second".to_vec(), b"first".to_vec()]
    );

    Ok(())
}

#[test]
fn log_record_size_limits_preserve_existing_records() -> io::Result<()> {
    let database = Database::new()?;
    let mut log = database.log_manager.lock().unwrap();
    // Each block stores a boundary, and each record stores its byte length.
    let max_record_length = BLOCK_SIZE - 2 * size_of::<i32>();
    let record = vec![255; max_record_length];

    assert_eq!(log.append(&record)?, 1);

    assert_eq!(
        log.append(&vec![0; max_record_length + 1])
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(log.flush(2).unwrap_err().kind(), ErrorKind::InvalidInput);

    assert_eq!(log.append(&[])?, 2);
    assert_eq!(
        log.iterator()?.collect::<io::Result<Vec<_>>>()?,
        [Vec::new(), record]
    );
    assert_eq!(
        database
            .file_manager
            .lock()
            .unwrap()
            .length("simpledb.log")?,
        2
    );

    Ok(())
}

#[test]
fn log_rejects_invalid_block_size_and_corrupt_boundaries() -> io::Result<()> {
    let directory = TestDirectory::new();
    // A boundary integer and an empty record's length must both fit.
    let minimum_block_size = 2 * size_of::<i32>();
    let small_files = Arc::new(Mutex::new(FileManager::new(
        directory.path(),
        minimum_block_size - 1,
    )?));

    assert!(LogManager::new(small_files, "small.log").is_err());

    let database = Database::new()?;
    let boundary_bytes = size_of::<i32>() as i32;
    for boundary in [-1, 0, boundary_bytes - 1, BLOCK_SIZE as i32 + 1] {
        let mut page = Page::new(BLOCK_SIZE);
        page.set_int(0, boundary)?;
        database
            .file_manager
            .lock()
            .unwrap()
            .write(&BlockId::new("bad.log", 0), &page)?;

        assert!(
            matches!(LogManager::new(Arc::clone(&database.file_manager), "bad.log"), Err(e) if e.kind() == ErrorKind::InvalidData)
        );
    }

    Ok(())
}

#[test]
fn log_iterator_reports_corrupt_records_once_then_stops() -> io::Result<()> {
    let database = Database::new()?;
    let mut page = Page::new(BLOCK_SIZE);
    let record_offset = size_of::<i32>();
    page.set_int(0, record_offset as i32)?;
    page.set_int(record_offset, -1)?;
    database
        .file_manager
        .lock()
        .unwrap()
        .write(&BlockId::new("bad.log", 0), &page)?;

    let mut log = LogManager::new(Arc::clone(&database.file_manager), "bad.log")?;
    let mut records_iter = log.iterator()?;

    assert_eq!(
        records_iter.next().unwrap().unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert!(records_iter.next().is_none());

    Ok(())
}

#[test]
fn repeated_pins_share_a_buffer_until_the_last_unpin() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(1)?;
    let block = database.append_block()?;

    let first = pool.pin(&block)?;
    let second = pool.pin(&block)?;

    assert_eq!(pool.available()?, 0);

    {
        let mut buffer = first.lock()?;
        assert_eq!(buffer.block(), Some(&block));
        assert!(buffer.is_pinned());

        buffer.contents_mut().set_int(0, 42)?;
        buffer.set_modified(1, None);
    }
    assert_eq!(second.lock()?.contents().get_int(0)?, 42);

    drop(first);
    assert_eq!(pool.available()?, 0);

    drop(second);
    assert_eq!(pool.available()?, 1);

    Ok(())
}

#[test]
fn first_unpinned_replacement_flushes_dirty_page_and_its_log() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(3)?;
    let first = database.append_block()?;
    let second = database.append_block()?;
    let log_path = database.directory.path().join("simpledb.log");
    let initial_log = fs::read(&log_path)?;

    let lsn = database.log_manager.lock().unwrap().append(b"update")?;
    let pin = pool.pin(&first)?;
    {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, 345)?;
        buffer.set_modified(1, Some(lsn));
        buffer.set_modified(1, None);
    }
    drop(pin);

    assert_eq!(database.read_disk_int(&first)?, 0);
    assert_eq!(fs::read(&log_path)?, initial_log);

    let replacement = pool.pin(&second)?;

    assert_eq!(database.read_disk_int(&first)?, 345);
    assert_ne!(fs::read(&log_path)?, initial_log);
    assert_eq!(replacement.lock()?.modifying_tx(), None);

    Ok(())
}

#[test]
fn flush_all_writes_only_the_requested_transactions_pages() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(2)?;
    let first = database.append_block()?;
    let second = database.append_block()?;

    let first_pin = pool.pin(&first)?;
    let second_pin = pool.pin(&second)?;
    for (pin, tx) in [(&first_pin, 1), (&second_pin, 2)] {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, tx)?;
        buffer.set_modified(tx as u64, None);
    }
    pool.flush_all(1)?;

    assert_eq!(database.read_disk_int(&first)?, 1);
    assert_eq!(database.read_disk_int(&second)?, 0);
    assert_eq!(first_pin.lock()?.modifying_tx(), None);
    assert_eq!(second_pin.lock()?.modifying_tx(), Some(2));

    Ok(())
}

#[test]
fn poisoned_pool_mutex_rejects_access_and_allows_unpin() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(1)?;
    let block = database.append_block()?;
    let pin = pool.pin(&block)?;

    let panic = std::panic::catch_unwind(|| {
        let _buffer = pin.lock().unwrap();
        panic!("interrupted page update");
    });

    assert!(panic.is_err());
    assert!(matches!(pin.lock(), Err(error) if error.kind() == ErrorKind::Other));

    drop(pin);
    assert_eq!(pool.flush_all(1).unwrap_err().kind(), ErrorKind::Other);

    Ok(())
}

#[test]
fn failed_log_flush_prevents_data_write_and_keeps_buffer_dirty() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(1)?;
    let block = database.append_block()?;
    let pin = pool.pin(&block)?;
    {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, 99)?;
        buffer.set_modified(1, Some(1));
    }

    assert_eq!(
        pool.flush_all(1).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(database.read_disk_int(&block)?, 0);
    assert_eq!(pin.lock()?.modifying_tx(), Some(1));

    database.log_manager.lock().unwrap().append(b"update")?;
    pool.flush_all(1)?;

    assert_eq!(database.read_disk_int(&block)?, 99);

    Ok(())
}

#[test]
fn failed_read_invalidates_the_old_cached_block() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(1)?;
    let block = database.append_block()?;

    drop(pool.pin(&block)?);
    fs::write(database.directory.path().join("partial.tbl"), [1, 2, 3])?;

    assert!(
        matches!(pool.pin(&BlockId::new("partial.tbl", 0)), Err(e) if e.kind() == ErrorKind::UnexpectedEof)
    );
    assert_eq!(pool.available()?, 1);

    let pin = pool.pin(&block)?;

    assert_eq!(pin.lock()?.contents().get_int(0)?, 0);

    Ok(())
}

#[test]
fn waiting_pin_wakes_when_a_buffer_is_released() -> io::Result<()> {
    let database = Database::new()?;
    let pool = database.new_buffer_pool(1)?;
    let first = database.append_block()?;
    let second = database.append_block()?;
    let pin = pool.pin(&first)?;

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_sender, started_receiver) = mpsc::channel();
        let (finished_sender, finished_receiver) = mpsc::channel();
        let pool = &pool;

        scope.spawn(move || {
            started_sender.send(()).unwrap();
            let result = pool.pin(&second).map(drop);
            finished_sender.send(result).unwrap();
        });

        started_receiver.recv().unwrap();

        assert!(matches!(
            finished_receiver.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        drop(pin);
        finished_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap()?;

        Ok(())
    })?;

    assert_eq!(pool.available()?, 1);

    Ok(())
}

#[test]
fn exhausted_pool_times_out_and_remains_usable() -> io::Result<()> {
    let database = Database::new()?;

    assert!(database.new_buffer_pool(0).is_err());

    let pool = database.new_buffer_pool(1)?;
    let first = database.append_block()?;
    let second = database.append_block()?;
    let pin = pool.pin(&first)?;

    assert!(matches!(pool.pin(&second), Err(e) if e.kind() == ErrorKind::TimedOut));
    assert_eq!(pool.available()?, 0);

    drop(pin);
    drop(pool.pin(&second)?);

    assert_eq!(pool.available()?, 1);

    Ok(())
}
