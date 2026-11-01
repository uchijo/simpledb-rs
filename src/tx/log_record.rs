use std::io::{self, Cursor, Error, ErrorKind, Read};

use crate::file::BlockId;

const CHECKPOINT: i32 = 0;
const START: i32 = 1;
const COMMIT: i32 = 2;
const ROLLBACK: i32 = 3;
const SET_INT: i32 = 4;
const SET_STRING: i32 = 5;

pub(super) enum Value {
    Int(i32),
    String(String),
}

pub(super) enum LogRecord {
    Checkpoint,
    Start(u64),
    Commit(u64),
    Rollback(u64),
    Set {
        transaction_id: u64,
        block: BlockId,
        offset: usize,
        old_value: Value,
    },
}

impl LogRecord {
    pub(super) fn transaction_id(&self) -> Option<u64> {
        match self {
            Self::Checkpoint => None,
            Self::Start(transaction_id)
            | Self::Commit(transaction_id)
            | Self::Rollback(transaction_id)
            | Self::Set { transaction_id, .. } => Some(*transaction_id),
        }
    }

    pub(super) fn encode(&self) -> io::Result<Vec<u8>> {
        let operation: i32 = match self {
            Self::Checkpoint => CHECKPOINT,
            Self::Start(_) => START,
            Self::Commit(_) => COMMIT,
            Self::Rollback(_) => ROLLBACK,
            Self::Set {
                old_value: Value::Int(_),
                ..
            } => SET_INT,
            Self::Set {
                old_value: Value::String(_),
                ..
            } => SET_STRING,
        };
        let mut bytes = operation.to_le_bytes().to_vec();
        if let Some(transaction_id) = self.transaction_id() {
            bytes.extend(transaction_id.to_le_bytes());
        }
        if let Self::Set {
            block,
            offset,
            old_value,
            ..
        } = self
        {
            write_string(&mut bytes, block.filename())?;
            bytes.extend(block.number().to_le_bytes());
            bytes.extend((*offset as u64).to_le_bytes());
            match old_value {
                Value::Int(old_value) => bytes.extend(old_value.to_le_bytes()),
                Value::String(old_value) => write_string(&mut bytes, old_value)?,
            }
        }
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut input = Cursor::new(bytes);
        let operation = i32::from_le_bytes(read_array(&mut input)?);
        let record = match operation {
            CHECKPOINT => Self::Checkpoint,
            START => Self::Start(u64::from_le_bytes(read_array(&mut input)?)),
            COMMIT => Self::Commit(u64::from_le_bytes(read_array(&mut input)?)),
            ROLLBACK => Self::Rollback(u64::from_le_bytes(read_array(&mut input)?)),
            SET_INT | SET_STRING => {
                let transaction_id = u64::from_le_bytes(read_array(&mut input)?);
                let filename = read_string(&mut input)?;
                let block_number = u64::from_le_bytes(read_array(&mut input)?);
                let block = BlockId::new(filename, block_number);
                let offset = usize::try_from(u64::from_le_bytes(read_array(&mut input)?))
                    .map_err(|_| Error::new(ErrorKind::InvalidData, "log offset overflow"))?;
                let old_value = match operation {
                    SET_INT => Value::Int(i32::from_le_bytes(read_array(&mut input)?)),
                    _ => Value::String(read_string(&mut input)?),
                };
                Self::Set {
                    transaction_id,
                    block,
                    offset,
                    old_value,
                }
            }
            _ => return Err(Error::new(ErrorKind::InvalidData, "unknown log operation")),
        };

        if input.position() != bytes.len() as u64 {
            return Err(Error::new(ErrorKind::InvalidData, "trailing log bytes"));
        }
        Ok(record)
    }
}

fn read_array<const N: usize>(input: &mut Cursor<&[u8]>) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn write_string(bytes: &mut Vec<u8>, text: &str) -> io::Result<()> {
    let length = i32::try_from(text.len())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "log string too long"))?;
    bytes.extend(length.to_le_bytes());
    bytes.extend(text.as_bytes());
    Ok(())
}

fn read_string(input: &mut Cursor<&[u8]>) -> io::Result<String> {
    let length = usize::try_from(i32::from_le_bytes(read_array(input)?))
        .map_err(|_| Error::new(ErrorKind::InvalidData, "negative log string length"))?;
    let start = input.position() as usize;
    let end = start
        .checked_add(length)
        .filter(|&end| end <= input.get_ref().len())
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "log string exceeds record"))?;
    let text = std::str::from_utf8(&input.get_ref()[start..end])
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?
        .to_owned();
    input.set_position(end as u64);
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_records_are_rejected() -> io::Result<()> {
        let record = LogRecord::Set {
            transaction_id: 1,
            block: BlockId::new("test.tbl", 0),
            offset: 40,
            old_value: Value::String("元の値".to_owned()),
        }
        .encode()?;

        for end in 0..record.len() {
            assert!(LogRecord::decode(&record[..end]).is_err());
        }

        let mut invalid = record.clone();
        invalid[..size_of::<i32>()].copy_from_slice(&99_i32.to_le_bytes());
        assert!(LogRecord::decode(&invalid).is_err());

        invalid = record.clone();
        let filename_length_offset = size_of::<i32>() + size_of::<u64>();
        let filename_start_offset = filename_length_offset + size_of::<i32>();
        invalid[filename_length_offset..filename_start_offset]
            .copy_from_slice(&(-1_i32).to_le_bytes());
        assert!(LogRecord::decode(&invalid).is_err());

        invalid = record;
        invalid.push(0);
        assert!(LogRecord::decode(&invalid).is_err());

        Ok(())
    }
}
