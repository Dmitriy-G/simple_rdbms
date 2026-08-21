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

    /// A numeric literal's text could not be parsed as the numeric type its
    /// shape implies (an integer literal too large for `i64`, or a float
    /// literal outside `f64`'s range), rather than being silently replaced
    /// with a sentinel value.
    #[error("invalid numeric literal '{text}' at offset {offset}")]
    InvalidNumericLiteral {
        /// The exact source text the user typed.
        text: String,
        /// The byte offset where the literal starts.
        offset: usize,
    },
}

impl SqlError {
    /// The byte offset in the source text this error points at. An
    /// `UnexpectedEof` has no token of its own to point at, so it points
    /// one past the end of `source`.
    pub fn offset(&self, source: &str) -> usize {
        match self {
            SqlError::UnexpectedChar { offset, .. }
            | SqlError::UnterminatedString { offset }
            | SqlError::UnexpectedToken { offset, .. }
            | SqlError::InvalidNumericLiteral { offset, .. } => *offset,
            SqlError::UnexpectedEof { .. } => source.len(),
        }
    }

    /// Renders this error against the `source` it came from as a message
    /// followed by the offending line and a caret pointing at the exact
    /// byte offset.
    pub fn render(&self, source: &str) -> String {
        let offset = self.offset(source).min(source.len());
        let line_start = source[..offset].rfind('\n').map_or(0, |i| i + 1);
        let line_end = source[offset..].find('\n').map_or(source.len(), |i| offset + i);
        let line = &source[line_start..line_end];
        let column = source[line_start..offset].chars().count();
        let caret = format!("{}^", " ".repeat(column));
        format!("{self}\n{line}\n{caret}")
    }
}

impl From<SqlError> for common::Error {
    fn from(err: SqlError) -> Self {
        common::Error::Parse(err.to_string())
    }
}
