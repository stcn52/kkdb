use crate::error::{KkdbError, Result};
use crate::sql::ast::*;
use crate::sql::tokenizer::{Token, Tokenizer};
use crate::types::DataType;
use std::collections::VecDeque;

pub struct Parser<'a> {
    tokenizer: Tokenizer<'a>,
    buf: VecDeque<Token>, // small lookahead buffer (typically 1-3 tokens)
}

impl<'a> Parser<'a> {
    pub fn new(tokenizer: Tokenizer<'a>) -> Self {
        Parser {
            tokenizer,
            buf: VecDeque::with_capacity(4),
        }
    }

    pub fn parse(&mut self) -> Result<Statement> {
        let stmt = self.parse_statement()?;
        // optional semicolon
        if self.peek() == Some(&Token::Semicolon) {
            self.advance();
        }
        Ok(stmt)
    }

    /// Ensure buf has at least `n` tokens
    #[inline]
    fn ensure_buf(&mut self, n: usize) {
        while self.buf.len() < n {
            match self.tokenizer.next_token() {
                Ok(tok) => self.buf.push_back(tok),
                Err(_) => self.buf.push_back(Token::Eof),
            }
        }
    }

    #[inline]
    fn peek(&mut self) -> Option<&Token> {
        self.ensure_buf(1);
        let tok = self.buf.front()?;
        if matches!(tok, Token::Eof) {
            None
        } else {
            Some(tok)
        }
    }

    /// Lookahead N tokens ahead (0 = peek)
    #[inline]
    fn lookahead(&mut self, n: usize) -> Option<&Token> {
        self.ensure_buf(n + 1);
        self.buf.get(n)
    }

    #[inline]
    fn advance(&mut self) -> Token {
        self.ensure_buf(1);
        self.buf.pop_front().unwrap_or(Token::Eof)
    }

    #[inline]
    fn expect(&mut self, expected: &Token) -> Result<()> {
        let tok = self.advance();
        if std::mem::discriminant(&tok) == std::mem::discriminant(expected) {
            Ok(())
        } else {
            Err(KkdbError::ParseError(format!(
                "expected {:?}, got {:?}",
                expected, tok
            )))
        }
    }

    fn expect_identifier(&mut self) -> Result<String> {
        let tok = self.advance();
        match tok {
            Token::Identifier(s) => Ok(s),
            // Allow keywords as identifiers in some contexts
            _ => {
                if let Some(s) = self.token_as_ident(&tok) {
                    Ok(s)
                } else {
                    Err(KkdbError::ParseError(format!(
                        "expected identifier, got {:?}",
                        tok
                    )))
                }
            }
        }
    }

    fn token_as_ident(&self, tok: &Token) -> Option<String> {
        let s: &str = match tok {
            Token::Identifier(s) => return Some(s.clone()),
            // Type names
            Token::Text => "TEXT",
            Token::Integer => "INTEGER",
            Token::Real => "REAL",
            Token::Blob => "BLOB",
            // Constraint / DDL keywords
            Token::Key => "KEY",
            Token::Index => "INDEX",
            Token::Primary => "PRIMARY",
            Token::Autoincrement => "AUTOINCREMENT",
            Token::Unique => "UNIQUE",
            Token::Default => "DEFAULT",
            Token::Column => "COLUMN",
            // Aggregate / function names (common column names)
            Token::Count => "COUNT",
            Token::Sum => "SUM",
            Token::Avg => "AVG",
            Token::Min => "MIN",
            Token::Max => "MAX",
            // ALTER modifiers
            Token::Add => "ADD",
            Token::Rename => "RENAME",
            Token::To => "TO",
            // Other contextual keywords
            Token::Asc => "ASC",
            Token::Desc => "DESC",
            Token::Distinct => "DISTINCT",
            Token::Transaction => "TRANSACTION",
            Token::Outer => "OUTER",
            Token::Between => "BETWEEN",
            Token::Explain => "EXPLAIN",
            _ => return None,
        };
        Some(s.to_string())
    }

