use crate::error::Result;
use crate::sql::ast::Statement;

/// Convenience function: parse SQL string to Statement.
#[inline]
pub fn parse_sql(sql: &str) -> Result<Statement> {
    crate::sql::sqlparser_adapter::parse_sql_with_sqlparser(sql)
}

#[cfg(test)]
#[path = "parser_tests.rs"]
mod tests;
