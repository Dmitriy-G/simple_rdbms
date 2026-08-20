use thiserror::Error;

/// Errors raised while lexing or parsing SQL text.
#[derive(Debug, Error)]
pub enum SqlError {
    /// The lexer found a byte that cannot start any valid token.
    #[error("unexpected character '{ch}' at offset {offset}")]
    UnexpectedChar {
        /// The offending character.
        ch: char,
        /// Its byte offset in the source text.
        offset: usize,
    },

    /// A string or quoted identifier was never closed before end of input.
    #[error("unterminated string literal starting at offset {offset}")]
    UnterminatedString {
        /// The byte offset of the opening quote.
        offset: usize,
    },

    /// The parser found a token where a different construct was expected.
    #[error("unexpected token {found:?} at offset {offset}, expected {expected}")]
    UnexpectedToken {
        /// A description of what the grammar expected at this point.
        expected: String,
        /// The token kind actually found, formatted for display.
        found: String,
        /// The offending token's byte offset.
        offset: usize,
    },

    /// The token stream ended while the parser still expected more input.
    #[error("unexpected end of input, expected {expected}")]
    UnexpectedEof {
        /// A description of what the grammar expected next.
        expected: String,
    },
}

impl From<SqlError> for common::Error {
    fn from(err: SqlError) -> Self {
        common::Error::Parse(err.to_string())
    }
}