    fn check(&mut self, expected: &Token) -> bool {
        match self.peek() {
            Some(tok) => std::mem::discriminant(tok) == std::mem::discriminant(expected),
            None => false,
        }
    }

    fn match_token(&mut self, expected: &Token) -> bool {
        if self.check(expected) {
            self.advance();
            true
        } else {
            false
        }
    }

    // ---- Statement Parsing ----

    fn parse_statement(&mut self) -> Result<Statement> {
        match self.peek() {
            Some(Token::Explain) => {
                self.advance();
                let inner = self.parse_statement()?;
                Ok(Statement::Explain(Box::new(inner)))
            }
            Some(Token::Create) => {
                self.advance();
                match self.peek() {
                    Some(Token::Table) => self.parse_create_table(),
                    Some(Token::Index) | Some(Token::Unique) => self.parse_create_index(),
                    _ => Err(KkdbError::ParseError(
                        "expected TABLE or INDEX after CREATE".into(),
                    )),
                }
            }
            Some(Token::Drop) => {
                self.advance();
                self.expect(&Token::Table)?;
                self.parse_drop_table()
            }
            Some(Token::Insert) => {
                self.advance();
                self.parse_insert()
            }
            Some(Token::Select) => {
                self.advance();
                let stmt = self.parse_select()?;
                Ok(Statement::Select(stmt))
            }
            Some(Token::Update) => {
                self.advance();
                self.parse_update()
            }
            Some(Token::Delete) => {
                self.advance();
                self.parse_delete()
            }
            Some(Token::Begin) => {
                self.advance();
                let _ = self.match_token(&Token::Transaction);
                Ok(Statement::Begin)
            }
            Some(Token::Commit) => {
                self.advance();
                Ok(Statement::Commit)
            }
            Some(Token::Rollback) => {
                self.advance();
                Ok(Statement::Rollback)
            }
            Some(Token::Alter) => {
                self.advance();
                self.expect(&Token::Table)?;
                self.parse_alter_table()
            }
            Some(tok) => Err(KkdbError::ParseError(format!(
                "unexpected token: {:?}",
                tok
            ))),
            None => Err(KkdbError::ParseError("unexpected end of input".into())),
        }
    }

    fn parse_create_table(&mut self) -> Result<Statement> {
        self.advance(); // TABLE
        let if_not_exists = if self.match_token(&Token::If) {
            self.expect(&Token::Not)?;
            self.expect(&Token::Exists)?;
            true
        } else {
            false
        };

        let table_name = self.expect_identifier()?;
        self.expect(&Token::LeftParen)?;

        let mut columns = Vec::new();
        loop {
            let col = self.parse_column_def()?;
            columns.push(col);
            if !self.match_token(&Token::Comma) {
                break;
            }
            // Check for table constraints (PRIMARY KEY (col1, col2))
            if self.check(&Token::Primary) {
                // skip table-level primary key constraint for simplicity
                self.advance(); // PRIMARY
                self.expect(&Token::Key)?;
                self.expect(&Token::LeftParen)?;
                while !self.check(&Token::RightParen) {
                    self.advance();
                    let _ = self.match_token(&Token::Comma);
                }
                self.expect(&Token::RightParen)?;
                break;
            }
        }

        self.expect(&Token::RightParen)?;

        Ok(Statement::CreateTable(CreateTableStmt {
            table_name,
            columns,
            if_not_exists,
        }))
    }

