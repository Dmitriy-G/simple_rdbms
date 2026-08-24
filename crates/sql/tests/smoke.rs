use sql::{Expr, Lexer, Parser, Token, TokenKind};
use types::Value;

#[test]
fn lexer_and_parser_construct() {
    let _lexer = Lexer::new("SELECT 1;");
    let _parser = Parser::new(vec![Token::new(TokenKind::Eof, 0)]);
}

#[test]
fn ast_expr_constructs() {
    let expr = Expr::Literal(Value::Integer(1));
    assert_eq!(expr, Expr::Literal(Value::Integer(1)));
}
