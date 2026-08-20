//! The value system shared by the SQL layer, catalog, and storage engine:
//! the set of column types (`DataType`), the runtime values that populate
//! tuples (`Value`), and the traits used to encode/decode tuples to and
//! from the byte representation stored on disk.

#![forbid(unsafe_code)]

mod data_type;
mod tuple;
mod value;

pub use data_type::DataType;
pub use tuple::{Decode, Encode, Tuple};
pub use value::Value;
