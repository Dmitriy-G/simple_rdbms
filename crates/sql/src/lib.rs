//! SQL front end: turns SQL text into an AST the `planner` can bind.
//!
//! The pipeline is the classic two stages: `Lexer` turns source text into a
//! flat `Token` stream, and `Parser` is a hand-written recursive-descent
//! parser that turns that stream into a `Statement`. There is deliberately
//! no parser-generator or third-party SQL grammar here — writing the
//! grammar by hand is the point of this crate.

#![forbid(unsafe_code)]

mod ast;
mod error;
mod lexer;
mod parser;
mod token;

pub use ast::{
    BinaryOperator, ColumnDef, CreateTableStatement, Expr, InsertStatement, SelectItem,
    SelectStatement, Statement, UnaryOperator,
};
pub use error::SqlError;
pub use lexer::Lexer;
pub use parser::Parser;
pub use token::{Token, TokenKind};
