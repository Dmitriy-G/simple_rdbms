#![forbid(unsafe_code)]

mod data_type;
mod memcomparable;
mod tuple;
mod value;

pub use data_type::DataType;
pub use memcomparable::{MemcomparableEncode, decode_memcomparable};
pub use tuple::{Decode, Encode, Tuple, TupleError};
pub use value::{Value, ValueError};
