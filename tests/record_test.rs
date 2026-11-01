mod support;

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::sync::{Arc, Mutex};

use simpledb_rs::buffer::BufferManager;
use simpledb_rs::file::{FileManager, Page};
use simpledb_rs::log::LogManager;
use simpledb_rs::record::{Layout, RecordPage, Rid, Schema, TableScan};
use simpledb_rs::tx::{LockTable, Transaction};
use support::TestDirectory;

const BLOCK_SIZE: usize = 400;

fn open_database_transaction(directory: &TestDirectory) -> io::Result<Arc<Mutex<Transaction>>> {
    let file_manager = Arc::new(Mutex::new(FileManager::new(directory.path(), BLOCK_SIZE)?));
    let log_manager = Arc::new(Mutex::new(LogManager::new(
        Arc::clone(&file_manager),
        "simpledb.log",
    )?));
    let buffer_manager = Arc::new(BufferManager::new(
        Arc::clone(&file_manager),
        Arc::clone(&log_manager),
        1,
    )?);
    Ok(Arc::new(Mutex::new(Transaction::new(
        file_manager,
        log_manager,
        buffer_manager,
        Arc::new(LockTable::default()),
    )?)))
}

fn student_layout() -> io::Result<Arc<Layout>> {
    let mut schema = Schema::new();
    schema.add_int_field("id");
    schema.add_string_field("name", 3);
    Ok(Arc::new(Layout::new(schema)?))
}

#[test]
fn layout_preserves_field_order_and_validates_catalog_offsets() -> io::Result<()> {
    let layout = student_layout()?;
    let integer_bytes = size_of::<i32>();

    assert_eq!(layout.offset("id")?, integer_bytes);
    assert_eq!(layout.offset("name")?, 2 * integer_bytes);
    assert_eq!(layout.slot_size(), 2 * integer_bytes + Page::max_length(3)?);

    let offsets = HashMap::from([
        ("id".to_owned(), layout.offset("id")?),
        ("name".to_owned(), layout.offset("name")?),
    ]);
    let restored = Layout::from_parts(layout.schema().clone(), offsets, layout.slot_size())?;
    assert_eq!(restored.offset("name")?, layout.offset("name")?);

    let overlapping = HashMap::from([
        ("id".to_owned(), integer_bytes),
        ("name".to_owned(), integer_bytes),
    ]);
    assert!(Layout::from_parts(layout.schema().clone(), overlapping, layout.slot_size()).is_err());

    let mut oversized = Schema::new();
    oversized.add_string_field("too_large", usize::MAX);
    assert!(Layout::new(oversized).is_err());

    Ok(())
}

