mod support;

use std::io::{self, ErrorKind};
use std::sync::Arc;

use simpledb_rs::metadata::{IndexInfo, MAX_NAME, MAX_VIEW_DEFINITION, StatInfo};
use simpledb_rs::record::{FieldType, Layout, Schema, TableScan};
use simpledb_rs::server::SimpleDb;
use support::TestDirectory;

const BLOCK_SIZE: usize = 512;

fn student_schema() -> Schema {
    let mut schema = Schema::new();
    schema.add_int_field("id");
    schema.add_string_field("name", 3);
    schema
}

#[test]
fn invalid_startup_options_do_not_create_a_database_directory() -> io::Result<()> {
    let directory = TestDirectory::new();
    let path = directory.path().join("database");
    let oversized_block_size = i32::MAX as usize + 1;
    for (block_size, buffer_count) in [(400, 1), (BLOCK_SIZE, 0), (oversized_block_size, 1)] {
        let error = SimpleDb::with_options(&path, block_size, buffer_count)
            .err()
            .unwrap();
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(!path.exists());
    }
    SimpleDb::new(&path)?;
    Ok(())
}

#[test]
fn catalogs_restore_layouts_views_and_index_definitions_after_restart() -> io::Result<()> {
    let directory = TestDirectory::new();
    let path = directory.path().join("database");
    let expected = Layout::new(student_schema())?;
    let view_definition = "🦀".repeat(MAX_VIEW_DEFINITION);

    {
        let database = SimpleDb::with_options(&path, BLOCK_SIZE, 1)?;
        let transaction = database.new_transaction()?;
        let metadata = database.metadata_manager();
        let manager = metadata.lock().unwrap();
        manager.create_table("student", student_schema(), Arc::clone(&transaction))?;
        manager.create_view("studentview", &view_definition, Arc::clone(&transaction))?;
        manager.create_index("studentid", "student", "id", Arc::clone(&transaction))?;
        manager.create_index("studentname", "student", "name", Arc::clone(&transaction))?;

        // Force both catalogs across blocks before reconstructing their layouts.
        for number in 0..8 {
            manager.create_table(
                &format!("extra{number}"),
                student_schema(),
                Arc::clone(&transaction),
            )?;
        }
        assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
        transaction.lock().unwrap().commit()?;
    }

    let database = SimpleDb::with_options(&path, BLOCK_SIZE, 1)?;
    let transaction = database.new_transaction()?;
    let metadata = database.metadata_manager();
    let mut manager = metadata.lock().unwrap();
    for table_name in ["tblcat", "fldcat", "viewcat", "idxcat", "student", "extra7"] {
        let layout = manager.get_layout(table_name, Arc::clone(&transaction))?;
        assert!(!layout.schema().fields().is_empty());
    }
    let actual = manager.get_layout("student", Arc::clone(&transaction))?;
    assert_eq!(actual.slot_size(), expected.slot_size());
    assert_eq!(actual.schema().fields(), expected.schema().fields());
    for field in expected.schema().fields() {
        assert_eq!(
            actual.schema().field_type(field)?,
            expected.schema().field_type(field)?
        );
        assert_eq!(actual.offset(field)?, expected.offset(field)?);
    }
    assert_eq!(
        manager.get_view_def("studentview", Arc::clone(&transaction))?,
        Some(view_definition)
    );
    assert_eq!(
        manager.get_view_def("missing", Arc::clone(&transaction))?,
        None
    );

    let indexes = manager.get_index_info("student", Arc::clone(&transaction))?;
    assert_eq!(indexes.len(), 2);
    assert_eq!(indexes["id"].index_name(), "studentid");
    assert_eq!(indexes["id"].field_name(), "id");
    assert_eq!(
        indexes["id"]
            .index_layout()
            .schema()
            .field_type("dataval")?,
        FieldType::Integer
    );
    assert_eq!(
        indexes["name"]
            .index_layout()
            .schema()
            .field_type("dataval")?,
        FieldType::Varchar(3)
    );
    assert_eq!(indexes["id"].records_output(), 0);

    assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
    transaction.lock().unwrap().commit()?;
    Ok(())
}

