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
    fm: Arc<Mutex<FileManager>>,
    lm: Arc<Mutex<LogManager>>,
}

impl Database {
    fn new() -> io::Result<Self> {
        let directory = TestDirectory::new();
        let fm = Arc::new(Mutex::new(FileManager::new(directory.path(), BLOCK_SIZE)?));
        let lm = Arc::new(Mutex::new(LogManager::new(
            Arc::clone(&fm),
            "simpledb.log",
        )?));

        Ok(Self { directory, fm, lm })
    }

    fn pool(&self, count: usize) -> io::Result<Arc<BufferManager>> {
        Ok(Arc::new(BufferManager::new(
            Arc::clone(&self.fm),
            Arc::clone(&self.lm),
            count,
        )?))
    }

    fn block(&self) -> io::Result<BlockId> {
        self.fm.lock().unwrap().append("test.tbl")
    }

    fn disk_value(&self, block: &BlockId) -> io::Result<i32> {
        let mut page = Page::new(BLOCK_SIZE);
        self.fm.lock().unwrap().read(block, &mut page)?;
        page.get_int(0)
    }
}

#[test]
fn logged_updates_can_be_undone_in_reverse_order() -> io::Result<()> {
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let block = db.block()?;

    let pin = pool.pin(&block)?;
    {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, 10)?;
        buffer.set_modified(0, None);
    }
    pool.flush_all(0)?;

    for value in [20, 30] {
        let mut buffer = pin.lock()?;
        let old_value = buffer.contents().get_int(0)?;

        let mut record = Page::new(4);
        record.set_int(0, old_value)?;
        let lsn = db.lm.lock().unwrap().append(&record.into_bytes())?;

        buffer.contents_mut().set_int(0, value)?;
        buffer.set_modified(1, Some(lsn));
    }
    pool.flush_all(1)?;

    assert_eq!(db.disk_value(&block)?, 30);

    drop(pin);

    // Recovery holds the iterator, not the log lock, while undoing through buffers.
    let records = db.lm.lock().unwrap().iterator()?;
    for record in records {
        let old_value = Page::from_bytes(record?).get_int(0)?;

        let pin = pool.pin(&block)?;
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, old_value)?;
        buffer.set_modified(2, None);
    }
    pool.flush_all(2)?;

    assert_eq!(db.disk_value(&block)?, 10);
    assert_eq!(pool.available()?, 1);
    assert_eq!(
        db.lm
            .lock()
            .unwrap()
            .iterator()?
            .collect::<io::Result<Vec<_>>>()?
            .len(),
        2
    );

    Ok(())
}

#[test]
fn log_records_cross_blocks_and_survive_reopening_in_reverse_order() -> io::Result<()> {
    let db = Database::new()?;
    let records: Vec<_> = (0..70_i32)
        .map(|value| value.to_le_bytes().to_vec())
        .collect();
    let mut log = db.lm.lock().unwrap();

    assert_eq!(log.iterator()?.count(), 0);

    for (index, record) in records.iter().enumerate() {
        assert_eq!(log.append(record)?, index as u64 + 1);
    }
    log.flush(65)?;

    let expected: Vec<_> = records.into_iter().rev().collect();
    assert_eq!(log.iterator()?.collect::<io::Result<Vec<_>>>()?, expected);

    drop(log);

    let reopened_fm = Arc::new(Mutex::new(FileManager::new(
        db.directory.path(),
        BLOCK_SIZE,
    )?));
    let mut reopened = LogManager::new(reopened_fm, "simpledb.log")?;

    assert_eq!(
        reopened.iterator()?.collect::<io::Result<Vec<_>>>()?,
        expected
    );

    reopened.append(b"after restart")?;
    let mut iter = reopened.iterator()?;

    assert_eq!(iter.next().unwrap()?, b"after restart");
    assert_eq!(iter.collect::<io::Result<Vec<_>>>()?, expected);

    Ok(())
}

