use crate::error::SqlError;
use crate::token::{Token, TokenKind};

pub struct Lexer<'a> {
    input: &'a str,
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(input: &'a str) -> Self {
        Self { input, bytes: input.as_bytes(), offset: 0 }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, SqlError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_trivia();
            let start = self.offset;
            let kind = match self.peek_byte() {
                None => {
                    tokens.push(Token::new(TokenKind::Eof, start));
                    break;
                }
                Some(b) if b.is_ascii_digit() => self.lex_number()?,
                Some(b'\'') => self.lex_string(start)?,
                Some(b) if b == b'_' || b.is_ascii_alphabetic() => self.lex_ident_or_keyword(),
                Some(_) => self.lex_punct(start)?,
            };
            tokens.push(Token::new(kind, start));
        }
        Ok(tokens)
    }

    fn peek_byte(&self) -> Option<u8> {
        self.bytes.get(self.offset).copied()
    }

    fn peek_byte_at(&self, delta: usize) -> Option<u8> {
        self.bytes.get(self.offset + delta).copied()
    }

    fn skip_trivia(&mut self) {
        loop {
            match self.peek_byte() {
                Some(b) if b.is_ascii_whitespace() => self.offset += 1,
                Some(b'-') if self.peek_byte_at(1) == Some(b'-') => {
                    while let Some(b) = self.peek_byte() {
                        if b == b'\n' {
                            break;
                        }
                        self.offset += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn lex_number(&mut self) -> Result<TokenKind, SqlError> {
        let start = self.offset;
        self.advance_while(|b| b.is_ascii_digit());

        let mut is_float = false;
        if self.peek_byte() == Some(b'.')
            && matches!(self.peek_byte_at(1), Some(b) if b.is_ascii_digit())
        {
            is_float = true;
            self.offset += 1;
            self.advance_while(|b| b.is_ascii_digit());
        }

        let text = &self.input[start..self.offset];
        let invalid = || SqlError::InvalidNumericLiteral { text: text.to_string(), offset: start };
        if is_float {
            Ok(TokenKind::FloatLiteral(text.parse().map_err(|_| invalid())?))
        } else {
            Ok(TokenKind::IntegerLiteral(text.parse().map_err(|_| invalid())?))
        }
    }

    fn lex_string(&mut self, start: usize) -> Result<TokenKind, SqlError> {
        self.offset += 1;
        let mut value = String::new();
        loop {
            match self.peek_byte() {
                None => return Err(SqlError::UnterminatedString { offset: start }),
                Some(b'\'') => {
                    if self.peek_byte_at(1) == Some(b'\'') {
                        value.push('\'');
                        self.offset += 2;
                    } else {
                        self.offset += 1;
                        break;
                    }
                }
                Some(_) => {
                    let ch = self.char_at(self.offset);
                    value.push(ch);
                    self.offset += ch.len_utf8();
                }
            }
        }
        Ok(TokenKind::StringLiteral(value))
    }

    fn lex_ident_or_keyword(&mut self) -> TokenKind {
        let start = self.offset;
        self.advance_while(|b| b == b'_' || b.is_ascii_alphanumeric());
        let text = &self.input[start..self.offset];
        match text.to_ascii_uppercase().as_str() {
            "SELECT" => TokenKind::Select,
            "FROM" => TokenKind::From,
            "WHERE" => TokenKind::Where,
            "INSERT" => TokenKind::Insert,
            "INTO" => TokenKind::Into,
            "VALUES" => TokenKind::Values,
            "AS" => TokenKind::As,
            "CREATE" => TokenKind::Create,
            "TABLE" => TokenKind::Table,
            "INDEX" => TokenKind::Index,
            "ON" => TokenKind::On,
            "AND" => TokenKind::And,
            "OR" => TokenKind::Or,
            "NOT" => TokenKind::Not,
            "IS" => TokenKind::Is,
            "NULL" => TokenKind::Null,
            "TRUE" => TokenKind::True,
            "FALSE" => TokenKind::False,
            "BEGIN" => TokenKind::Begin,
            "START" => TokenKind::Start,
            "TRANSACTION" => TokenKind::Transaction,
            "COMMIT" => TokenKind::Commit,
            "ROLLBACK" => TokenKind::Rollback,
            "EXPLAIN" => TokenKind::Explain,
            _ => TokenKind::Identifier(text.to_string()),
        }
    }

    fn lex_punct(&mut self, start: usize) -> Result<TokenKind, SqlError> {
        let b = match self.peek_byte() {
            Some(b) => b,
            None => unreachable!("caller only dispatches here when a byte is present"),
        };
        let kind = match b {
            b',' => {
                self.offset += 1;
                TokenKind::Comma
            }
            b';' => {
                self.offset += 1;
                TokenKind::Semicolon
            }
            b'(' => {
                self.offset += 1;
                TokenKind::LParen
            }
            b')' => {
                self.offset += 1;
                TokenKind::RParen
            }
            b'*' => {
                self.offset += 1;
                TokenKind::Star
            }
            b'.' => {
                self.offset += 1;
                TokenKind::Dot
            }
            b'+' => {
                self.offset += 1;
                TokenKind::Plus
            }
            b'-' => {
                self.offset += 1;
                TokenKind::Minus
            }
            b'/' => {
                self.offset += 1;
                TokenKind::Slash
            }
            b'=' => {
                self.offset += 1;
                TokenKind::Eq
            }
            b'<' => {
                self.offset += 1;
                match self.peek_byte() {
                    Some(b'=') => {
                        self.offset += 1;
                        TokenKind::LtEq
                    }
                    Some(b'>') => {
                        self.offset += 1;
                        TokenKind::NotEq
                    }
                    _ => TokenKind::Lt,
                }
            }
            b'>' => {
                self.offset += 1;
                if self.peek_byte() == Some(b'=') {
                    self.offset += 1;
                    TokenKind::GtEq
                } else {
                    TokenKind::Gt
                }
            }
            b'!' if self.peek_byte_at(1) == Some(b'=') => {
                self.offset += 2;
                TokenKind::NotEq
            }
            _ => return Err(SqlError::UnexpectedChar { ch: self.char_at(start), offset: start }),
        };
        Ok(kind)
    }

    fn advance_while(&mut self, pred: impl Fn(u8) -> bool) {
        while matches!(self.peek_byte(), Some(b) if pred(b)) {
            self.offset += 1;
        }
    }

    fn char_at(&self, offset: usize) -> char {
        match self.input[offset..].chars().next() {
            Some(ch) => ch,
            None => unreachable!("offset {offset} is within the source text"),
        }
    }
}

pub(crate) const RESERVED_WORDS: &[&str] = &[
    "ORDER",
    "BY",
    "GROUP",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "ON",
    "USING",
    "UNION",
    "INTERSECT",
    "EXCEPT",
    "RETURNING",
    "SET",
    "WHERE",
    "FROM",
];

pub(crate) fn is_reserved_word(word: &str) -> bool {
    RESERVED_WORDS.iter().any(|reserved| word.eq_ignore_ascii_case(reserved))
}
