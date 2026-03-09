use crate::error::{KkdbError, Result};
use crate::sql::ast as kk;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser as SqlParser;

mod common;
mod expr;
mod query;
mod statement;

pub fn parse_sql_with_sqlparser(sql: &str) -> Result<kk::Statement> {
    let trimmed = sql.trim();
    if trimmed.is_empty() {
        return Err(KkdbError::ParseError("unexpected end of input".into()));
    }

    // Intercept `CREATE FULLTEXT INDEX` before handing to sqlparser.
    // sqlparser 0.61 does not expose a `full_text` field on CreateIndex.
    // We detect and parse this DDL manually.
    if let Some(stmt) = try_parse_create_fulltext_index(trimmed) {
        return stmt;
    }

    // Intercept `CREATE VECTOR INDEX` — same approach; sqlparser has no vector support.
    if let Some(stmt) = try_parse_create_vector_index(trimmed) {
        return stmt;
    }

    // Intercept `DROP VECTOR INDEX` — sqlparser has no vector support.
    if let Some(stmt) = try_parse_drop_vector_index(trimmed) {
        return stmt;
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

/// Attempt to parse `CREATE [UNIQUE] FULLTEXT INDEX name ON table (col1, col2, ...) [IF NOT EXISTS]`
/// Returns Some(Result<Statement>) if the SQL looks like a FULLTEXT index DDL, else None.
fn try_parse_create_fulltext_index(sql: &str) -> Option<Result<kk::Statement>> {
    // Fast path: must contain FULLTEXT (case-insensitive)
    let upper = sql.to_ascii_uppercase();
    if !upper.contains("FULLTEXT") {
        return None;
    }

    // Tokenise by whitespace and punctuation (comma / paren).
    // Expected grammar (simplified, case-insensitive):
    // CREATE [UNIQUE] FULLTEXT INDEX [IF NOT EXISTS] name ON table ( columns... )
    let tokens: Vec<&str> = sql.split_whitespace().collect();

    // Must start with CREATE
    if tokens
        .first()
        .map(|t| t.to_ascii_uppercase().as_str() != "CREATE")
        .unwrap_or(true)
    {
        return None;
    }

    // Walk past CREATE [UNIQUE]
    let mut idx = 1;
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "UNIQUE")
        .unwrap_or(false)
    {
        idx += 1; // skip UNIQUE (UNIQUE FULLTEXT INDEX is valid MySQL)
    }

    // Must have FULLTEXT
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "FULLTEXT")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Must have INDEX
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "INDEX")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Optional IF NOT EXISTS
    let mut if_not_exists = false;
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "IF")
        .unwrap_or(false)
    {
        idx += 1;
        if tokens
            .get(idx)
            .map(|t| t.to_ascii_uppercase() == "NOT")
            .unwrap_or(false)
        {
            idx += 1;
        }
        if tokens
            .get(idx)
            .map(|t| t.to_ascii_uppercase() == "EXISTS")
            .unwrap_or(false)
        {
            idx += 1;
        }
        if_not_exists = true;
    }

    // Index name
    let index_name = match tokens.get(idx) {
        Some(t) => t
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .to_string(),
        None => {
            return Some(Err(KkdbError::ParseError(
                "FULLTEXT INDEX missing name".into(),
            )))
        }
    };
    idx += 1;

    // ON
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "ON")
        .unwrap_or(true)
    {
        return Some(Err(KkdbError::ParseError(
            "FULLTEXT INDEX missing ON clause".into(),
        )));
    }
    idx += 1;

    // Table name
    let table_name_raw = match tokens.get(idx) {
        Some(t) => *t,
        None => {
            return Some(Err(KkdbError::ParseError(
                "FULLTEXT INDEX missing table name".into(),
            )))
        }
    };
    let _ = idx; // columns extracted from raw SQL below; idx pointer no longer needed

    // Strip table name from any leading `(`, e.g. `mytable(col1, col2)` all in one token.
    let table_name = if let Some(pos) = table_name_raw.find('(') {
        table_name_raw[..pos].to_string()
    } else {
        table_name_raw.to_string()
    };

    // Extract column list from the remainder of the SQL after the table name.
    // Find the `(...)` section in the original sql.
    let rest = &sql[sql.to_ascii_uppercase().find("ON").unwrap_or(0)..];
    let open_paren = rest.find('(');
    let close_paren = rest.rfind(')');
    let columns: Vec<String> = match (open_paren, close_paren) {
        (Some(op), Some(cp)) if cp > op => {
            let cols_str = &rest[op + 1..cp];
            cols_str
                .split(',')
                .map(|c| c.trim().trim_matches('`').trim_matches('"').to_string())
                .filter(|c| !c.is_empty())
                .collect()
        }
        _ => {
            return Some(Err(KkdbError::ParseError(
                "FULLTEXT INDEX missing column list".into(),
            )))
        }
    };

    Some(Ok(kk::Statement::CreateFulltextIndex(
        kk::CreateFulltextIndexStmt {
            index_name,
            table_name: table_name.trim_matches('`').to_string(),
            columns,
            if_not_exists,
        },
    )))
}

