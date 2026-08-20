/// The kind of a lexical token, carrying any literal value the token
/// represents.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    /// `SELECT`
    Select,
    /// `FROM`
    From,
    /// `WHERE`
    Where,
    /// `INSERT`
    Insert,
    /// `INTO`
    Into,
    /// `VALUES`
    Values,
    /// `CREATE`
    Create,
    /// `TABLE`
    Table,
    /// `AND`
    And,
    /// `OR`
    Or,
    /// `NOT`
    Not,
    /// `NULL`
    Null,
    /// `TRUE`
    True,
    /// `FALSE`
    False,

    // Literals and identifiers
    /// A bare identifier: a table, column, or type name.
    Identifier(String),
    /// An integer literal, before it is narrowed to a `types::DataType`.
    IntegerLiteral(i64),
    /// A floating-point literal.
    FloatLiteral(f64),
    /// A single-quoted string literal, with quoting already stripped.
    StringLiteral(String),

    // Punctuation and operators
    /// `,`
    Comma,
    /// `;`
    Semicolon,
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `*`
    Star,
    /// `.`
    Dot,
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `/`
    Slash,
    /// `=`
    Eq,
    /// `<>` or `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,

    /// End of input, always the final token in a token stream.
    Eof,
}

/// One lexical token: its kind plus the source position it started at, used
/// to report parse errors with a location.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// The token's kind and any literal payload.
    pub kind: TokenKind,
    /// The byte offset into the source text where this token starts.
    pub offset: usize,
}

impl Token {
    /// Builds a token of `kind` starting at byte `offset`.
    pub fn new(kind: TokenKind, offset: usize) -> Self {
        Self { kind, offset }
    }
}
