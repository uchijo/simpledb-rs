/// Snapshot estimates for the planner, as in chapter 7.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatInfo {
    block_count: u64,
    record_count: u64,
}

impl StatInfo {
    pub fn new(block_count: u64, record_count: u64) -> Self {
        Self {
            block_count,
            record_count,
        }
    }

    /// Estimated scan blocks; chapter 7 excludes empty trailing blocks.
    pub fn blocks_accessed(&self) -> u64 {
        self.block_count
    }

    pub fn records_output(&self) -> u64 {
        self.record_count
    }

    pub fn distinct_values(&self, _field_name: &str) -> u64 {
        // shortcut: the book estimates 1/3 of the records; count values when refining the planner.
        1 + self.record_count / 3
    }
}