/// Attempt to parse:
/// `CREATE VECTOR INDEX [IF NOT EXISTS] name ON table (col) DIM N [DISTANCE COSINE|L2]`
///
/// Returns `Some(Result<Statement>)` when the SQL looks like a vector index DDL, else `None`.
fn try_parse_create_vector_index(sql: &str) -> Option<Result<kk::Statement>> {
    let upper = sql.to_ascii_uppercase();
    if !upper.contains("VECTOR") {
        return None;
    }

    let tokens: Vec<&str> = sql.split_whitespace().collect();

    // Must start with CREATE
    if tokens
        .first()
        .map(|t| t.to_ascii_uppercase() != "CREATE")
        .unwrap_or(true)
    {
        return None;
    }
    let mut idx = 1;

    // Must have VECTOR
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "VECTOR")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Must have INDEX
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "INDEX")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Optional IF NOT EXISTS
    let mut if_not_exists = false;
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "IF")
        .unwrap_or(false)
    {
        idx += 1;
        if tokens
            .get(idx)
            .map(|t| t.to_ascii_uppercase() == "NOT")
            .unwrap_or(false)
        {
            idx += 1;
        }
        if tokens
            .get(idx)
            .map(|t| t.to_ascii_uppercase() == "EXISTS")
            .unwrap_or(false)
        {
            idx += 1;
        }
        if_not_exists = true;
    }

    // Index name
    let index_name = match tokens.get(idx) {
        Some(t) => t
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .to_string(),
        None => {
            return Some(Err(KkdbError::ParseError(
                "VECTOR INDEX missing name".into(),
            )))
        }
    };
    idx += 1;

    // ON
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "ON")
        .unwrap_or(true)
    {
        return Some(Err(KkdbError::ParseError(
            "VECTOR INDEX missing ON clause".into(),
        )));
    }
    idx += 1;

    // Table name (may be followed immediately by `(col)` in same token)
    let table_name_raw = match tokens.get(idx) {
        Some(t) => *t,
        None => {
            return Some(Err(KkdbError::ParseError(
                "VECTOR INDEX missing table name".into(),
            )))
        }
    };
    let table_name = if let Some(pos) = table_name_raw.find('(') {
        table_name_raw[..pos].to_string()
    } else {
        table_name_raw.to_string()
    };
    idx += 1;

    // Extract column from `(col)` — find first `(...)` in the SQL after ON
    let after_on = &sql[upper.find("ON").unwrap_or(0)..];
    let column = {
        let op = after_on.find('(');
        let cp = after_on.find(')');
        match (op, cp) {
            (Some(o), Some(c)) if c > o => after_on[o + 1..c]
                .trim()
                .trim_matches('`')
                .trim_matches('"')
                .to_string(),
            _ => {
                return Some(Err(KkdbError::ParseError(
                    "VECTOR INDEX missing (column)".into(),
                )))
            }
        }
    };

    // Skip past the `)` in token stream; find DIM keyword
    // Advance idx past any tokens still inside / after the paren group
    while idx < tokens.len() && !tokens[idx].to_ascii_uppercase().starts_with("DIM") {
        idx += 1;
    }

    // DIM N
    let dim: u32 = if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "DIM")
        .unwrap_or(false)
    {
        idx += 1;
        match tokens.get(idx).and_then(|t| {
            let cleaned = t.trim_matches(|c: char| !c.is_ascii_digit());
            cleaned.parse::<u32>().ok()
        }) {
            Some(n) => {
                idx += 1;
                n
            }
            None => {
                return Some(Err(KkdbError::ParseError(
                    "VECTOR INDEX DIM must be a positive integer".into(),
                )))
            }
        }
    } else {
        return Some(Err(KkdbError::ParseError(
            "VECTOR INDEX requires DIM N clause".into(),
        )));
    };

    // Optional DISTANCE COSINE|L2
    let distance = if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "DISTANCE")
        .unwrap_or(false)
    {
        idx += 1;
        match tokens.get(idx).map(|t| t.to_ascii_uppercase()).as_deref() {
            Some("COSINE") | Some("COS") => kk::VecDistanceType::Cosine,
            Some("L2") | Some("EUCLIDEAN") => kk::VecDistanceType::L2,
            other => {
                return Some(Err(KkdbError::ParseError(format!(
                    "unknown distance metric {:?}; use COSINE or L2",
                    other
                ))))
            }
        }
    } else {
        kk::VecDistanceType::Cosine // default
    };

    Some(Ok(kk::Statement::CreateVectorIndex(
        kk::CreateVectorIndexStmt {
            index_name,
            table_name: table_name.trim_matches('`').to_string(),
            column,
            dim,
            distance,
            if_not_exists,
        },
    )))
}

