use crate::error::{KkdbError, Result};
use crate::sql::ast as kk;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser as SqlParser;

mod common;
mod expr;
mod query;
mod statement;

pub fn parse_sql_with_sqlparser(sql: &str) -> Result<kk::Statement> {
    if sql.trim().is_empty() {
        return Err(KkdbError::ParseError("unexpected end of input".into()));
    }

    let dialect = SQLiteDialect {};
    let mut statements =
        SqlParser::parse_sql(&dialect, sql).map_err(|e| KkdbError::ParseError(e.to_string()))?;

    if statements.is_empty() {
        return Err(KkdbError::ParseError("unexpected end of input".into()));
    }
    if statements.len() != 1 {
        return Err(KkdbError::ParseError(
            "only a single SQL statement is supported".into(),
        ));
    }

    statement::convert_statement(statements.remove(0))
}
