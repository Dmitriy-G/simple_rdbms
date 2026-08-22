//! Parses each statement form and asserts on the resulting AST, checks
//! operator precedence, and checks that malformed input produces a located
//! parse error (not a panic).

use sql::{
    BinaryOperator, ColumnDef, CreateTableStatement, Expr, InsertStatement, Lexer, Parser,
    SelectItem, SelectStatement, SqlError, Statement, UnaryOperator,
};
use types::{DataType, Value};

fn try_parse(source: &str) -> Result<Statement, SqlError> {
    let tokens = Lexer::new(source).tokenize()?;
    Parser::new(tokens).parse()
}

fn parse(source: &str) -> Statement {
    match try_parse(source) {
        Ok(stmt) => stmt,
        Err(err) => panic!("unexpected parse error for {source:?}: {err}"),
    }
}

fn parse_err(source: &str) -> SqlError {
    match try_parse(source) {
        Ok(stmt) => panic!("expected a parse error for {source:?}, got {stmt:?}"),
        Err(err) => err,
    }
}

fn byte_offset(source: &str, needle: char) -> usize {
    match source.find(needle) {
        Some(i) => i,
        None => panic!("{needle:?} not found in {source:?}"),
    }
}

#[test]
fn parses_create_table() {
    let stmt = parse("CREATE TABLE users (id INTEGER, name TEXT, active BOOLEAN)");
    assert_eq!(
        stmt,
        Statement::CreateTable(CreateTableStatement {
            table: "users".to_string(),
            columns: vec![
                ColumnDef { name: "id".to_string(), data_type: DataType::Integer, nullable: true },
                ColumnDef {
                    name: "name".to_string(),
                    data_type: DataType::Varchar(u32::MAX),
                    nullable: true,
                },
                ColumnDef {
                    name: "active".to_string(),
                    data_type: DataType::Boolean,
                    nullable: true
                },
            ],
        })
    );
}

#[test]
fn create_table_type_names_are_case_insensitive() {
    let stmt = parse("create table t (a int)");
    assert_eq!(
        stmt,
        Statement::CreateTable(CreateTableStatement {
            table: "t".to_string(),
            columns: vec![ColumnDef {
                name: "a".to_string(),
                data_type: DataType::Integer,
                nullable: true,
            }],
        })
    );
}

#[test]
fn parses_insert_with_explicit_columns_and_multiple_rows() {
    let stmt = parse("INSERT INTO t (a, b) VALUES (1, 'x'), (2, 'y')");
    assert_eq!(
        stmt,
        Statement::Insert(InsertStatement {
            table: "t".to_string(),
            columns: vec!["a".to_string(), "b".to_string()],
            values: vec![
                vec![
                    Expr::Literal(Value::BigInt(1)),
                    Expr::Literal(Value::Varchar("x".to_string()))
                ],
                vec![
                    Expr::Literal(Value::BigInt(2)),
                    Expr::Literal(Value::Varchar("y".to_string()))
                ],
            ],
        })
    );
}

#[test]
fn parses_insert_without_column_list() {
    let stmt = parse("INSERT INTO t VALUES (1, TRUE, NULL)");
    assert_eq!(
        stmt,
        Statement::Insert(InsertStatement {
            table: "t".to_string(),
            columns: vec![],
            values: vec![vec![
                Expr::Literal(Value::BigInt(1)),
                Expr::Literal(Value::Boolean(true)),
                Expr::Literal(Value::Null),
            ]],
        })
    );
}

#[test]
fn parses_select_wildcard() {
    let stmt = parse("SELECT * FROM t");
    assert_eq!(
        stmt,
        Statement::Select(SelectStatement {
            items: vec![SelectItem::Wildcard],
            from: "t".to_string(),
            where_clause: None,
        })
    );
}

#[test]
fn parses_select_list_and_where_clause() {
    let stmt = parse("SELECT a, b FROM t WHERE a = 1");
    assert_eq!(
        stmt,
        Statement::Select(SelectStatement {
            items: vec![
                SelectItem::Expr(Expr::Column("a".to_string())),
                SelectItem::Expr(Expr::Column("b".to_string())),
            ],
            from: "t".to_string(),
            where_clause: Some(Expr::BinaryOp {
                left: Box::new(Expr::Column("a".to_string())),
                op: BinaryOperator::Eq,
                right: Box::new(Expr::Literal(Value::BigInt(1))),
            }),
        })
    );
}

#[test]
fn precedence_or_binds_looser_than_and() {
    // a = 1 OR b = 2 AND c = 3  ==  a=1 OR (b=2 AND c=3)
    let stmt = parse("SELECT * FROM t WHERE a = 1 OR b = 2 AND c = 3");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };

    let eq = |col: &str, v: i64| Expr::BinaryOp {
        left: Box::new(Expr::Column(col.to_string())),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Literal(Value::BigInt(v))),
    };
    let expected = Expr::BinaryOp {
        left: Box::new(eq("a", 1)),
        op: BinaryOperator::Or,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(eq("b", 2)),
            op: BinaryOperator::And,
            right: Box::new(eq("c", 3)),
        }),
    };
    assert_eq!(select.where_clause, Some(expected));
}

#[test]
fn parentheses_override_precedence() {
    // (a = 1 OR b = 2) AND c = 3
    let stmt = parse("SELECT * FROM t WHERE (a = 1 OR b = 2) AND c = 3");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };

    let eq = |col: &str, v: i64| Expr::BinaryOp {
        left: Box::new(Expr::Column(col.to_string())),
        op: BinaryOperator::Eq,
        right: Box::new(Expr::Literal(Value::BigInt(v))),
    };
    let expected = Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(eq("a", 1)),
            op: BinaryOperator::Or,
            right: Box::new(eq("b", 2)),
        }),
        op: BinaryOperator::And,
        right: Box::new(eq("c", 3)),
    };
    assert_eq!(select.where_clause, Some(expected));
}