    fn parse_alter_table(&mut self) -> Result<Statement> {
        let table_name = self.expect_identifier()?;

        let action = match self.peek() {
            Some(Token::Add) => {
                self.advance();
                // Optional COLUMN keyword
                let _ = self.match_token(&Token::Column);
                let col_def = self.parse_column_def()?;
                AlterTableAction::AddColumn(col_def)
            }
            Some(Token::Drop) => {
                self.advance();
                // Optional COLUMN keyword
                let _ = self.match_token(&Token::Column);
                let col_name = self.expect_identifier()?;
                AlterTableAction::DropColumn(col_name)
            }
            Some(Token::Rename) => {
                self.advance();
                if self.match_token(&Token::To) {
                    // RENAME TO new_name
                    let new_name = self.expect_identifier()?;
                    AlterTableAction::RenameTable(new_name)
                } else {
                    // RENAME [COLUMN] old_name TO new_name
                    let _ = self.match_token(&Token::Column);
                    let old_name = self.expect_identifier()?;
                    self.expect(&Token::To)?;
                    let new_name = self.expect_identifier()?;
                    AlterTableAction::RenameColumn { old_name, new_name }
                }
            }
            _ => {
                return Err(KkdbError::ParseError(
                    "expected ADD, DROP, or RENAME after ALTER TABLE <name>".into(),
                ))
            }
        };

        Ok(Statement::AlterTable(AlterTableStmt { table_name, action }))
    }

    fn parse_column_def(&mut self) -> Result<ColumnDef> {
        let name = self.expect_identifier()?;

        // optional type
        let data_type = if self.check(&Token::Integer)
            || self.check(&Token::Real)
            || self.check(&Token::Text)
            || self.check(&Token::Blob)
            || self.check(&Token::Identifier("".into()))
        {
            let type_tok = self.advance();
            match &type_tok {
                Token::Integer => DataType::Integer,
                Token::Real => DataType::Real,
                Token::Text => DataType::Text,
                Token::Blob => DataType::Blob,
                Token::Identifier(s) => DataType::from_str(s),
                _ => DataType::Blob,
            }
        } else {
            DataType::Blob // SQLite default
        };

        // optional type size like VARCHAR(255)
        if self.match_token(&Token::LeftParen) {
            // consume size params
            while !self.check(&Token::RightParen) {
                self.advance();
            }
            self.expect(&Token::RightParen)?;
        }

        let mut primary_key = false;
        let mut autoincrement = false;
        let mut not_null = false;
        let mut unique = false;
        let mut default = None;

        // column constraints
        loop {
            match self.peek() {
                Some(Token::Primary) => {
                    self.advance();
                    self.expect(&Token::Key)?;
                    primary_key = true;
                    if self.match_token(&Token::Autoincrement) {
                        autoincrement = true;
                    }
                }
                Some(Token::Not) => {
                    self.advance();
                    self.expect(&Token::Null)?;
                    not_null = true;
                }
                Some(Token::Unique) => {
                    self.advance();
                    unique = true;
                }
                Some(Token::Default) => {
                    self.advance();
                    default = Some(self.parse_expr()?);
                }
                _ => break,
            }
        }

        Ok(ColumnDef {
            name,
            data_type,
            primary_key,
            autoincrement,
            not_null,
            unique,
            default,
        })
    }

    fn parse_drop_table(&mut self) -> Result<Statement> {
        let if_exists = if self.match_token(&Token::If) {
            self.expect(&Token::Exists)?;
            true
        } else {
            false
        };
        let table_name = self.expect_identifier()?;
        Ok(Statement::DropTable(DropTableStmt {
            table_name,
            if_exists,
        }))
    }

