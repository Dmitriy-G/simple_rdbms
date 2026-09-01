use types::{DataType, Value};

use crate::ast::{
    BinaryOperator, ColumnDef, CreateIndexStatement, CreateTableStatement, Expr, InsertStatement,
    SelectItem, SelectStatement, Statement, UnaryOperator,
};
use crate::error::SqlError;
use crate::token::{Token, TokenKind};

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    pub fn parse(&mut self) -> Result<Statement, SqlError> {
        let statement = self.parse_statement()?;

        if matches!(self.current().kind, TokenKind::Semicolon) {
            self.advance();
        }
        match self.current().kind {
            TokenKind::Eof => Ok(statement),
            _ => Err(self.unexpected("end of statement")),
        }
    }

    fn parse_statement(&mut self) -> Result<Statement, SqlError> {
        match &self.current().kind {
            TokenKind::Select => Ok(Statement::Select(self.parse_select()?)),
            TokenKind::Insert => Ok(Statement::Insert(self.parse_insert()?)),
            TokenKind::Create => match self.peek_kind(1) {
                TokenKind::Index => Ok(Statement::CreateIndex(self.parse_create_index()?)),
                _ => Ok(Statement::CreateTable(self.parse_create_table()?)),
            },
            TokenKind::Begin => {
                self.advance();
                Ok(Statement::Begin)
            }
            TokenKind::Start => {
                self.advance();
                self.expect_kind(TokenKind::Transaction, "TRANSACTION")?;
                Ok(Statement::Begin)
            }
            TokenKind::Commit => {
                self.advance();
                Ok(Statement::Commit)
            }
            TokenKind::Rollback => {
                self.advance();
                Ok(Statement::Rollback)
            }
            TokenKind::Explain => self.parse_explain(),
            _ => Err(self
                .unexpected("SELECT, INSERT, CREATE, BEGIN, START, COMMIT, ROLLBACK, or EXPLAIN")),
        }
    }

    fn parse_explain(&mut self) -> Result<Statement, SqlError> {
        let explain_offset = self.current().offset;
        self.expect_kind(TokenKind::Explain, "EXPLAIN")?;
        let verbose = self.consume_verbose_keyword();
        let inner = self.parse_statement()?;
        match &inner {
            Statement::Begin | Statement::Commit | Statement::Rollback => {
                Err(SqlError::ExplainOfTransactionControl { offset: explain_offset })
            }
            Statement::Explain { .. } => Err(SqlError::NestedExplain { offset: explain_offset }),
            _ => Ok(Statement::Explain { verbose, inner: Box::new(inner) }),
        }
    }

    fn consume_verbose_keyword(&mut self) -> bool {
        match &self.current().kind {
            TokenKind::Identifier(name) if name.eq_ignore_ascii_case("VERBOSE") => {
                self.advance();
                true
            }
            _ => false,
        }
    }

    fn parse_select(&mut self) -> Result<SelectStatement, SqlError> {
        self.expect_kind(TokenKind::Select, "SELECT")?;
        let items = self.parse_select_list()?;
        self.expect_kind(TokenKind::From, "FROM")?;
        let from = self.expect_identifier()?;
        let where_clause = if matches!(self.current().kind, TokenKind::Where) {
            self.advance();
            Some(self.parse_expr()?)
        } else {
            None
        };
        Ok(SelectStatement { items, from, where_clause })
    }

    fn parse_select_list(&mut self) -> Result<Vec<SelectItem>, SqlError> {
        if matches!(self.current().kind, TokenKind::Star) {
            self.advance();
            return Ok(vec![SelectItem::Wildcard]);
        }
        let mut items = vec![SelectItem::Expr(self.parse_expr()?)];
        while matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            items.push(SelectItem::Expr(self.parse_expr()?));
        }
        Ok(items)
    }

    fn parse_insert(&mut self) -> Result<InsertStatement, SqlError> {
        self.expect_kind(TokenKind::Insert, "INSERT")?;
        self.expect_kind(TokenKind::Into, "INTO")?;
        let table = self.expect_identifier()?;

        let mut columns = Vec::new();
        if matches!(self.current().kind, TokenKind::LParen) {
            self.advance();
            columns.push(self.expect_identifier()?);
            while matches!(self.current().kind, TokenKind::Comma) {
                self.advance();
                columns.push(self.expect_identifier()?);
            }
            self.expect_kind(TokenKind::RParen, ")")?;
        }

        self.expect_kind(TokenKind::Values, "VALUES")?;
        let mut values = vec![self.parse_value_row()?];
        while matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            values.push(self.parse_value_row()?);
        }

        Ok(InsertStatement { table, columns, values })
    }

    fn parse_value_row(&mut self) -> Result<Vec<Expr>, SqlError> {
        self.expect_kind(TokenKind::LParen, "(")?;
        let mut exprs = vec![self.parse_expr()?];
        while matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            exprs.push(self.parse_expr()?);
        }
        self.expect_kind(TokenKind::RParen, ")")?;
        Ok(exprs)
    }

    fn parse_create_table(&mut self) -> Result<CreateTableStatement, SqlError> {
        self.expect_kind(TokenKind::Create, "CREATE")?;
        self.expect_kind(TokenKind::Table, "TABLE")?;
        let table = self.expect_identifier()?;

        self.expect_kind(TokenKind::LParen, "(")?;
        let mut columns = vec![self.parse_col_def()?];
        while matches!(self.current().kind, TokenKind::Comma) {
            self.advance();
            columns.push(self.parse_col_def()?);
        }
        self.expect_kind(TokenKind::RParen, ")")?;

        Ok(CreateTableStatement { table, columns })
    }

    fn parse_create_index(&mut self) -> Result<CreateIndexStatement, SqlError> {
        self.expect_kind(TokenKind::Create, "CREATE")?;
        self.expect_kind(TokenKind::Index, "INDEX")?;
        let index_name = self.expect_identifier()?;
        self.expect_kind(TokenKind::On, "ON")?;
        let table = self.expect_identifier()?;

        self.expect_kind(TokenKind::LParen, "(")?;
        let column = self.expect_identifier()?;
        self.expect_kind(TokenKind::RParen, ")")?;

        Ok(CreateIndexStatement { index_name, table, column })
    }

    fn parse_col_def(&mut self) -> Result<ColumnDef, SqlError> {
        let name = self.expect_identifier()?;
        let data_type = self.parse_type_name()?;
        Ok(ColumnDef { name, data_type, nullable: true })
    }

    fn parse_type_name(&mut self) -> Result<DataType, SqlError> {
        const EXPECTED: &str = "INTEGER, INT, BIGINT, DOUBLE, TEXT, or BOOLEAN";
        let token = self.current().clone();
        let name = match &token.kind {
            TokenKind::Identifier(name) => name.clone(),
            _ => return Err(self.unexpected(EXPECTED)),
        };
        let data_type = match name.to_ascii_uppercase().as_str() {
            "INTEGER" | "INT" => DataType::Integer,
            "BIGINT" => DataType::BigInt,
            "DOUBLE" => DataType::Double,
            "TEXT" => DataType::Varchar(u32::MAX),
            "BOOLEAN" => DataType::Boolean,
            _ => return Err(self.unexpected(EXPECTED)),
        };
        self.advance();
        Ok(data_type)
    }

    fn parse_expr(&mut self) -> Result<Expr, SqlError> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.parse_and()?;
        while matches!(self.current().kind, TokenKind::Or) {
            self.advance();
            let right = self.parse_and()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.parse_comparison()?;
        while matches!(self.current().kind, TokenKind::And) {
            self.advance();
            let right = self.parse_comparison()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_comparison(&mut self) -> Result<Expr, SqlError> {
        let mut left = self.parse_is_null_operand()?;
        loop {
            let op = match &self.current().kind {
                TokenKind::Eq => BinaryOperator::Eq,
                TokenKind::NotEq => BinaryOperator::NotEq,
                TokenKind::Lt => BinaryOperator::Lt,
                TokenKind::LtEq => BinaryOperator::LtEq,
                TokenKind::Gt => BinaryOperator::Gt,
                TokenKind::GtEq => BinaryOperator::GtEq,
                _ => break,
            };
            self.advance();
            let right = self.parse_is_null_operand()?;
            left = Expr::BinaryOp { left: Box::new(left), op, right: Box::new(right) };
        }
        Ok(left)
    }

    fn parse_is_null_operand(&mut self) -> Result<Expr, SqlError> {
        let mut expr = self.parse_unary()?;
        while matches!(self.current().kind, TokenKind::Is) {
            self.advance();
            let negated = if matches!(self.current().kind, TokenKind::Not) {
                self.advance();
                true
            } else {
                false
            };
            self.expect_kind(TokenKind::Null, "NULL")?;
            expr = Expr::IsNull { expr: Box::new(expr), negated };
        }
        Ok(expr)
    }

    fn parse_unary(&mut self) -> Result<Expr, SqlError> {
        match self.current().kind {
            TokenKind::Not => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp { op: UnaryOperator::Not, expr: Box::new(expr) })
            }
            TokenKind::Minus => {
                self.advance();
                let expr = self.parse_unary()?;
                Ok(Expr::UnaryOp { op: UnaryOperator::Negate, expr: Box::new(expr) })
            }
            _ => self.parse_primary(),
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, SqlError> {
        let token = self.current().clone();
        match token.kind {
            TokenKind::IntegerLiteral(v) => {
                self.advance();
                Ok(Expr::Literal(Value::BigInt(v)))
            }
            TokenKind::FloatLiteral(v) => {
                self.advance();
                Ok(Expr::Literal(Value::Double(v)))
            }
            TokenKind::StringLiteral(s) => {
                self.advance();
                Ok(Expr::Literal(Value::Varchar(s)))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal(Value::Boolean(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal(Value::Boolean(false)))
            }
            TokenKind::Null => {
                self.advance();
                Ok(Expr::Literal(Value::Null))
            }
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(Expr::Column(name))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expr()?;
                self.expect_kind(TokenKind::RParen, ")")?;
                Ok(expr)
            }
            _ => Err(self.unexpected("an expression")),
        }
    }

    fn current(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn peek_kind(&self, ahead: usize) -> &TokenKind {
        let idx = (self.pos + ahead).min(self.tokens.len() - 1);
        &self.tokens[idx].kind
    }

    fn advance(&mut self) -> Token {
        let token = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        token
    }

    fn expect_kind(&mut self, kind: TokenKind, expected: &str) -> Result<Token, SqlError> {
        if self.current().kind == kind {
            Ok(self.advance())
        } else {
            Err(self.unexpected(expected))
        }
    }

    fn expect_identifier(&mut self) -> Result<String, SqlError> {
        match self.current().kind.clone() {
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(name)
            }
            _ => Err(self.unexpected("an identifier")),
        }
    }

    fn unexpected(&self, expected: &str) -> SqlError {
        let token = self.current();
        if token.kind == TokenKind::Eof {
            SqlError::UnexpectedEof { expected: expected.to_string() }
        } else {
            SqlError::UnexpectedToken {
                expected: expected.to_string(),
                found: format!("{:?}", token.kind),
                offset: token.offset,
            }
        }
    }
}
