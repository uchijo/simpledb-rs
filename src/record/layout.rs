use std::collections::HashMap;
use std::io::{self, Error, ErrorKind};

use super::{FieldType, Schema};
use crate::file::Page;

pub(super) const SLOT_FLAG_BYTES: usize = size_of::<i32>();

/// Physical field offsets, including the integer-sized slot flag at offset 0.
#[derive(Clone, Debug)]
pub struct Layout {
    schema: Schema,
    offsets: HashMap<String, usize>,
    slot_size: usize,
}

impl Layout {
    pub fn new(schema: Schema) -> io::Result<Self> {
        let mut offsets = HashMap::new();
        let mut slot_size = SLOT_FLAG_BYTES;
        for field_name in schema.fields() {
            offsets.insert(field_name.clone(), slot_size);
            slot_size = slot_size
                .checked_add(field_size(schema.field_type(field_name)?)?)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "record size overflow"))?;
        }
        Ok(Self {
            schema,
            offsets,
            slot_size,
        })
    }

    /// Reconstruct a layout from the catalog, as needed by chapter 7.
    pub fn from_parts(
        schema: Schema,
        offsets: HashMap<String, usize>,
        slot_size: usize,
    ) -> io::Result<Self> {
        if slot_size < SLOT_FLAG_BYTES || offsets.len() != schema.fields().len() {
            return Err(Error::new(ErrorKind::InvalidInput, "invalid record layout"));
        }
        let mut ranges = Vec::new();
        for field_name in schema.fields() {
            let start = *offsets
                .get(field_name)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "missing field offset"))?;
            if start < SLOT_FLAG_BYTES {
                return Err(Error::new(
                    ErrorKind::InvalidInput,
                    "field overlaps slot flag",
                ));
            }
            let end = start
                .checked_add(field_size(schema.field_type(field_name)?)?)
                .filter(|&end| end <= slot_size)
                .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "field exceeds slot"))?;
            ranges.push(start..end);
        }
        ranges.sort_by_key(|range| range.start);
        if ranges.windows(2).any(|pair| pair[0].end > pair[1].start) {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "overlapping record fields",
            ));
        }
        Ok(Self {
            schema,
            offsets,
            slot_size,
        })
    }

    pub fn schema(&self) -> &Schema {
        &self.schema
    }

    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    pub fn offset(&self, field_name: &str) -> io::Result<usize> {
        self.offsets.get(field_name).copied().ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidInput,
                format!("unknown field: {field_name}"),
            )
        })
    }
}

fn field_size(field_type: FieldType) -> io::Result<usize> {
    match field_type {
        FieldType::Integer => Ok(size_of::<i32>()),
        FieldType::Varchar(char_count) => Page::max_length(char_count),
    }
}
