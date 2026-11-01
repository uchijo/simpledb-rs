use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Error, ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use super::{BlockId, Page};

/// Block-level disk access. Exclusive mutable access serializes file operations.
pub struct FileManager {
    directory: PathBuf,
    block_size: usize,
    is_new: bool,
    open_files: HashMap<String, File>,
}

impl FileManager {
    pub fn new(directory: impl AsRef<Path>, block_size: usize) -> io::Result<Self> {
        if block_size == 0 {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid block size"));
        }
        let directory = directory.as_ref().to_path_buf();
        let is_new = match fs::metadata(&directory) {
            Ok(metadata) if metadata.is_dir() => false,
            Ok(_) => {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "database path is not a directory",
                ));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => true,
            Err(e) => return Err(e),
        };
        fs::create_dir_all(&directory)?;
        // As in SimpleDB, names beginning with "temp" are reserved for temporary tables.
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("temp")
                && entry.file_type()?.is_file()
            {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(Self {
            directory,
            block_size,
            is_new,
            open_files: HashMap::new(),
        })
    }

    pub fn is_new(&self) -> bool {
        self.is_new
    }
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    fn get_file(&mut self, filename: &str) -> io::Result<&mut File> {
        let mut components = Path::new(filename).components();
        if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "expected a database file name",
            ));
        }
        if !self.open_files.contains_key(filename) {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.directory.join(filename))?;
            self.open_files.insert(filename.to_owned(), file);
        }
        Ok(self
            .open_files
            .get_mut(filename)
            .expect("file was inserted"))
    }

    fn offset(&self, block: &BlockId) -> io::Result<u64> {
        block
            .number()
            .checked_mul(self.block_size as u64)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "block offset overflow"))
    }

    fn check_page(&self, page: &Page) -> io::Result<()> {
        if page.contents().len() != self.block_size {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "page size must equal block size",
            ));
        }
        Ok(())
    }

    pub fn read(&mut self, block: &BlockId, page: &mut Page) -> io::Result<()> {
        self.check_page(page)?;
        let offset = self.offset(block)?;
        let file = self.get_file(block.filename())?;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(page.contents_mut())
    }

    pub fn write(&mut self, block: &BlockId, page: &Page) -> io::Result<()> {
        self.check_page(page)?;
        let offset = self.offset(block)?;
        let file = self.get_file(block.filename())?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(page.contents())?;
        file.sync_all()
    }

    pub fn length(&mut self, filename: &str) -> io::Result<u64> {
        let block_size = self.block_size as u64;
        let size = self.get_file(filename)?.metadata()?.len();
        if size % block_size != 0 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "file contains a partial block",
            ));
        }
        Ok(size / block_size)
    }

    pub fn append(&mut self, filename: &str) -> io::Result<BlockId> {
        let block = BlockId::new(filename, self.length(filename)?);
        self.write(&block, &Page::new(self.block_size))?;
        Ok(block)
    }
}
