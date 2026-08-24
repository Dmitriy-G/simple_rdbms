#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Select,
    From,
    Where,
    Insert,
    Into,
    Values,
    Create,
    Table,
    And,
    Or,
    Not,
    Null,
    True,
    False,
    Begin,
    Start,
    Transaction,
    Commit,
    Rollback,

    Identifier(String),
    IntegerLiteral(i64),
    FloatLiteral(f64),
    StringLiteral(String),

    Comma,
    Semicolon,
    LParen,
    RParen,
    Star,
    Dot,
    Plus,
    Minus,
    Slash,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub offset: usize,
}

impl Token {
    pub fn new(kind: TokenKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}
