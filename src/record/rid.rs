use std::fmt;

/// A record's physical location within a table file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rid {
    block_number: u64,
    slot: usize,
}

impl Rid {
    pub fn new(block_number: u64, slot: usize) -> Self {
        Self { block_number, slot }
    }

    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    pub fn slot(&self) -> usize {
        self.slot
    }
}

impl fmt::Display for Rid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "[{}, {}]", self.block_number, self.slot)
    }
}
