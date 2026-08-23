use crate::{DataType, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Tuple {
    values: Vec<Value>,
}

impl Tuple {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    pub fn decode(buf: &[u8], column_types: &[DataType]) -> Result<Tuple, TupleError> {
        let bitmap_len = column_types.len().div_ceil(8);
        let bitmap = take(buf, 0, bitmap_len)?;
        let mut offset = bitmap_len;
        let mut values = Vec::with_capacity(column_types.len());

        for (i, data_type) in column_types.iter().enumerate() {
            if bitmap[i / 8] & (1 << (i % 8)) != 0 {
                values.push(Value::Null);
                continue;
            }
            let value = match data_type {
                DataType::Boolean => {
                    let bytes = take(buf, offset, 1)?;
                    offset += 1;
                    Value::Boolean(bytes[0] != 0)
                }
                DataType::Integer => {
                    let bytes = take(buf, offset, 4)?;
                    offset += 4;
                    Value::Integer(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
                }
                DataType::BigInt => {
                    let bytes = take(buf, offset, 8)?;
                    offset += 8;
                    Value::BigInt(i64::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]))
                }
                DataType::Double => {
                    let bytes = take(buf, offset, 8)?;
                    offset += 8;
                    Value::Double(f64::from_le_bytes([
                        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6],
                        bytes[7],
                    ]))
                }
                DataType::Varchar(_) => {
                    let len_bytes = take(buf, offset, 4)?;
                    let len = u32::from_le_bytes([
                        len_bytes[0],
                        len_bytes[1],
                        len_bytes[2],
                        len_bytes[3],
                    ]) as usize;
                    offset += 4;
                    let str_bytes = take(buf, offset, len)?;
                    offset += len;
                    Value::Varchar(String::from_utf8(str_bytes.to_vec())?)
                }
            };
            values.push(value);
        }

        Ok(Tuple { values })
    }
}

fn take(buf: &[u8], offset: usize, len: usize) -> Result<&[u8], TupleError> {
    buf.get(offset..offset + len)
        .ok_or(TupleError::Truncated { expected: offset + len, found: buf.len() })
}

#[derive(Debug, thiserror::Error)]
pub enum TupleError {
    #[error("truncated tuple: expected at least {expected} bytes, found {found}")]
    Truncated { expected: usize, found: usize },
    #[error("invalid utf-8 in varchar column: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

pub trait Encode {
    fn encode(&self, buf: &mut Vec<u8>);
}

pub trait Decode: Sized {
    type Error;

    fn decode(buf: &[u8]) -> Result<(Self, usize), Self::Error>;
}

impl Encode for Tuple {
    fn encode(&self, buf: &mut Vec<u8>) {
        let bitmap_len = self.values.len().div_ceil(8);
        let bitmap_start = buf.len();
        buf.resize(bitmap_start + bitmap_len, 0u8);

        for (i, value) in self.values.iter().enumerate() {
            match value {
                Value::Null => {
                    buf[bitmap_start + i / 8] |= 1 << (i % 8);
                }
                Value::Boolean(b) => buf.push(u8::from(*b)),
                Value::Integer(v) => buf.extend_from_slice(&v.to_le_bytes()),
                Value::BigInt(v) => buf.extend_from_slice(&v.to_le_bytes()),
                Value::Double(v) => buf.extend_from_slice(&v.to_le_bytes()),
                Value::Varchar(s) => {
                    let bytes = s.as_bytes();
                    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    buf.extend_from_slice(bytes);
                }
            }
        }
    }
}