#[test]
fn index_cost_estimates_use_table_statistics_and_hash_buckets() -> io::Result<()> {
    let info = IndexInfo::new(
        "studentid",
        "id",
        &student_schema(),
        BLOCK_SIZE,
        StatInfo::new(1_000, 30_000),
    )?;

    // 30,000 entries / 32 per block / 100 buckets, rounded down.
    assert_eq!(info.blocks_accessed(), 9);
    assert_eq!(info.records_output(), 2);
    assert_eq!(info.distinct_values("id"), 1);
    assert_eq!(info.distinct_values("name"), 10_001);
    Ok(())
}

#[test]
fn statistics_refresh_after_100_cached_requests_and_ignore_empty_trailing_blocks() -> io::Result<()>
{
    let directory = TestDirectory::new();
    let database = SimpleDb::with_options(directory.path().join("database"), BLOCK_SIZE, 1)?;
    let metadata = database.metadata_manager();
    let transaction = database.new_transaction()?;
    let mut manager = metadata.lock().unwrap();
    manager.create_table("student", student_schema(), Arc::clone(&transaction))?;
    let layout = Arc::new(manager.get_layout("student", Arc::clone(&transaction))?);
    let empty = manager.get_stat_info("student", Arc::clone(&layout), Arc::clone(&transaction))?;
    assert_eq!(empty, StatInfo::new(0, 0));

    let slots_per_block = BLOCK_SIZE / layout.slot_size();
    let record_count = slots_per_block * 3 + 1;
    let mut scan = TableScan::new(Arc::clone(&transaction), "student", Arc::clone(&layout))?;
    for id in 0..record_count {
        scan.insert()?;
        scan.set_int("id", id as i32)?;
    }
    // Leave the last allocated block empty; the book counts through the last record.
    scan.delete()?;
    scan.close()?;
    assert_eq!(transaction.lock().unwrap().size("student.tbl")?, 4);
    transaction.lock().unwrap().commit()?;

    let transaction = database.new_transaction()?;
    for _ in 0..99 {
        assert_eq!(
            manager.get_stat_info("student", Arc::clone(&layout), Arc::clone(&transaction))?,
            empty
        );
    }
    let refreshed = manager.get_stat_info("student", layout, Arc::clone(&transaction))?;
    assert_eq!(refreshed.blocks_accessed(), 3);
    assert_eq!(refreshed.records_output(), (slots_per_block * 3) as u64);
    assert_eq!(refreshed.distinct_values("id"), 1 + slots_per_block as u64);
    assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
    transaction.lock().unwrap().commit()?;
    Ok(())
}

