use std::io::{self, Cursor, Error, ErrorKind, Read};

use crate::file::BlockId;

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
        tx: u64,
        block: BlockId,
        offset: usize,
        value: Value,
    },
}

impl LogRecord {
    pub(super) fn tx(&self) -> Option<u64> {
        match self {
            Self::Checkpoint => None,
            Self::Start(tx) | Self::Commit(tx) | Self::Rollback(tx) | Self::Set { tx, .. } => {
                Some(*tx)
            }
        }
    }

    pub(super) fn encode(&self) -> io::Result<Vec<u8>> {
        let op: i32 = match self {
            Self::Checkpoint => 0,
            Self::Start(_) => 1,
            Self::Commit(_) => 2,
            Self::Rollback(_) => 3,
            Self::Set {
                value: Value::Int(_),
                ..
            } => 4,
            Self::Set {
                value: Value::String(_),
                ..
            } => 5,
        };
        let mut bytes = op.to_le_bytes().to_vec();
        if let Some(tx) = self.tx() {
            bytes.extend(tx.to_le_bytes());
        }
        if let Self::Set {
            block,
            offset,
            value,
            ..
        } = self
        {
            put_string(&mut bytes, block.filename())?;
            bytes.extend(block.number().to_le_bytes());
            bytes.extend((*offset as u64).to_le_bytes());
            match value {
                Value::Int(value) => bytes.extend(value.to_le_bytes()),
                Value::String(value) => put_string(&mut bytes, value)?,
            }
        }
        Ok(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let mut input = Cursor::new(bytes);
        let op = i32::from_le_bytes(read(&mut input)?);
        let record = if op == 0 {
            Self::Checkpoint
        } else {
            let tx = u64::from_le_bytes(read(&mut input)?);
            match op {
                1 => Self::Start(tx),
                2 => Self::Commit(tx),
                3 => Self::Rollback(tx),
                4 | 5 => {
                    let filename = get_string(&mut input)?;
                    let block = BlockId::new(filename, u64::from_le_bytes(read(&mut input)?));
                    let offset = usize::try_from(u64::from_le_bytes(read(&mut input)?))
                        .map_err(|_| Error::new(ErrorKind::InvalidData, "log offset overflow"))?;
                    let value = if op == 4 {
                        Value::Int(i32::from_le_bytes(read(&mut input)?))
                    } else {
                        Value::String(get_string(&mut input)?)
                    };
                    Self::Set {
                        tx,
                        block,
                        offset,
                        value,
                    }
                }
                _ => return Err(Error::new(ErrorKind::InvalidData, "unknown log operation")),
            }
        };
        if input.position() != bytes.len() as u64 {
            return Err(Error::new(ErrorKind::InvalidData, "trailing log bytes"));
        }
        Ok(record)
    }
}

fn read<const N: usize>(input: &mut Cursor<&[u8]>) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> io::Result<()> {
    let length = i32::try_from(value.len())
        .map_err(|_| Error::new(ErrorKind::InvalidInput, "log string too long"))?;
    bytes.extend(length.to_le_bytes());
    bytes.extend(value.as_bytes());
    Ok(())
}

fn get_string(input: &mut Cursor<&[u8]>) -> io::Result<String> {
    let length = usize::try_from(i32::from_le_bytes(read(input)?))
        .map_err(|_| Error::new(ErrorKind::InvalidData, "negative log string length"))?;
    let start = input.position() as usize;
    let end = start
        .checked_add(length)
        .filter(|&end| end <= input.get_ref().len())
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "log string exceeds record"))?;
    let value = std::str::from_utf8(&input.get_ref()[start..end])
        .map_err(|error| Error::new(ErrorKind::InvalidData, error))?
        .to_owned();
    input.set_position(end as u64);
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_records_are_rejected() -> io::Result<()> {
        let record = LogRecord::Set {
            tx: 1,
            block: BlockId::new("test.tbl", 0),
            offset: 40,
            value: Value::String("元の値".to_owned()),
        }
        .encode()?;

        for end in 0..record.len() {
            assert!(LogRecord::decode(&record[..end]).is_err());
        }

        let mut invalid = record.clone();
        invalid[..4].copy_from_slice(&99_i32.to_le_bytes());
        assert!(LogRecord::decode(&invalid).is_err());

        invalid = record.clone();
        invalid[12..16].copy_from_slice(&(-1_i32).to_le_bytes());
        assert!(LogRecord::decode(&invalid).is_err());

        invalid = record;
        invalid.push(0);
        assert!(LogRecord::decode(&invalid).is_err());

        Ok(())
    }
}
