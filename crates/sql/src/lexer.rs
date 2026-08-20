use crate::error::SqlError;
use crate::token::Token;

/// Turns SQL source text into a flat stream of `Token`s. Holds no
/// knowledge of grammar; a `Parser` is what gives the token stream
/// structure.
pub struct Lexer<'a> {
    #[allow(dead_code)]
    input: &'a str,
    #[allow(dead_code)]
    offset: usize,
}

impl<'a> Lexer<'a> {
    /// Creates a lexer over `input`.
    pub fn new(input: &'a str) -> Self {
        Self { input, offset: 0 }
    }

    /// Consumes the lexer, producing every token in `input` followed by a
    /// trailing `TokenKind::Eof`.
    pub fn tokenize(self) -> Result<Vec<Token>, SqlError> {
        todo!("scan input byte by byte, skipping whitespace/comments, emitting tokens")
    }
}
