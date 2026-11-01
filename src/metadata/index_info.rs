use std::io::{self, Error, ErrorKind};

use super::StatInfo;
use crate::record::{Layout, Schema};

/// An index definition and its planner estimates; index storage belongs to chapter 12.
#[derive(Clone, Debug)]
pub struct IndexInfo {
    index_name: String,
    field_name: String,
    layout: Layout,
    records_per_block: u64,
    table_stats: StatInfo,
}

impl IndexInfo {
    pub fn new(
        index_name: &str,
        field_name: &str,
        table_schema: &Schema,
        block_size: usize,
        table_stats: StatInfo,
    ) -> io::Result<Self> {
        let layout = create_index_layout(table_schema, field_name, block_size)?;
        let records_per_block = block_size / layout.slot_size();
        Ok(Self {
            index_name: index_name.to_owned(),
            field_name: field_name.to_owned(),
            layout,
            records_per_block: records_per_block as u64,
            table_stats,
        })
    }

    pub fn index_name(&self) -> &str {
        &self.index_name
    }

    pub fn field_name(&self) -> &str {
        &self.field_name
    }

    pub fn index_layout(&self) -> &Layout {
        &self.layout
    }

    pub fn blocks_accessed(&self) -> u64 {
        // shortcut: use the book's 100-bucket hash cost until chapter 12 supplies HashIndex.
        const HASH_BUCKET_COUNT: u64 = 100;
        let index_blocks = self.table_stats.records_output() / self.records_per_block;
        index_blocks / HASH_BUCKET_COUNT
    }

    pub fn records_output(&self) -> u64 {
        self.table_stats.records_output() / self.table_stats.distinct_values(&self.field_name)
    }

    pub fn distinct_values(&self, field_name: &str) -> u64 {
        if field_name == self.field_name {
            1
        } else {
            self.table_stats.distinct_values(field_name)
        }
    }
}

pub(super) fn create_index_layout(
    table_schema: &Schema,
    field_name: &str,
    block_size: usize,
) -> io::Result<Layout> {
    let mut schema = Schema::new();
    schema.add_int_field("block");
    schema.add_int_field("id");
    schema.add_field("dataval", table_schema.field_type(field_name)?);
    let layout = Layout::new(schema)?;
    if layout.slot_size() > block_size {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "index record does not fit in a block",
        ));
    }
    Ok(layout)
}