    fn parse_insert(&mut self) -> Result<Statement> {
        self.expect(&Token::Into)?;
        let table_name = self.expect_identifier()?;

        // optional column list
        let columns = if self.match_token(&Token::LeftParen) {
            let mut cols = Vec::new();
            loop {
                cols.push(self.expect_identifier()?);
                if !self.match_token(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RightParen)?;
            Some(cols)
        } else {
            None
        };

        self.expect(&Token::Values)?;

        let mut all_values = Vec::new();
        loop {
            self.expect(&Token::LeftParen)?;
            let mut row_values = Vec::new();
            loop {
                row_values.push(self.parse_expr()?);
                if !self.match_token(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RightParen)?;
            all_values.push(row_values);
            if !self.match_token(&Token::Comma) {
                break;
            }
        }

        Ok(Statement::Insert(InsertStmt {
            table_name,
            columns,
            values: all_values,
        }))
    }

    fn parse_select(&mut self) -> Result<SelectStmt> {
        let distinct = self.match_token(&Token::Distinct);

        // columns
        let columns = self.parse_select_columns()?;

        // FROM
        let from = if self.match_token(&Token::From) {
            Some(self.parse_from_clause()?)
        } else {
            None
        };

        // WHERE
        let where_clause = if self.match_token(&Token::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // GROUP BY
        let group_by = if self.match_token(&Token::Group) {
            self.expect(&Token::By)?;
            let mut exprs = Vec::new();
            loop {
                exprs.push(self.parse_expr()?);
                if !self.match_token(&Token::Comma) {
                    break;
                }
            }
            exprs
        } else {
            Vec::new()
        };

        // HAVING
        let having = if self.match_token(&Token::Having) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // ORDER BY
        let order_by = if self.match_token(&Token::Order) {
            self.expect(&Token::By)?;
            let mut items = Vec::new();
            loop {
                let expr = self.parse_expr()?;
                let ascending = if self.match_token(&Token::Desc) {
                    false
                } else {
                    let _ = self.match_token(&Token::Asc);
                    true
                };
                items.push(OrderByItem { expr, ascending });
                if !self.match_token(&Token::Comma) {
                    break;
                }
            }
            items
        } else {
            Vec::new()
        };

        // LIMIT
        let limit = if self.match_token(&Token::Limit) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        // OFFSET
        let offset = if self.match_token(&Token::Offset) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(SelectStmt {
            distinct,
            columns,
            from,
            where_clause,
            group_by,
            having,
            order_by,
            limit,
            offset,
        })
    }

    fn parse_select_columns(&mut self) -> Result<Vec<SelectColumn>> {
        let mut columns = Vec::new();
        loop {
            if self.match_token(&Token::Star) {
                columns.push(SelectColumn::AllColumns);
            } else {
                // Check for table.* pattern: Identifier Dot Star
                if matches!(self.peek(), Some(Token::Identifier(_))) {
                    if self.lookahead(1) == Some(&Token::Dot)
                        && self.lookahead(2) == Some(&Token::Star)
                    {
                        let name = self.expect_identifier()?;
                        self.advance(); // Dot
                        self.advance(); // Star
                        columns.push(SelectColumn::TableAllColumns(name));
                        if !self.match_token(&Token::Comma) {
                            break;
                        }
                        continue;
                    }
                }

                let expr = self.parse_expr()?;
                let alias = if self.match_token(&Token::As) {
                    Some(self.expect_identifier()?)
                } else if self.check(&Token::Identifier("".into())) && !self.check(&Token::From) {
                    // implicit alias
                    Some(self.expect_identifier()?)
                } else {
                    None
                };
                columns.push(SelectColumn::Expr { expr, alias });
            }
            if !self.match_token(&Token::Comma) {
                break;
            }
        }
        Ok(columns)
    }

    fn parse_from_clause(&mut self) -> Result<FromClause> {
        let mut from = self.parse_from_item()?;

        // joins
        loop {
            let join_type = match self.peek() {
                Some(Token::Inner) => {
                    self.advance();
                    self.expect(&Token::Join)?;
                    JoinType::Inner
                }
                Some(Token::Left) => {
                    self.advance();
                    let _ = self.match_token(&Token::Outer);
                    self.expect(&Token::Join)?;
                    JoinType::Left
                }
                Some(Token::Right) => {
                    self.advance();
                    let _ = self.match_token(&Token::Outer);
                    self.expect(&Token::Join)?;
                    JoinType::Right
                }
                Some(Token::Join) => {
                    self.advance();
                    JoinType::Inner
                }
                Some(Token::Comma) => {
                    self.advance();
                    JoinType::Cross
                }
                _ => break,
            };

            let right = self.parse_from_item()?;
            let on = if self.match_token(&Token::On) {
                Some(self.parse_expr()?)
            } else {
                None
            };

            from = FromClause::Join {
                left: Box::new(from),
                join_type,
                right: Box::new(right),
                on,
            };
        }

        Ok(from)
    }

    fn parse_from_item(&mut self) -> Result<FromClause> {
        if self.match_token(&Token::LeftParen) {
            // subquery
            if self.check(&Token::Select) {
                self.advance();
                let query = self.parse_select()?;
                self.expect(&Token::RightParen)?;
                let _ = self.match_token(&Token::As);
                let alias = self.expect_identifier()?;
                Ok(FromClause::Subquery {
                    query: Box::new(query),
                    alias,
                })
            } else {
                let inner = self.parse_from_clause()?;
                self.expect(&Token::RightParen)?;
                Ok(inner)
            }
        } else {
            let name = self.expect_identifier()?;
            let alias = if self.match_token(&Token::As) {
                Some(self.expect_identifier()?)
            } else if self.check(&Token::Identifier("".into()))
                && !self.check(&Token::Where)
                && !self.check(&Token::Order)
                && !self.check(&Token::Group)
                && !self.check(&Token::On)
                && !self.check(&Token::Inner)
                && !self.check(&Token::Left)
                && !self.check(&Token::Right)
                && !self.check(&Token::Join)
            {
                Some(self.expect_identifier()?)
            } else {
                None
            };
            Ok(FromClause::Table { name, alias })
        }
    }

    fn parse_update(&mut self) -> Result<Statement> {
        let table_name = self.expect_identifier()?;
        self.expect(&Token::Set)?;

        let mut assignments = Vec::new();
        loop {
            let col = self.expect_identifier()?;
            self.expect(&Token::Equal)?;
            let val = self.parse_expr()?;
            assignments.push((col, val));
            if !self.match_token(&Token::Comma) {
                break;
            }
        }

        let where_clause = if self.match_token(&Token::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(Statement::Update(UpdateStmt {
            table_name,
            assignments,
            where_clause,
        }))
    }

    fn parse_delete(&mut self) -> Result<Statement> {
        self.expect(&Token::From)?;
        let table_name = self.expect_identifier()?;

        let where_clause = if self.match_token(&Token::Where) {
            Some(self.parse_expr()?)
        } else {
            None
        };

        Ok(Statement::Delete(DeleteStmt {
            table_name,
            where_clause,
        }))
    }

    fn parse_create_index(&mut self) -> Result<Statement> {
        let unique = if self.check(&Token::Unique) {
            self.advance();
            true
        } else {
            false
        };
        self.expect(&Token::Index)?;

        let if_not_exists = if self.match_token(&Token::If) {
            self.expect(&Token::Not)?;
            self.expect(&Token::Exists)?;
            true
        } else {
            false
        };

        let index_name = self.expect_identifier()?;
        self.expect(&Token::On)?;
        let table_name = self.expect_identifier()?;
        self.expect(&Token::LeftParen)?;

        let mut columns = Vec::new();
        loop {
            columns.push(self.expect_identifier()?);
            if !self.match_token(&Token::Comma) {
                break;
            }
        }
        self.expect(&Token::RightParen)?;

        Ok(Statement::CreateIndex(CreateIndexStmt {
            index_name,
            table_name,
            columns,
            unique,
            if_not_exists,
        }))
    }

    // ---- Expression Parsing (Pratt parser) ----

    pub fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_or_expr()
    }

    fn parse_or_expr(&mut self) -> Result<Expr> {
        let mut left = self.parse_and_expr()?;
        while self.match_token(&Token::Or) {
            let right = self.parse_and_expr()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::Or,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and_expr(&mut self) -> Result<Expr> {
        let mut left = self.parse_not_expr()?;
        while self.match_token(&Token::And) {
            let right = self.parse_not_expr()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op: BinaryOperator::And,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_not_expr(&mut self) -> Result<Expr> {
        if self.match_token(&Token::Not) {
            let expr = self.parse_not_expr()?;
            Ok(Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr: Box::new(expr),
            })
        } else {
            self.parse_comparison()
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr> {
        let mut left = self.parse_addition()?;

        loop {
            // IS NULL / IS NOT NULL
            if self.match_token(&Token::Is) {
                if self.match_token(&Token::Not) {
                    self.expect(&Token::Null)?;
                    left = Expr::IsNull {
                        expr: Box::new(left),
                        negated: true,
                    };
                } else {
                    self.expect(&Token::Null)?;
                    left = Expr::IsNull {
                        expr: Box::new(left),
                        negated: false,
                    };
                }
                continue;
            }

            // NOT IN / NOT LIKE / NOT BETWEEN (use lookahead to avoid consuming NOT)
            if self.check(&Token::Not) {
                if self.lookahead(1) == Some(&Token::In) {
                    self.advance(); // NOT
                    self.advance(); // IN
                    left = self.parse_in_list(left, true)?;
                    continue;
                } else if self.lookahead(1) == Some(&Token::Like) {
                    self.advance(); // NOT
                    self.advance(); // LIKE
                    let pattern = self.parse_addition()?;
                    left = Expr::Like {
                        expr: Box::new(left),
                        pattern: Box::new(pattern),
                        negated: true,
                    };
                    continue;
                } else if self.lookahead(1) == Some(&Token::Between) {
                    self.advance(); // NOT
                    self.advance(); // BETWEEN
                    let low = self.parse_addition()?;
                    self.expect(&Token::And)?;
                    let high = self.parse_addition()?;
                    left = Expr::Between {
                        expr: Box::new(left),
                        low: Box::new(low),
                        high: Box::new(high),
                        negated: true,
                    };
                    continue;
                }
            }

            // IN
            if self.match_token(&Token::In) {
                left = self.parse_in_list(left, false)?;
                continue;
            }

            // LIKE
            if self.match_token(&Token::Like) {
                let pattern = self.parse_addition()?;
                left = Expr::Like {
                    expr: Box::new(left),
                    pattern: Box::new(pattern),
                    negated: false,
                };
                continue;
            }

            // BETWEEN
            if self.match_token(&Token::Between) {
                let low = self.parse_addition()?;
                self.expect(&Token::And)?;
                let high = self.parse_addition()?;
                left = Expr::Between {
                    expr: Box::new(left),
                    low: Box::new(low),
                    high: Box::new(high),
                    negated: false,
                };
                continue;
            }

            let op = match self.peek() {
                Some(Token::Equal) => BinaryOperator::Equal,
                Some(Token::NotEqual) => BinaryOperator::NotEqual,
                Some(Token::Less) => BinaryOperator::LessThan,
                Some(Token::LessEqual) => BinaryOperator::LessThanOrEqual,
                Some(Token::Greater) => BinaryOperator::GreaterThan,
                Some(Token::GreaterEqual) => BinaryOperator::GreaterThanOrEqual,
                _ => break,
            };
            self.advance();
            let right = self.parse_addition()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }

        Ok(left)
    }

    fn parse_addition(&mut self) -> Result<Expr> {
        let mut left = self.parse_multiplication()?;
        loop {
            let op = match self.peek() {
                Some(Token::Plus) => BinaryOperator::Add,
                Some(Token::Minus) => BinaryOperator::Subtract,
                Some(Token::Concat) => BinaryOperator::Concat,
                _ => break,
            };
            self.advance();
            let right = self.parse_multiplication()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_multiplication(&mut self) -> Result<Expr> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Star) => BinaryOperator::Multiply,
                Some(Token::Slash) => BinaryOperator::Divide,
                Some(Token::Percent) => BinaryOperator::Modulo,
                _ => break,
            };
            self.advance();
            let right = self.parse_unary()?;
            left = Expr::BinaryOp {
                left: Box::new(left),
                op,
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Expr> {
        if self.match_token(&Token::Minus) {
            let expr = self.parse_primary()?;
            Ok(Expr::UnaryOp {
                op: UnaryOperator::Minus,
                expr: Box::new(expr),
            })
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Expr> {
        let tok = self.advance();
        match tok {
            Token::IntegerLiteral(v) => Ok(Expr::IntegerLiteral(v)),
            Token::RealLiteral(v) => Ok(Expr::RealLiteral(v)),
            Token::StringLiteral(v) => Ok(Expr::StringLiteral(v)),
            Token::BlobLiteral(v) => Ok(Expr::BlobLiteral(v)),
            Token::Null => Ok(Expr::Null),

            // Function calls: COUNT, SUM, etc.
            Token::Count | Token::Sum | Token::Avg | Token::Min | Token::Max => {
                let name = match tok {
                    Token::Count => "COUNT",
                    Token::Sum => "SUM",
                    Token::Avg => "AVG",
                    Token::Min => "MIN",
                    Token::Max => "MAX",
                    _ => unreachable!(),
                }
                .to_string();
                self.expect(&Token::LeftParen)?;
                let distinct = self.match_token(&Token::Distinct);
                let mut args = Vec::new();
                if self.match_token(&Token::Star) {
                    // COUNT(*)
                    args.push(Expr::IntegerLiteral(1));
                } else if !self.check(&Token::RightParen) {
                    loop {
                        args.push(self.parse_expr()?);
                        if !self.match_token(&Token::Comma) {
                            break;
                        }
                    }
                }
                self.expect(&Token::RightParen)?;
                Ok(Expr::Function {
                    name,
                    args,
                    distinct,
                })
            }

            Token::Identifier(name) => {
                // function call?
                if self.match_token(&Token::LeftParen) {
                    let mut args = Vec::new();
                    if !self.check(&Token::RightParen) {
                        loop {
                            args.push(self.parse_expr()?);
                            if !self.match_token(&Token::Comma) {
                                break;
                            }
                        }
                    }
                    self.expect(&Token::RightParen)?;
                    Ok(Expr::Function {
                        name,
                        args,
                        distinct: false,
                    })
                }
                // table.column
                else if self.match_token(&Token::Dot) {
                    let col = self.expect_identifier()?;
                    Ok(Expr::ColumnRef {
                        table: Some(name),
                        column: col,
                    })
                } else {
                    Ok(Expr::ColumnRef {
                        table: None,
                        column: name,
                    })
                }
            }

            Token::LeftParen => {
                // Check for subquery expression: (SELECT ...)
                if self.check(&Token::Select) {
                    self.advance();
                    let subquery = self.parse_select()?;
                    self.expect(&Token::RightParen)?;
                    Ok(Expr::Subquery(Box::new(subquery)))
                } else {
                    let expr = self.parse_expr()?;
                    self.expect(&Token::RightParen)?;
                    Ok(Expr::Nested(Box::new(expr)))
                }
            }

            Token::Exists => {
                self.expect(&Token::LeftParen)?;
                if self.check(&Token::Select) {
                    self.advance();
                    let subquery = self.parse_select()?;
                    self.expect(&Token::RightParen)?;
                    Ok(Expr::Exists(Box::new(subquery)))
                } else {
                    Err(KkdbError::ParseError(
                        "expected SELECT after EXISTS (".into(),
                    ))
                }
            }

            _ => Err(KkdbError::ParseError(format!(
                "unexpected token in expression: {:?}",
                tok
            ))),
        }
    }

    /// Parse IN list or IN subquery after the opening `IN` keyword has been consumed.
    fn parse_in_list(&mut self, left: Expr, negated: bool) -> Result<Expr> {
        self.expect(&Token::LeftParen)?;
        if self.check(&Token::Select) {
            self.advance();
            let subquery = self.parse_select()?;
            self.expect(&Token::RightParen)?;
            Ok(Expr::InSubquery {
                expr: Box::new(left),
                subquery: Box::new(subquery),
                negated,
            })
        } else {
            let mut list = Vec::new();
            loop {
                list.push(self.parse_expr()?);
                if !self.match_token(&Token::Comma) {
                    break;
                }
            }
            self.expect(&Token::RightParen)?;
            Ok(Expr::InList {
                expr: Box::new(left),
                list,
                negated,
            })
        }
    }
}

/// Convenience function: parse SQL string to Statement
#[inline]
pub fn parse_sql(sql: &str) -> Result<Statement> {
    let tokenizer = Tokenizer::new(sql);
    let mut parser = Parser::new(tokenizer);
    parser.parse()
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
