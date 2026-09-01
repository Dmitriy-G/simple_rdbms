use sql::{
    BinaryOperator, ColumnDef, CreateIndexStatement, CreateTableStatement, Expr, InsertStatement,
    Lexer, Parser, SelectItem, SelectStatement, SqlError, Statement, TableRef, UnaryOperator,
};
use types::{DataType, Value};

fn col_expr(name: &str) -> Expr {
    Expr::Column { table: None, name: name.to_string() }
}

fn table_ref(name: &str) -> TableRef {
    TableRef { name: name.to_string(), alias: None }
}

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
fn parses_create_index() {
    let stmt = parse("CREATE INDEX idx_users_id ON users (id)");
    assert_eq!(
        stmt,
        Statement::CreateIndex(CreateIndexStatement {
            index_name: "idx_users_id".to_string(),
            table: "users".to_string(),
            column: "id".to_string(),
        })
    );
}

#[test]
fn create_index_and_create_table_are_disambiguated_by_one_token_of_lookahead() {
    let index_stmt = parse("create index idx on t (a)");
    assert_eq!(
        index_stmt,
        Statement::CreateIndex(CreateIndexStatement {
            index_name: "idx".to_string(),
            table: "t".to_string(),
            column: "a".to_string(),
        })
    );

    let table_stmt = parse("create table t (a int)");
    assert!(matches!(table_stmt, Statement::CreateTable(_)));
}

#[test]
fn create_index_missing_on_is_a_parse_error() {
    let err = parse_err("CREATE INDEX idx users (id)");
    assert!(matches!(err, SqlError::UnexpectedToken { .. }));
}

#[test]
fn create_index_missing_parens_around_column_is_a_parse_error() {
    let err = parse_err("CREATE INDEX idx ON users id");
    assert!(matches!(err, SqlError::UnexpectedToken { .. }));
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
            from: table_ref("t"),
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
            items: vec![SelectItem::Expr(col_expr("a")), SelectItem::Expr(col_expr("b")),],
            from: table_ref("t"),
            where_clause: Some(Expr::BinaryOp {
                left: Box::new(col_expr("a")),
                op: BinaryOperator::Eq,
                right: Box::new(Expr::Literal(Value::BigInt(1))),
            }),
        })
    );
}

#[test]
fn parses_qualified_column_reference() {
    let stmt = parse("SELECT t.a FROM t WHERE t.b = 1");
    assert_eq!(
        stmt,
        Statement::Select(SelectStatement {
            items: vec![SelectItem::Expr(Expr::Column {
                table: Some("t".to_string()),
                name: "a".to_string(),
            })],
            from: table_ref("t"),
            where_clause: Some(Expr::BinaryOp {
                left: Box::new(Expr::Column {
                    table: Some("t".to_string()),
                    name: "b".to_string(),
                }),
                op: BinaryOperator::Eq,
                right: Box::new(Expr::Literal(Value::BigInt(1))),
            }),
        })
    );
}

#[test]
fn parses_table_alias_with_and_without_as() {
    let stmt = parse("SELECT * FROM t AS u");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(select.from, TableRef { name: "t".to_string(), alias: Some("u".to_string()) });

    let stmt = parse("SELECT * FROM t u");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(select.from, TableRef { name: "t".to_string(), alias: Some("u".to_string()) });
}

#[test]
fn from_without_an_alias_still_allows_a_where_clause() {
    let stmt = parse("SELECT * FROM t WHERE a = 1");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(select.from, table_ref("t"));
    assert!(select.where_clause.is_some());
}

#[test]
fn order_by_is_not_swallowed_as_a_table_alias() {
    let err = parse_err("SELECT * FROM t ORDER BY x");
    match err {
        SqlError::UnexpectedToken { found, .. } => {
            assert!(found.contains("ORDER"), "expected the error to name ORDER, got {found:?}");
        }
        other => panic!("expected UnexpectedToken naming ORDER, got {other:?}"),
    }
}

#[test]
fn table_alias_declines_reserved_words_not_yet_tokenized() {
    for word in ["ORDER", "GROUP", "HAVING", "LIMIT", "JOIN", "RETURNING", "UNION", "OFFSET"] {
        let source = format!("SELECT * FROM t {word}");
        match parse_err(&source) {
            SqlError::UnexpectedToken { found, .. } => {
                assert!(found.contains(word), "expected {found:?} to mention {word:?}");
            }
            other => panic!("expected UnexpectedToken for {source:?}, got {other:?}"),
        }
    }
}

#[test]
fn table_alias_declines_reserved_words_case_insensitively() {
    match parse_err("SELECT * FROM t order BY x") {
        SqlError::UnexpectedToken { found, .. } => {
            assert!(found.contains("order"), "expected the error to name order, got {found:?}");
        }
        other => panic!("expected UnexpectedToken, got {other:?}"),
    }
}

