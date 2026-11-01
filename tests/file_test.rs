mod support;

use simpledb_rs::file::{BlockId, FileManager, Page};
use std::fs;
use std::io::{self, ErrorKind};
use support::TestDirectory;

const BLOCK_SIZE: usize = 400;

#[test]
fn written_values_can_be_read_after_reopening_database() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database_path = directory.path().join("db");
    let block = BlockId::new("students.tbl", 2);
    let text = "abcdefghijklm";
    let text_offset = 88;
    let number_offset = text_offset + Page::max_length(text.chars().count())?;
    let mut written_page = Page::new(BLOCK_SIZE);
    written_page.set_string(text_offset, text)?;
    written_page.set_int(number_offset, 345)?;

    let mut writer = FileManager::new(&database_path, BLOCK_SIZE)?;
    writer.write(&block, &written_page)?;

    assert_eq!(writer.length("students.tbl")?, 3);

    drop(writer);

    let mut reader = FileManager::new(&database_path, BLOCK_SIZE)?;
    let mut loaded_page = Page::new(BLOCK_SIZE);
    reader.read(&block, &mut loaded_page)?;

    assert_eq!(loaded_page.get_string(text_offset)?, text);
    assert_eq!(loaded_page.get_int(number_offset)?, 345);

    Ok(())
}

#[test]
fn new_database_is_reported_only_on_first_open() -> io::Result<()> {
    let directory = TestDirectory::new();
    let database_path = directory.path().join("db");

    let first_open = FileManager::new(&database_path, BLOCK_SIZE)?;

    assert!(first_open.is_new());

    drop(first_open);

    let second_open = FileManager::new(&database_path, BLOCK_SIZE)?;

    assert!(!second_open.is_new());

    Ok(())
}

#[test]
fn append_does_not_overwrite_blocks_after_reading_from_start() -> io::Result<()> {
    let directory = TestDirectory::new();
    let file_path = directory.path().join("students.tbl");
    let original_bytes = [vec![1; BLOCK_SIZE], vec![2; BLOCK_SIZE]].concat();
    fs::write(&file_path, &original_bytes)?;
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;

    // Move the file cursor away from the end before appending.
    let first_block = BlockId::new("students.tbl", 0);
    let mut read_buffer = Page::new(BLOCK_SIZE);
    file_manager.read(&first_block, &mut read_buffer)?;
    file_manager.append("students.tbl")?;

    let expected_bytes = [original_bytes, vec![0; BLOCK_SIZE]].concat();
    assert_eq!(fs::read(&file_path)?, expected_bytes);

    Ok(())
}

#[test]
fn zero_block_size_is_rejected() {
    let directory = TestDirectory::new();

    assert!(FileManager::new(directory.path(), 0).is_err());
}

#[test]
fn reading_missing_block_returns_unexpected_eof() -> io::Result<()> {
    let directory = TestDirectory::new();
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;
    let block = BlockId::new("test", 0);
    let mut page = Page::new(BLOCK_SIZE);

    let error = file_manager.read(&block, &mut page).unwrap_err();

    assert_eq!(error.kind(), ErrorKind::UnexpectedEof);

    Ok(())
}

#[test]
fn page_size_must_match_block_size() -> io::Result<()> {
    let directory = TestDirectory::new();
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;
    let block = BlockId::new("test", 0);

    let write_error = file_manager
        .write(&block, &Page::new(BLOCK_SIZE - 1))
        .unwrap_err();
    let read_error = file_manager
        .read(&block, &mut Page::new(BLOCK_SIZE + 1))
        .unwrap_err();

    assert_eq!(write_error.kind(), ErrorKind::InvalidInput);
    assert_eq!(read_error.kind(), ErrorKind::InvalidInput);

    Ok(())
}

#[test]
fn overflowing_block_offset_is_rejected() -> io::Result<()> {
    let directory = TestDirectory::new();
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;
    let block = BlockId::new("test", u64::MAX);

    let error = file_manager
        .write(&block, &Page::new(BLOCK_SIZE))
        .unwrap_err();

    assert_eq!(error.kind(), ErrorKind::InvalidInput);

    Ok(())
}

#[test]
fn filename_must_be_a_single_name() -> io::Result<()> {
    let directory = TestDirectory::new();
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;

    for name in ["", "../outside", "/absolute", "nested/file"] {
        let error = file_manager.append(name).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidInput, "filename: {name:?}");
    }

    Ok(())
}

#[test]
fn partial_block_is_rejected() -> io::Result<()> {
    let directory = TestDirectory::new();
    let mut file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;
    // The file is shorter than one complete block.
    fs::write(directory.path().join("partial"), [1, 2, 3])?;

    let length_error = file_manager.length("partial").unwrap_err();
    let append_error = file_manager.append("partial").unwrap_err();

    assert_eq!(length_error.kind(), ErrorKind::InvalidData);
    assert_eq!(append_error.kind(), ErrorKind::InvalidData);

    Ok(())
}

#[test]
fn startup_removes_only_temporary_files() -> io::Result<()> {
    let directory = TestDirectory::new();
    fs::write(directory.path().join("temp_old"), [])?;
    fs::write(directory.path().join("students.tbl"), [])?;
    fs::create_dir(directory.path().join("temp_directory"))?;

    let _file_manager = FileManager::new(directory.path(), BLOCK_SIZE)?;

    assert!(!directory.path().join("temp_old").exists());
    assert!(directory.path().join("students.tbl").exists());
    assert!(directory.path().join("temp_directory").is_dir());

    Ok(())
}