#[test]
fn unary_not_and_negate() {
    let stmt = parse("SELECT * FROM t WHERE NOT a AND b = -1");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    let expected = Expr::BinaryOp {
        left: Box::new(Expr::UnaryOp {
            op: UnaryOperator::Not,
            expr: Box::new(Expr::Column("a".to_string())),
        }),
        op: BinaryOperator::And,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(Expr::Column("b".to_string())),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::UnaryOp {
                op: UnaryOperator::Negate,
                expr: Box::new(Expr::Literal(Value::BigInt(1))),
            }),
        }),
    };
    assert_eq!(select.where_clause, Some(expected));
}

#[test]
fn string_literal_escapes_doubled_quote() {
    let stmt = parse("SELECT * FROM t WHERE a = 'it''s a test'");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(
        select.where_clause,
        Some(Expr::BinaryOp {
            left: Box::new(Expr::Column("a".to_string())),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::Literal(Value::Varchar("it's a test".to_string()))),
        })
    );
}

#[test]
fn keywords_are_case_insensitive_identifiers_are_not() {
    let stmt = parse("select Id from MyTable");
    assert_eq!(
        stmt,
        Statement::Select(SelectStatement {
            items: vec![SelectItem::Expr(Expr::Column("Id".to_string()))],
            from: "MyTable".to_string(),
            where_clause: None,
        })
    );
}

#[test]
fn parses_begin_commit_rollback() {
    assert_eq!(parse("BEGIN"), Statement::Begin);
    assert_eq!(parse("COMMIT"), Statement::Commit);
    assert_eq!(parse("ROLLBACK"), Statement::Rollback);
}

#[test]
fn start_transaction_is_a_synonym_for_begin() {
    assert_eq!(parse("START TRANSACTION"), Statement::Begin);
    assert_eq!(parse("start transaction"), Statement::Begin, "keywords are case-insensitive");
}

#[test]
fn transaction_control_keywords_are_case_insensitive() {
    assert_eq!(parse("begin"), Statement::Begin);
    assert_eq!(parse("commit"), Statement::Commit);
    assert_eq!(parse("rollback"), Statement::Rollback);
}

#[test]
fn start_without_transaction_is_a_parse_error_not_a_panic() {
    let err = parse_err("START");
    assert!(matches!(err, SqlError::UnexpectedEof { .. }));
}

#[test]
fn missing_closing_paren_errors_with_offset_not_panic() {
    let err = parse_err("SELECT * FROM t WHERE (a = 1");
    assert!(matches!(err, SqlError::UnexpectedEof { .. }));
}

#[test]
fn unterminated_string_errors_with_offset() {
    let source = "SELECT * FROM t WHERE a = 'oops";
    match parse_err(source) {
        SqlError::UnterminatedString { offset } => assert_eq!(offset, byte_offset(source, '\'')),
        other => panic!("expected UnterminatedString, got {other:?}"),
    }
}

#[test]
fn trailing_comma_in_select_list_errors_not_panics() {
    let err = parse_err("SELECT a, FROM t");
    assert!(matches!(err, SqlError::UnexpectedToken { .. }));
}

#[test]
fn oversized_integer_literal_is_a_lexer_error_quoting_the_text() {
    let source = "SELECT 99999999999999999999 FROM t";
    match parse_err(source) {
        SqlError::InvalidNumericLiteral { text, offset } => {
            assert_eq!(text, "99999999999999999999");
            assert_eq!(offset, byte_offset(source, '9'));
        }
        other => panic!("expected InvalidNumericLiteral, got {other:?}"),
    }
}

#[test]
fn float_literal_parses_as_a_double_value() {
    let stmt = parse("SELECT * FROM t WHERE a = 1.5");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(
        select.where_clause,
        Some(Expr::BinaryOp {
            left: Box::new(Expr::Column("a".to_string())),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::Literal(Value::Double(1.5))),
        })
    );
}

#[test]
fn parses_bigint_and_double_column_types() {
    let stmt = parse("CREATE TABLE t (a INTEGER, b BIGINT, c DOUBLE)");
    assert_eq!(
        stmt,
        Statement::CreateTable(CreateTableStatement {
            table: "t".to_string(),
            columns: vec![
                ColumnDef { name: "a".to_string(), data_type: DataType::Integer, nullable: true },
                ColumnDef { name: "b".to_string(), data_type: DataType::BigInt, nullable: true },
                ColumnDef { name: "c".to_string(), data_type: DataType::Double, nullable: true },
            ],
        })
    );
}

#[test]
fn unexpected_char_reports_offset() {
    let source = "SELECT * FROM t WHERE a = 1 @ 2";
    match parse_err(source) {
        SqlError::UnexpectedChar { ch, offset } => {
            assert_eq!(ch, '@');
            assert_eq!(offset, byte_offset(source, '@'));
        }
        other => panic!("expected UnexpectedChar, got {other:?}"),
    }
}

#[test]
fn render_points_a_caret_at_the_offending_token() {
    let source = "SELECT * FROM t WHERE a = @";
    let err = parse_err(source);
    let rendered = err.render(source);
    let lines: Vec<&str> = rendered.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[1], source);
    let offset = byte_offset(source, '@');
    assert_eq!(lines[2], format!("{}^", " ".repeat(offset)));
}
