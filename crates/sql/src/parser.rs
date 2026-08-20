use crate::ast::{CreateTableStatement, Expr, InsertStatement, SelectStatement, Statement};
use crate::error::SqlError;
use crate::token::Token;

/// A hand-written recursive-descent parser over a `Token` stream, producing
/// a `Statement` AST. Each grammar production gets its own method, named
/// after the production it parses, and consumes tokens by advancing an
/// internal cursor rather than by recursing over the input text.
pub struct Parser {
    #[allow(dead_code)]
    tokens: Vec<Token>,
    #[allow(dead_code)]
    pos: usize,
}

impl Parser {
    /// Creates a parser over a complete token stream (as produced by
    /// `Lexer::tokenize`).
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    /// Parses exactly one statement, terminated by `;` or end of input.
    pub fn parse(&mut self) -> Result<Statement, SqlError> {
        todo!("dispatch on the leading keyword to parse_select/parse_insert/parse_create_table")
    }

    /// Parses a `SELECT` statement.
    #[allow(dead_code)]
    fn parse_select(&mut self) -> Result<SelectStatement, SqlError> {
        todo!("parse the select list, FROM clause, and optional WHERE clause")
    }

    /// Parses an `INSERT INTO` statement.
    #[allow(dead_code)]
    fn parse_insert(&mut self) -> Result<InsertStatement, SqlError> {
        todo!("parse the target table, optional column list, and VALUES rows")
    }

    /// Parses a `CREATE TABLE` statement.
    #[allow(dead_code)]
    fn parse_create_table(&mut self) -> Result<CreateTableStatement, SqlError> {
        todo!("parse the table name and parenthesized column definition list")
    }

    /// Parses a expression, respecting operator precedence and
    /// associativity via precedence climbing.
    #[allow(dead_code)]
    fn parse_expr(&mut self) -> Result<Expr, SqlError> {
        todo!("precedence-climb through OR, AND, comparisons, +/-, */:, and unary/primary")
    }
}
