use crate::Value;

const NULL_TAG: u8 = 0x00;
const VALUE_TAG: u8 = 0x01;

pub trait MemcomparableEncode {
    fn encode_memcomparable(&self, buf: &mut Vec<u8>);
}

impl MemcomparableEncode for Value {
    fn encode_memcomparable(&self, buf: &mut Vec<u8>) {
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
                buf.push(VALUE_TAG);
                buf.extend_from_slice(&order_preserving_bits(*v).to_be_bytes());
            }
            Value::Varchar(s) => {
                buf.push(VALUE_TAG);
                buf.extend_from_slice(s.as_bytes());
            }
        }
    }
}

const SIGN_BIT_32: u32 = 1 << 31;
const SIGN_BIT_64: u64 = 1 << 63;

fn order_preserving_bits(v: f64) -> u64 {
    let v = if v == 0.0 { 0.0 } else { v };
    let bits = v.to_bits();
    if bits & SIGN_BIT_64 != 0 { !bits } else { bits | SIGN_BIT_64 }
}