/// Attempt to parse `DROP VECTOR INDEX [IF EXISTS] name`
///
/// Returns `Some(Result<Statement>)` when the SQL looks like a DROP VECTOR INDEX, else `None`.
fn try_parse_drop_vector_index(sql: &str) -> Option<Result<kk::Statement>> {
    let upper = sql.to_ascii_uppercase();
    // Fast path: must contain both DROP and VECTOR
    if !upper.contains("DROP") || !upper.contains("VECTOR") {
        return None;
    }

    let tokens: Vec<&str> = sql.split_whitespace().collect();

    // Must start with DROP
    if tokens
        .first()
        .map(|t| t.to_ascii_uppercase() != "DROP")
        .unwrap_or(true)
    {
        return None;
    }
    let mut idx = 1;

    // Must have VECTOR
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "VECTOR")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Must have INDEX
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() != "INDEX")
        .unwrap_or(true)
    {
        return None;
    }
    idx += 1;

    // Optional IF EXISTS
    let mut if_exists = false;
    if tokens
        .get(idx)
        .map(|t| t.to_ascii_uppercase() == "IF")
        .unwrap_or(false)
    {
        idx += 1;
        if tokens
            .get(idx)
            .map(|t| t.to_ascii_uppercase() == "EXISTS")
            .unwrap_or(false)
        {
            idx += 1;
        }
        if_exists = true;
    }

    // Index name
    let index_name = match tokens.get(idx) {
        Some(t) => t
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
            .to_string(),
        None => {
            return Some(Err(KkdbError::ParseError(
                "DROP VECTOR INDEX missing name".into(),
            )))
        }
    };

    if index_name.is_empty() {
        return Some(Err(KkdbError::ParseError(
            "DROP VECTOR INDEX missing name".into(),
        )));
    }

    Some(Ok(kk::Statement::DropVectorIndex {
        index_name,
        if_exists,
    }))
}
