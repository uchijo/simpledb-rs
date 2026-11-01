use std::io::{self, Error, ErrorKind};
use std::ops::Range;

/// In-memory bytes of a block or log record.
/// Integers and byte lengths are signed 32-bit little-endian values.
#[derive(Clone, Debug)]
pub struct Page {
    contents: Vec<u8>,
}

impl Page {
    const INT_BYTES: usize = size_of::<i32>();

    pub fn new(size: usize) -> Self {
        Self::from_bytes(vec![0; size])
    }

    pub fn from_bytes(contents: Vec<u8>) -> Self {
        Self { contents }
    }

    fn range(&self, offset: usize, length: usize) -> io::Result<Range<usize>> {
        let end = offset
            .checked_add(length)
            .filter(|&end| end <= self.contents.len())
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "value exceeds page bounds"))?;
        Ok(offset..end)
    }

    pub fn get_int(&self, offset: usize) -> io::Result<i32> {
        let range = self.range(offset, Self::INT_BYTES)?;
        let mut bytes = [0; Self::INT_BYTES];
        bytes.copy_from_slice(&self.contents[range]);
        Ok(i32::from_le_bytes(bytes))
    }

    pub fn set_int(&mut self, offset: usize, value: i32) -> io::Result<()> {
        let range = self.range(offset, Self::INT_BYTES)?;
        self.contents[range].copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn get_bytes(&self, offset: usize) -> io::Result<&[u8]> {
        let length = usize::try_from(self.get_int(offset)?)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "negative byte length"))?;
        // get_int already checked that the length prefix fits.
        let range = self
            .range(offset + Self::INT_BYTES, length)
            .map_err(|_| Error::new(ErrorKind::InvalidData, "byte length exceeds page bounds"))?;
        Ok(&self.contents[range])
    }

    pub fn set_bytes(&mut self, offset: usize, value: &[u8]) -> io::Result<()> {
        let length = i32::try_from(value.len())
            .map_err(|_| Error::new(ErrorKind::InvalidInput, "byte length exceeds i32"))?;
        self.range(offset, Self::INT_BYTES + value.len())?;
        self.set_int(offset, length)?;
        let start = offset + Self::INT_BYTES;
        self.contents[start..start + value.len()].copy_from_slice(value);
        Ok(())
    }

    pub fn get_string(&self, offset: usize) -> io::Result<&str> {
        std::str::from_utf8(self.get_bytes(offset)?)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))
    }

    pub fn set_string(&mut self, offset: usize, value: &str) -> io::Result<()> {
        self.set_bytes(offset, value.as_bytes())
    }

    /// Maximum storage size for this many Unicode scalar values in UTF-8.
    pub fn max_length(char_count: usize) -> io::Result<usize> {
        let bytes = char_count
            .checked_mul(4)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "string size overflow"))?;
        bytes
            .checked_add(Self::INT_BYTES)
            .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "string size overflow"))
    }

    pub(crate) fn contents(&self) -> &[u8] {
        &self.contents
    }
    pub(crate) fn contents_mut(&mut self) -> &mut [u8] {
        &mut self.contents
    }
}

#[cfg(test)]
mod tests {
    use super::Page;
    use std::io::{self, ErrorKind};

    #[test]
    fn string_round_trip_preserves_japanese_and_emoji() -> io::Result<()> {
        let text = "日本語🦀";
        let storage_size = Page::max_length(text.chars().count())?;
        let mut page = Page::new(storage_size);

        page.set_string(0, text)?;

        assert_eq!(page.get_string(0)?, text);
        // The length prefix stores the UTF-8 byte count, not the character count.
        assert_eq!(page.get_int(0)?, text.len() as i32);
        Ok(())
    }

    #[test]
    fn byte_array_round_trip_preserves_non_text_bytes() -> io::Result<()> {
        let mut page = Page::new(16);
        let bytes = [0, 255, 128];

        page.set_bytes(0, &bytes)?;

        assert_eq!(page.get_bytes(0)?, &bytes);
        Ok(())
    }

    #[test]
    fn string_read_rejects_invalid_utf8() -> io::Result<()> {
        let mut page = Page::new(16);
        page.set_bytes(0, &[255])?;

        let error = page.get_string(0).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        Ok(())
    }

    #[test]
    fn signed_integer_uses_little_endian_storage() -> io::Result<()> {
        // -345 is 0xfffffea7, stored least significant byte first.
        let stored_bytes = vec![0xa7, 0xfe, 0xff, 0xff];
        let stored_page = Page::from_bytes(stored_bytes.clone());
        assert_eq!(stored_page.get_int(0)?, -345);

        let mut written_page = Page::new(Page::INT_BYTES);
        written_page.set_int(0, -345)?;
        assert_eq!(written_page.contents(), stored_bytes);
        Ok(())
    }

    #[test]
    fn failed_write_preserves_existing_value() -> io::Result<()> {
        let mut page = Page::new(Page::INT_BYTES);
        page.set_bytes(0, &[])?;

        // Only the length prefix fits; there is no space for a payload.
        let error = page.set_bytes(0, &[1]).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert_eq!(page.get_bytes(0)?, &[]);
        Ok(())
    }

    #[test]
    fn integer_access_outside_page_returns_error() {
        let mut page = Page::new(Page::INT_BYTES);

        assert!(page.get_int(1).is_err());
        assert!(page.set_int(usize::MAX, 1).is_err());
    }

    #[test]
    fn byte_read_rejects_negative_length() -> io::Result<()> {
        let mut page = Page::new(16);
        page.set_int(0, -1)?;

        let error = page.get_bytes(0).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        Ok(())
    }

    #[test]
    fn byte_read_rejects_length_larger_than_page() -> io::Result<()> {
        let mut page = Page::new(16);
        let payload_larger_than_page = page.contents().len() as i32;
        page.set_int(0, payload_larger_than_page)?;

        let error = page.get_bytes(0).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        Ok(())
    }

    #[test]
    fn storage_size_overflow_returns_error() {
        assert!(Page::max_length(usize::MAX).is_err());
    }
}
