use thiserror::Error;

#[derive(Debug, Error)]
pub enum SqlError {
    #[error("unexpected character '{ch}' at offset {offset}")]
    UnexpectedChar { ch: char, offset: usize },

    #[error("unterminated string literal starting at offset {offset}")]
    UnterminatedString { offset: usize },

    #[error("unexpected token {found:?} at offset {offset}, expected {expected}")]
    UnexpectedToken { expected: String, found: String, offset: usize },

    #[error("unexpected end of input, expected {expected}")]
    UnexpectedEof { expected: String },

    #[error("invalid numeric literal '{text}' at offset {offset}")]
    InvalidNumericLiteral { text: String, offset: usize },
}

impl SqlError {
    pub fn offset(&self, source: &str) -> usize {
        match self {
            SqlError::UnexpectedChar { offset, .. }
            | SqlError::UnterminatedString { offset }
            | SqlError::UnexpectedToken { offset, .. }
            | SqlError::InvalidNumericLiteral { offset, .. } => *offset,
            SqlError::UnexpectedEof { .. } => source.len(),
        }
    }

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