#[test]
fn precedence_or_binds_looser_than_and() {
    let stmt = parse("SELECT * FROM t WHERE a = 1 OR b = 2 AND c = 3");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };

    let eq = |col: &str, v: i64| Expr::BinaryOp {
        left: Box::new(col_expr(col)),
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
    let stmt = parse("SELECT * FROM t WHERE (a = 1 OR b = 2) AND c = 3");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };

    let eq = |col: &str, v: i64| Expr::BinaryOp {
        left: Box::new(col_expr(col)),
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
        left: Box::new(Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(col_expr("a")) }),
        op: BinaryOperator::And,
        right: Box::new(Expr::BinaryOp {
            left: Box::new(col_expr("b")),
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
fn parses_is_null_and_is_not_null() {
    let stmt = parse("SELECT * FROM t WHERE a IS NULL");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(
        select.where_clause,
        Some(Expr::IsNull { expr: Box::new(col_expr("a")), negated: false })
    );

    let stmt = parse("SELECT * FROM t WHERE a IS NOT NULL");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(
        select.where_clause,
        Some(Expr::IsNull { expr: Box::new(col_expr("a")), negated: true })
    );
}

#[test]
fn is_null_binds_tighter_than_and_but_looser_than_comparison() {
    let stmt = parse("SELECT * FROM t WHERE a = 1 AND b IS NULL");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    let expected = Expr::BinaryOp {
        left: Box::new(Expr::BinaryOp {
            left: Box::new(col_expr("a")),
            op: BinaryOperator::Eq,
            right: Box::new(Expr::Literal(Value::BigInt(1))),
        }),
        op: BinaryOperator::And,
        right: Box::new(Expr::IsNull { expr: Box::new(col_expr("b")), negated: false }),
    };
    assert_eq!(select.where_clause, Some(expected));
}

#[test]
fn is_null_missing_null_keyword_is_a_parse_error() {
    let err = parse_err("SELECT * FROM t WHERE a IS");
    assert!(matches!(err, SqlError::UnexpectedEof { .. }));
}

#[test]
fn string_literal_escapes_doubled_quote() {
    let stmt = parse("SELECT * FROM t WHERE a = 'it''s a test'");
    let Statement::Select(select) = stmt else { panic!("expected a SELECT") };
    assert_eq!(
        select.where_clause,
        Some(Expr::BinaryOp {
            left: Box::new(col_expr("a")),
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
            items: vec![SelectItem::Expr(col_expr("Id"))],
            from: table_ref("MyTable"),
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
            left: Box::new(col_expr("a")),
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
fn parses_explain_select() {
    let stmt = parse("EXPLAIN SELECT * FROM t");
    assert_eq!(
        stmt,
        Statement::Explain {
            verbose: false,
            inner: Box::new(Statement::Select(SelectStatement {
                items: vec![SelectItem::Wildcard],
                from: table_ref("t"),
                where_clause: None,
            })),
        }
    );
}

#[test]
fn parses_explain_verbose() {
    let stmt = parse("EXPLAIN VERBOSE SELECT * FROM t");
    let Statement::Explain { verbose, inner } = stmt else { panic!("expected an EXPLAIN") };
    assert!(verbose);
    assert!(matches!(*inner, Statement::Select(_)));
}

#[test]
fn explain_verbose_is_case_insensitive() {
    let stmt = parse("explain verbose select * from t");
    let Statement::Explain { verbose, .. } = stmt else { panic!("expected an EXPLAIN") };
    assert!(verbose);
}

#[test]
fn explain_of_insert_and_create_statements_parses() {
    assert!(matches!(
        parse("EXPLAIN INSERT INTO t VALUES (1)"),
        Statement::Explain { verbose: false, inner } if matches!(*inner, Statement::Insert(_))
    ));
    assert!(matches!(
        parse("EXPLAIN CREATE TABLE t (a INTEGER)"),
        Statement::Explain { verbose: false, inner } if matches!(*inner, Statement::CreateTable(_))
    ));
    assert!(matches!(
        parse("EXPLAIN CREATE INDEX idx ON t (a)"),
        Statement::Explain { verbose: false, inner } if matches!(*inner, Statement::CreateIndex(_))
    ));
}

#[test]
fn select_verbose_still_parses_verbose_as_a_column_reference() {
    let stmt = parse("SELECT verbose FROM t");
    assert_eq!(
        stmt,
        Statement::Select(SelectStatement {
            items: vec![SelectItem::Expr(col_expr("verbose"))],
            from: table_ref("t"),
            where_clause: None,
        })
    );
}

#[test]
fn explain_of_begin_commit_rollback_is_a_parse_error() {
    for target in ["BEGIN", "COMMIT", "ROLLBACK"] {
        let err = parse_err(&format!("EXPLAIN {target}"));
        assert!(
            matches!(err, SqlError::ExplainOfTransactionControl { .. }),
            "expected ExplainOfTransactionControl for EXPLAIN {target}, got {err:?}"
        );
    }
}

#[test]
fn nested_explain_is_a_parse_error() {
    let err = parse_err("EXPLAIN EXPLAIN SELECT * FROM t");
    assert!(matches!(err, SqlError::NestedExplain { .. }), "got {err:?}");
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