#[test]
fn record_page_formats_fills_and_reuses_slots_without_overwriting_neighbors() -> io::Result<()> {
    let directory = TestDirectory::new();
    let transaction = open_database_transaction(&directory)?;
    let block = transaction.lock().unwrap().append("students.tbl")?;
    let layout = student_layout()?;
    let slot_count = BLOCK_SIZE / layout.slot_size();
    let mut page = RecordPage::new(Arc::clone(&transaction), block, layout)?;
    page.format()?;

    let mut previous_slot = None;
    for expected_slot in 0..slot_count {
        let slot = page.insert_after(previous_slot)?.unwrap();
        assert_eq!(slot, expected_slot);
        page.set_int(slot, "id", slot as i32)?;
        page.set_string(slot, "name", "日本🦀")?;
        previous_slot = Some(slot);
    }
    assert_eq!(page.insert_after(previous_slot)?, None);

    assert_eq!(
        page.set_string(0, "name", "長すぎる名前")
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(page.get_int(1, "id")?, 1);
    assert!(page.get_string(0, "id").is_err());
    assert!(page.get_int(slot_count, "id").is_err());
    assert!(page.get_int(0, "unknown").is_err());

    page.delete(0)?;
    assert_eq!(page.next_after(None)?, Some(1));
    assert_eq!(page.insert_after(None)?, Some(0));
    assert_eq!(page.get_string(1, "name")?, "日本🦀");

    page.close()?;
    page.close()?;
    assert!(page.next_after(None).is_err());
    assert!(page.get_int(0, "id").is_err());
    assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
    transaction.lock().unwrap().commit()?;

    Ok(())
}

#[test]
fn table_scan_crosses_empty_blocks_repositions_and_rolls_back_changes() -> io::Result<()> {
    let directory = TestDirectory::new();
    let layout = student_layout()?;
    let slots_per_block = BLOCK_SIZE / layout.slot_size();
    let record_count = slots_per_block * 3;
    let transaction = open_database_transaction(&directory)?;
    let mut scan = TableScan::new(Arc::clone(&transaction), "students", Arc::clone(&layout))?;
    let mut record_ids = Vec::new();

    for id in 0..record_count {
        scan.insert()?;
        scan.set_int("id", id as i32)?;
        scan.set_string("name", "日本🦀")?;
        record_ids.push(scan.current_rid()?);
    }
    scan.before_first()?;

    for expected_id in 0..record_count {
        assert!(scan.next()?);
        assert_eq!(scan.get_int("id")?, expected_id as i32);
    }
    assert!(!scan.next()?);
    assert!(!scan.next()?);
    assert!(scan.get_int("id").is_err());

    // An entirely empty middle block must not end a scan.
    for &rid in &record_ids[slots_per_block..2 * slots_per_block] {
        scan.move_to_rid(rid)?;
        scan.delete()?;
    }
    scan.close()?;
    transaction.lock().unwrap().commit()?;
    drop(scan);
    drop(transaction);

    let transaction = open_database_transaction(&directory)?;
    let mut scan = TableScan::new(Arc::clone(&transaction), "students", Arc::clone(&layout))?;
    let mut actual_ids = Vec::new();
    while scan.next()? {
        actual_ids.push(scan.get_int("id")?);
    }
    let expected_ids: Vec<_> = (0..record_count)
        .filter(|id| *id < slots_per_block || *id >= 2 * slots_per_block)
        .map(|id| id as i32)
        .collect();
    assert_eq!(actual_ids, expected_ids);

    scan.move_to_rid(record_ids[0])?;
    scan.set_int("id", 999)?;
    scan.delete()?;
    scan.before_first()?;
    scan.insert()?;
    scan.set_int("id", 888)?;
    scan.close()?;
    transaction.lock().unwrap().rollback()?;

    let transaction = open_database_transaction(&directory)?;
    let mut scan = TableScan::new(Arc::clone(&transaction), "students", layout)?;
    scan.move_to_rid(record_ids[0])?;
    assert_eq!(scan.get_int("id")?, 0);
    assert!(scan.move_to_rid(Rid::new(0, slots_per_block)).is_err());

    scan.before_first()?;
    while scan.next()? {}
    scan.insert()?;
    assert_eq!(
        scan.current_rid()?.block_number(),
        (record_count / slots_per_block) as u64
    );

    scan.close()?;
    transaction.lock().unwrap().commit()?;

    Ok(())
}

#[test]
fn failed_close_retains_the_page_for_retry() -> io::Result<()> {
    let directory = TestDirectory::new();
    let transaction = open_database_transaction(&directory)?;
    let mut scan = TableScan::new(Arc::clone(&transaction), "students", student_layout()?)?;

    // Interrupt a transaction guard without changing its data.
    let interrupted = std::panic::catch_unwind(|| {
        let _guard = transaction.lock().unwrap();
        panic!("interrupted transaction access");
    });
    assert!(interrupted.is_err());
    assert!(scan.close().is_err());

    transaction.clear_poison();
    assert_eq!(transaction.lock().unwrap().available_buffs()?, 0);

    scan.close()?;
    assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
    transaction.lock().unwrap().rollback()?;

    Ok(())
}