#[test]
fn log_flushes_only_when_needed_and_iterator_keeps_its_start_position() -> io::Result<()> {
    let db = Database::new()?;
    let path = db.directory.path().join("simpledb.log");
    let initial = fs::read(&path)?;
    let mut log = db.lm.lock().unwrap();

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
fn log_handles_exact_fit_empty_records_and_rejects_oversized_records_without_changes()
-> io::Result<()> {
    let db = Database::new()?;
    let mut log = db.lm.lock().unwrap();
    let record = vec![255; BLOCK_SIZE - 8];

    assert_eq!(log.append(&record)?, 1);

    assert_eq!(
        log.append(&[0; BLOCK_SIZE - 7]).unwrap_err().kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(log.flush(2).unwrap_err().kind(), ErrorKind::InvalidInput);

    assert_eq!(log.append(&[])?, 2);
    assert_eq!(
        log.iterator()?.collect::<io::Result<Vec<_>>>()?,
        [Vec::new(), record]
    );
    assert_eq!(db.fm.lock().unwrap().length("simpledb.log")?, 2);

    Ok(())
}

#[test]
fn log_rejects_invalid_block_size_and_corrupt_boundaries() -> io::Result<()> {
    let directory = TestDirectory::new();
    let small_fm = Arc::new(Mutex::new(FileManager::new(directory.path(), 7)?));

    assert!(LogManager::new(small_fm, "small.log").is_err());

    let db = Database::new()?;
    for boundary in [-1, 0, 3, BLOCK_SIZE as i32 + 1] {
        let mut page = Page::new(BLOCK_SIZE);
        page.set_int(0, boundary)?;
        db.fm
            .lock()
            .unwrap()
            .write(&BlockId::new("bad.log", 0), &page)?;

        assert!(
            matches!(LogManager::new(Arc::clone(&db.fm), "bad.log"), Err(e) if e.kind() == ErrorKind::InvalidData)
        );
    }

    Ok(())
}

#[test]
fn log_iterator_reports_corrupt_records_once_then_stops() -> io::Result<()> {
    let db = Database::new()?;
    let mut page = Page::new(BLOCK_SIZE);
    page.set_int(0, 4)?;
    page.set_int(4, -1)?;
    db.fm
        .lock()
        .unwrap()
        .write(&BlockId::new("bad.log", 0), &page)?;

    let mut log = LogManager::new(Arc::clone(&db.fm), "bad.log")?;
    let mut iter = log.iterator()?;

    assert_eq!(
        iter.next().unwrap().unwrap_err().kind(),
        ErrorKind::InvalidData
    );
    assert!(iter.next().is_none());

    Ok(())
}

#[test]
fn repeated_pins_share_a_buffer_and_only_last_drop_makes_it_available() -> io::Result<()> {
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let block = db.block()?;

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
    let db = Database::new()?;
    let pool = db.pool(3)?;
    let first = db.block()?;
    let second = db.block()?;
    let log_path = db.directory.path().join("simpledb.log");
    let initial_log = fs::read(&log_path)?;

    let lsn = db.lm.lock().unwrap().append(b"update")?;
    let pin = pool.pin(&first)?;
    {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, 345)?;
        buffer.set_modified(1, Some(lsn));
        buffer.set_modified(1, None);
    }
    drop(pin);

    assert_eq!(db.disk_value(&first)?, 0);
    assert_eq!(fs::read(&log_path)?, initial_log);

    let replacement = pool.pin(&second)?;

    assert_eq!(db.disk_value(&first)?, 345);
    assert_ne!(fs::read(&log_path)?, initial_log);
    assert_eq!(replacement.lock()?.modifying_tx(), None);

    Ok(())
}

#[test]
fn flush_all_writes_only_the_requested_transactions_pages() -> io::Result<()> {
    let db = Database::new()?;
    let pool = db.pool(2)?;
    let first = db.block()?;
    let second = db.block()?;

    let a = pool.pin(&first)?;
    let b = pool.pin(&second)?;
    for (pin, tx) in [(&a, 1), (&b, 2)] {
        let mut buffer = pin.lock()?;
        buffer.contents_mut().set_int(0, tx)?;
        buffer.set_modified(tx as u64, None);
    }
    pool.flush_all(1)?;

    assert_eq!(db.disk_value(&first)?, 1);
    assert_eq!(db.disk_value(&second)?, 0);
    assert_eq!(a.lock()?.modifying_tx(), None);
    assert_eq!(b.lock()?.modifying_tx(), Some(2));

    Ok(())
}

#[test]
fn poisoned_buffer_rejects_access_without_panicking_during_unpin() -> io::Result<()> {
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let block = db.block()?;
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
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let block = db.block()?;
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
    assert_eq!(db.disk_value(&block)?, 0);
    assert_eq!(pin.lock()?.modifying_tx(), Some(1));

    db.lm.lock().unwrap().append(b"update")?;
    pool.flush_all(1)?;

    assert_eq!(db.disk_value(&block)?, 99);

    Ok(())
}

#[test]
fn failed_read_invalidates_the_old_cached_block() -> io::Result<()> {
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let block = db.block()?;

    drop(pool.pin(&block)?);
    fs::write(db.directory.path().join("partial.tbl"), [1, 2, 3])?;

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
    let db = Database::new()?;
    let pool = db.pool(1)?;
    let first = db.block()?;
    let second = db.block()?;
    let pin = pool.pin(&first)?;

    std::thread::scope(|scope| -> io::Result<()> {
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let pool = &pool;

        scope.spawn(move || {
            started_tx.send(()).unwrap();
            let result = pool.pin(&second).map(drop);
            finished_tx.send(result).unwrap();
        });

        started_rx.recv().unwrap();

        assert!(matches!(
            finished_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));

        drop(pin);
        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap()?;

        Ok(())
    })?;

    assert_eq!(pool.available()?, 1);

    Ok(())
}

#[test]
fn exhausted_pool_times_out_and_remains_usable() -> io::Result<()> {
    let db = Database::new()?;

    assert!(db.pool(0).is_err());

    let pool = db.pool(1)?;
    let first = db.block()?;
    let second = db.block()?;
    let pin = pool.pin(&first)?;

    assert!(matches!(pool.pin(&second), Err(e) if e.kind() == ErrorKind::TimedOut));
    assert_eq!(pool.available()?, 0);

    drop(pin);
    drop(pool.pin(&second)?);

    assert_eq!(pool.available()?, 1);

    Ok(())
}
