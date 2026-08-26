use crate::value::ValueError;
use crate::{DataType, Value};

const NULL_TAG: u8 = 0x00;
const VALUE_TAG: u8 = 0x01;

pub trait MemcomparableEncode {
    fn encode_memcomparable(&self, buf: &mut Vec<u8>) -> Result<(), ValueError>;
}

impl MemcomparableEncode for Value {
    fn encode_memcomparable(&self, buf: &mut Vec<u8>) -> Result<(), ValueError> {
        match self {
            Value::Null => buf.push(NULL_TAG),
            Value::Boolean(v) => {
                buf.push(VALUE_TAG);
                buf.push(u8::from(*v));
            }
            Value::Integer(v) => {
                buf.push(VALUE_TAG);
                buf.extend_from_slice(&((*v as u32) ^ SIGN_BIT_32).to_be_bytes());
            }
            Value::BigInt(v) => {
                buf.push(VALUE_TAG);
                buf.extend_from_slice(&((*v as u64) ^ SIGN_BIT_64).to_be_bytes());
            }
            Value::Double(v) => {
                if v.is_nan() {
                    return Err(ValueError::UnorderableValue {
                        reason: "NaN cannot be encoded as a memcomparable key: it has no \
                                 defined ordering"
                            .to_string(),
                    });
                }
                buf.push(VALUE_TAG);
                buf.extend_from_slice(&order_preserving_bits(*v).to_be_bytes());
            }
            Value::Varchar(s) => {
                buf.push(VALUE_TAG);
                // TODO(M11): composite key encoding - escape 0x00 as 0x00 0xFF and
                // append a 0x00 0x00 terminator here once a key can be more than one
                // concatenated value; see memcomparable.MD's "Concatenation is not safe".
                buf.extend_from_slice(s.as_bytes());
            }
        }
        Ok(())
    }
}

pub fn decode_memcomparable(
    bytes: &[u8],
    data_type: DataType,
) -> Result<(Value, usize), ValueError> {
    let &tag = bytes.first().ok_or_else(|| ValueError::InvalidEncoding {
        reason: "empty buffer, expected a tag byte".to_string(),
    })?;
    if tag == NULL_TAG {
        return Ok((Value::Null, 1));
    }
    if tag != VALUE_TAG {
        return Err(ValueError::InvalidEncoding {
            reason: format!(
                "unknown tag byte {tag:#04x}, expected {NULL_TAG:#04x} or {VALUE_TAG:#04x}"
            ),
        });
    }

    let rest = &bytes[1..];
    match data_type {
        DataType::Boolean => {
            let &b = rest.first().ok_or_else(|| ValueError::InvalidEncoding {
                reason: "truncated boolean value".to_string(),
            })?;
            Ok((Value::Boolean(b != 0), 2))
        }
        DataType::Integer => {
            let bits = read_be_u32(rest).ok_or_else(|| ValueError::InvalidEncoding {
                reason: "truncated integer value".to_string(),
            })? ^ SIGN_BIT_32;
            Ok((Value::Integer(bits as i32), 5))
        }
        DataType::BigInt => {
            let bits = read_be_u64(rest).ok_or_else(|| ValueError::InvalidEncoding {
                reason: "truncated bigint value".to_string(),
            })? ^ SIGN_BIT_64;
            Ok((Value::BigInt(bits as i64), 9))
        }
        DataType::Double => {
            let bits = read_be_u64(rest).ok_or_else(|| ValueError::InvalidEncoding {
                reason: "truncated double value".to_string(),
            })?;
            let original = if bits & SIGN_BIT_64 != 0 { bits & !SIGN_BIT_64 } else { !bits };
            Ok((Value::Double(f64::from_bits(original)), 9))
        }
        DataType::Varchar(_) => {
            let s = String::from_utf8(rest.to_vec()).map_err(|_| ValueError::InvalidEncoding {
                reason: "invalid utf-8 in varchar key".to_string(),
            })?;
            let consumed = 1 + s.len();
            Ok((Value::Varchar(s), consumed))
        }
    }
}

const SIGN_BIT_32: u32 = 1 << 31;
const SIGN_BIT_64: u64 = 1 << 63;

fn read_be_u32(bytes: &[u8]) -> Option<u32> {
    let b = bytes.get(0..4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn read_be_u64(bytes: &[u8]) -> Option<u64> {
    let b = bytes.get(0..8)?;
    Some(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
}

fn order_preserving_bits(v: f64) -> u64 {
    let v = if v == 0.0 { 0.0 } else { v };
    let bits = v.to_bits();
    if bits & SIGN_BIT_64 != 0 { !bits } else { bits | SIGN_BIT_64 }
}
