use crate::Value;

/// An ordered row of values, in schema column order. This is the in-memory
/// form the executor passes between operators; `storage` deals in the
/// encoded byte form produced by `Encode`/`Decode`.
#[derive(Debug, Clone, PartialEq)]
pub struct Tuple {
    values: Vec<Value>,
}

impl Tuple {
    /// Builds a tuple from already-evaluated values.
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    /// Borrows the tuple's values in column order.
    pub fn values(&self) -> &[Value] {
        &self.values
    }
}

/// Converts a value into its on-disk byte representation, as stored inside
/// a slotted page. Implemented per `DataType` so the storage layer can lay
/// fixed-width columns out contiguously and variable-length columns
/// out-of-line with a length prefix.
pub trait Encode {
    /// Appends this value's encoded bytes to `buf`.
    fn encode(&self, buf: &mut Vec<u8>);
}

/// Reconstructs a value from its on-disk byte representation. The inverse
/// of `Encode`.
pub trait Decode: Sized {
    /// Errors that can occur while decoding, e.g. truncated or malformed
    /// input.
    type Error;

    /// Reads a value out of `buf`, returning the value and the number of
    /// bytes consumed.
    fn decode(buf: &[u8]) -> Result<(Self, usize), Self::Error>;
}

impl Encode for Tuple {
    fn encode(&self, _buf: &mut Vec<u8>) {
        todo!("encode each value in column order, using a null bitmap for nullable columns")
    }
}