#[test]
fn invalid_definitions_do_not_leave_partial_catalog_entries() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = SimpleDb::new(directory.path().join("database"))?;
    let transaction = database.new_transaction()?;
    let metadata = database.metadata_manager();
    let manager = metadata.lock().unwrap();
    manager.create_table("student", student_schema(), Arc::clone(&transaction))?;

    let mut invalid_schema = student_schema();
    invalid_schema.add_int_field("x".repeat(MAX_NAME + 1));
    assert_eq!(
        manager
            .create_table("invalid", invalid_schema, Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        manager
            .get_layout("invalid", Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    for table_name in ["../outside", "tempstudent", ""] {
        assert_eq!(
            manager
                .create_table(table_name, student_schema(), Arc::clone(&transaction))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        manager
            .create_table("student", student_schema(), Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::AlreadyExists
    );

    let definition = "x".repeat(MAX_VIEW_DEFINITION + 1);
    assert_eq!(
        manager
            .create_view("invalid", &definition, Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    assert_eq!(
        manager.get_view_def("invalid", Arc::clone(&transaction))?,
        None
    );
    assert_eq!(
        manager
            .create_index("invalid", "student", "missing", Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::InvalidInput
    );
    transaction.lock().unwrap().commit()?;
    Ok(())
}

#[test]
fn catalog_reconstruction_rejects_invalid_field_types_lengths_and_offsets() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = SimpleDb::with_options(directory.path().join("database"), BLOCK_SIZE, 1)?;
    let metadata = database.metadata_manager();
    let manager = metadata.lock().unwrap();
    let transaction = database.new_transaction()?;
    manager.create_table("student", student_schema(), Arc::clone(&transaction))?;
    let catalog_layout = Arc::new(manager.get_layout("fldcat", Arc::clone(&transaction))?);
    transaction.lock().unwrap().commit()?;

    for (field, invalid_value) in [
        ("type", 99),
        ("length", -1),
        ("offset", 0),
        ("offset", i32::MAX),
    ] {
        let transaction = database.new_transaction()?;
        let mut catalog = TableScan::new(
            Arc::clone(&transaction),
            "fldcat",
            Arc::clone(&catalog_layout),
        )?;
        let mut found = false;
        while catalog.next()? {
            if catalog.get_string("tblname")? == "student" && catalog.get_string("fldname")? == "id"
            {
                catalog.set_int(field, invalid_value)?;
                found = true;
                break;
            }
        }
        assert!(found);
        catalog.close()?;

        assert_eq!(
            manager
                .get_layout("student", Arc::clone(&transaction))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidData
        );
        assert_eq!(transaction.lock().unwrap().available_buffs()?, 1);
        transaction.lock().unwrap().rollback()?;
    }
    Ok(())
}

#[test]
fn rollback_removes_table_view_and_index_metadata() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database = SimpleDb::new(directory.path().join("database"))?;
    let metadata = database.metadata_manager();
    let transaction = database.new_transaction()?;
    let mut manager = metadata.lock().unwrap();
    manager.create_table("student", student_schema(), Arc::clone(&transaction))?;
    manager.create_view(
        "studentview",
        "select id from student",
        Arc::clone(&transaction),
    )?;
    manager.create_index("studentid", "student", "id", Arc::clone(&transaction))?;
    transaction.lock().unwrap().rollback()?;

    let transaction = database.new_transaction()?;
    assert_eq!(
        manager
            .get_layout("student", Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        manager.get_view_def("studentview", Arc::clone(&transaction))?,
        None
    );
    assert!(
        manager
            .get_index_info("student", Arc::clone(&transaction))?
            .is_empty()
    );
    transaction.lock().unwrap().commit()?;
    Ok(())
}

#[test]
fn startup_recovers_uncommitted_catalog_changes_before_reading_metadata() -> io::Result<()> {
    let directory = TestDirectory::new();
    let path = directory.path().join("database");
    {
        let database = SimpleDb::with_options(&path, BLOCK_SIZE, 1)?;
        let metadata = database.metadata_manager();
        let manager = metadata.lock().unwrap();
        let committed = database.new_transaction()?;
        manager.create_table("committed", student_schema(), Arc::clone(&committed))?;
        committed.lock().unwrap().commit()?;

        let interrupted = database.new_transaction()?;
        manager.create_table("unfinished", student_schema(), Arc::clone(&interrupted))?;
        manager.create_view(
            "unfinishedview",
            "select id from unfinished",
            Arc::clone(&interrupted),
        )?;
        manager.create_index("unfinishedid", "unfinished", "id", Arc::clone(&interrupted))?;
        // With 1 buffer, catalog replacements have already forced uncommitted pages to disk.
    }

    let database = SimpleDb::with_options(&path, BLOCK_SIZE, 1)?;
    let metadata = database.metadata_manager();
    let mut manager = metadata.lock().unwrap();
    let transaction = database.new_transaction()?;
    manager.get_layout("committed", Arc::clone(&transaction))?;
    assert_eq!(
        manager
            .get_layout("unfinished", Arc::clone(&transaction))
            .unwrap_err()
            .kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        manager.get_view_def("unfinishedview", Arc::clone(&transaction))?,
        None
    );
    assert!(
        manager
            .get_index_info("unfinished", Arc::clone(&transaction))?
            .is_empty()
    );
    transaction.lock().unwrap().commit()?;
    Ok(())
}
