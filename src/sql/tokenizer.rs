use crate::error::{KkdbError, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    // Keywords
    Select,
    From,
    Where,
    Insert,
    Into,
    Values,
    Update,
    Set,
    Delete,
    Create,
    Drop,
    Table,
    Index,
    If,
    Not,
    Exists,
    And,
    Or,
    Is,
    Null,
    In,
    Like,
    Order,
    By,
    Asc,
    Desc,
    Limit,
    Offset,
    Primary,
    Key,
    Autoincrement,
    Unique,
    Default,
    Integer,
    Real,
    Text,
    Blob,
    As,
    Join,
    On,
    Inner,
    Left,
    Right,
    Outer,
    Group,
    Having,
    Distinct,
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Begin,
    Commit,
    Rollback,
    Transaction,
    Explain,
    Between,
    Alter,
    Add,
    Rename,
    Column,
    To,

    // Literals
    IntegerLiteral(i64),
    RealLiteral(f64),
    StringLiteral(String),
    BlobLiteral(Vec<u8>),

    // Identifiers
    Identifier(String),

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Equal,
    NotEqual, // != or <>
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Concat, // ||

    // Punctuation
    LeftParen,
    RightParen,
    Comma,
    Semicolon,
    Dot,

    // Special
    Eof,
}

impl Token {
    pub fn is_keyword(s: &str) -> Option<Token> {
        // Dispatch on length first, then use eq_ignore_ascii_case (zero-alloc).
        match s.len() {
            2 => {
                if s.eq_ignore_ascii_case("IF") {
                    return Some(Token::If);
                }
                if s.eq_ignore_ascii_case("IN") {
                    return Some(Token::In);
                }
                if s.eq_ignore_ascii_case("IS") {
                    return Some(Token::Is);
                }
                if s.eq_ignore_ascii_case("OR") {
                    return Some(Token::Or);
                }
                if s.eq_ignore_ascii_case("AS") {
                    return Some(Token::As);
                }
                if s.eq_ignore_ascii_case("ON") {
                    return Some(Token::On);
                }
                if s.eq_ignore_ascii_case("BY") {
                    return Some(Token::By);
                }
                if s.eq_ignore_ascii_case("TO") {
                    return Some(Token::To);
                }
            }
            3 => {
                if s.eq_ignore_ascii_case("SET") {
                    return Some(Token::Set);
                }
                if s.eq_ignore_ascii_case("NOT") {
                    return Some(Token::Not);
                }
                if s.eq_ignore_ascii_case("AND") {
                    return Some(Token::And);
                }
                if s.eq_ignore_ascii_case("ASC") {
                    return Some(Token::Asc);
                }
                if s.eq_ignore_ascii_case("KEY") {
                    return Some(Token::Key);
                }
                if s.eq_ignore_ascii_case("INT") {
                    return Some(Token::Integer);
                }
                if s.eq_ignore_ascii_case("SUM") {
                    return Some(Token::Sum);
                }
                if s.eq_ignore_ascii_case("AVG") {
                    return Some(Token::Avg);
                }
                if s.eq_ignore_ascii_case("MIN") {
                    return Some(Token::Min);
                }
                if s.eq_ignore_ascii_case("MAX") {
                    return Some(Token::Max);
                }
                if s.eq_ignore_ascii_case("ADD") {
                    return Some(Token::Add);
                }
            }
            4 => {
                if s.eq_ignore_ascii_case("FROM") {
                    return Some(Token::From);
                }
                if s.eq_ignore_ascii_case("INTO") {
                    return Some(Token::Into);
                }
                if s.eq_ignore_ascii_case("DROP") {
                    return Some(Token::Drop);
                }
                if s.eq_ignore_ascii_case("NULL") {
                    return Some(Token::Null);
                }
                if s.eq_ignore_ascii_case("LIKE") {
                    return Some(Token::Like);
                }
                if s.eq_ignore_ascii_case("DESC") {
                    return Some(Token::Desc);
                }
                if s.eq_ignore_ascii_case("REAL") {
                    return Some(Token::Real);
                }
                if s.eq_ignore_ascii_case("TEXT") {
                    return Some(Token::Text);
                }
                if s.eq_ignore_ascii_case("BLOB") {
                    return Some(Token::Blob);
                }
                if s.eq_ignore_ascii_case("CHAR") {
                    return Some(Token::Text);
                }
                if s.eq_ignore_ascii_case("JOIN") {
                    return Some(Token::Join);
                }
                if s.eq_ignore_ascii_case("LEFT") {
                    return Some(Token::Left);
                }
            }
            5 => {
                if s.eq_ignore_ascii_case("WHERE") {
                    return Some(Token::Where);
                }
                if s.eq_ignore_ascii_case("TABLE") {
                    return Some(Token::Table);
                }
                if s.eq_ignore_ascii_case("INDEX") {
                    return Some(Token::Index);
                }
                if s.eq_ignore_ascii_case("ORDER") {
                    return Some(Token::Order);
                }
                if s.eq_ignore_ascii_case("LIMIT") {
                    return Some(Token::Limit);
                }
                if s.eq_ignore_ascii_case("FLOAT") {
                    return Some(Token::Real);
                }
                if s.eq_ignore_ascii_case("INNER") {
                    return Some(Token::Inner);
                }
                if s.eq_ignore_ascii_case("RIGHT") {
                    return Some(Token::Right);
                }
                if s.eq_ignore_ascii_case("OUTER") {
                    return Some(Token::Outer);
                }
                if s.eq_ignore_ascii_case("GROUP") {
                    return Some(Token::Group);
                }
                if s.eq_ignore_ascii_case("COUNT") {
                    return Some(Token::Count);
                }
                if s.eq_ignore_ascii_case("BEGIN") {
                    return Some(Token::Begin);
                }
                if s.eq_ignore_ascii_case("ALTER") {
                    return Some(Token::Alter);
                }
            }
            6 => {
                if s.eq_ignore_ascii_case("SELECT") {
                    return Some(Token::Select);
                }
                if s.eq_ignore_ascii_case("INSERT") {
                    return Some(Token::Insert);
                }
                if s.eq_ignore_ascii_case("VALUES") {
                    return Some(Token::Values);
                }
                if s.eq_ignore_ascii_case("UPDATE") {
                    return Some(Token::Update);
                }
                if s.eq_ignore_ascii_case("DELETE") {
                    return Some(Token::Delete);
                }
                if s.eq_ignore_ascii_case("CREATE") {
                    return Some(Token::Create);
                }
                if s.eq_ignore_ascii_case("EXISTS") {
                    return Some(Token::Exists);
                }
                if s.eq_ignore_ascii_case("OFFSET") {
                    return Some(Token::Offset);
                }
                if s.eq_ignore_ascii_case("UNIQUE") {
                    return Some(Token::Unique);
                }
                if s.eq_ignore_ascii_case("DOUBLE") {
                    return Some(Token::Real);
                }
                if s.eq_ignore_ascii_case("HAVING") {
                    return Some(Token::Having);
                }
                if s.eq_ignore_ascii_case("COMMIT") {
                    return Some(Token::Commit);
                }
                if s.eq_ignore_ascii_case("RENAME") {
                    return Some(Token::Rename);
                }
                if s.eq_ignore_ascii_case("COLUMN") {
                    return Some(Token::Column);
                }
            }
            7 => {
                if s.eq_ignore_ascii_case("PRIMARY") {
                    return Some(Token::Primary);
                }
                if s.eq_ignore_ascii_case("DEFAULT") {
                    return Some(Token::Default);
                }
                if s.eq_ignore_ascii_case("INTEGER") {
                    return Some(Token::Integer);
                }
                if s.eq_ignore_ascii_case("VARCHAR") {
                    return Some(Token::Text);
                }
                if s.eq_ignore_ascii_case("BETWEEN") {
                    return Some(Token::Between);
                }
                if s.eq_ignore_ascii_case("EXPLAIN") {
                    return Some(Token::Explain);
                }
            }
            8 => {
                if s.eq_ignore_ascii_case("DISTINCT") {
                    return Some(Token::Distinct);
                }
                if s.eq_ignore_ascii_case("ROLLBACK") {
                    return Some(Token::Rollback);
                }
            }
            11 => {
                if s.eq_ignore_ascii_case("TRANSACTION") {
                    return Some(Token::Transaction);
                }
            }
            13 => {
                if s.eq_ignore_ascii_case("AUTOINCREMENT") {
                    return Some(Token::Autoincrement);
                }
            }
            _ => {}
        }
        None
    }
}

pub struct Tokenizer<'a> {
    input: &'a [u8],
    pos: usize,
}

impl<'a> Tokenizer<'a> {
    pub fn new(input: &'a str) -> Self {
        Tokenizer {
            input: input.as_bytes(),
            pos: 0,
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>> {
        let mut tokens = Vec::with_capacity(self.input.len() / 4 + 4);
        loop {
            let tok = self.next_token()?;
            if tok == Token::Eof {
                tokens.push(tok);
                break;
            }
            tokens.push(tok);
        }
        Ok(tokens)
    }

    #[inline]
    fn peek_char(&self) -> Option<char> {
        self.input.get(self.pos).map(|&b| b as char)
    }

    #[inline]
    fn advance(&mut self) -> Option<char> {
        let ch = self.input.get(self.pos).map(|&b| b as char);
        self.pos += 1;
        ch
    }

    #[inline]
    fn skip_whitespace(&mut self) {
        while let Some(&b) = self.input.get(self.pos) {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'-' && self.input.get(self.pos + 1) == Some(&b'-') {
                // line comment
                self.pos += 2;
                while let Some(&b2) = self.input.get(self.pos) {
                    self.pos += 1;
                    if b2 == b'\n' {
                        break;
                    }
                }
            } else if b == b'/' && self.input.get(self.pos + 1) == Some(&b'*') {
                // block comment
                self.advance();
                self.advance();
                let mut depth = 1;
                while depth > 0 {
                    match self.advance() {
                        Some('/') if self.peek_char() == Some('*') => {
                            self.advance();
                            depth += 1;
                        }
                        Some('*') if self.peek_char() == Some('/') => {
                            self.advance();
                            depth -= 1;
                        }
                        None => break,
                        _ => {}
                    }
                }
            } else {
                break;
            }
        }
    }

    #[inline]
    pub fn next_token(&mut self) -> Result<Token> {
        self.skip_whitespace();

        let b = match self.input.get(self.pos) {
            Some(&b) => b,
            None => return Ok(Token::Eof),
        };

        match b {
            b'(' => {
                self.pos += 1;
                Ok(Token::LeftParen)
            }
            b')' => {
                self.pos += 1;
                Ok(Token::RightParen)
            }
            b',' => {
                self.pos += 1;
                Ok(Token::Comma)
            }
            b';' => {
                self.pos += 1;
                Ok(Token::Semicolon)
            }
            b'.' => {
                self.pos += 1;
                Ok(Token::Dot)
            }
            b'+' => {
                self.pos += 1;
                Ok(Token::Plus)
            }
            b'-' => {
                self.pos += 1;
                Ok(Token::Minus)
            }
            b'*' => {
                self.pos += 1;
                Ok(Token::Star)
            }
            b'/' => {
                self.pos += 1;
                Ok(Token::Slash)
            }
            b'%' => {
                self.pos += 1;
                Ok(Token::Percent)
            }
            b'=' => {
                self.pos += 1;
                Ok(Token::Equal)
            }
            b'<' => {
                self.pos += 1;
                match self.input.get(self.pos) {
                    Some(&b'=') => {
                        self.pos += 1;
                        Ok(Token::LessEqual)
                    }
                    Some(&b'>') => {
                        self.pos += 1;
                        Ok(Token::NotEqual)
                    }
                    _ => Ok(Token::Less),
                }
            }
            b'>' => {
                self.pos += 1;
                if self.input.get(self.pos) == Some(&b'=') {
                    self.pos += 1;
                    Ok(Token::GreaterEqual)
                } else {
                    Ok(Token::Greater)
                }
            }
            b'!' => {
                self.pos += 1;
                if self.input.get(self.pos) == Some(&b'=') {
                    self.pos += 1;
                    Ok(Token::NotEqual)
                } else {
                    Err(KkdbError::SyntaxError("unexpected character '!'".into()))
                }
            }
            b'|' => {
                self.pos += 1;
                if self.input.get(self.pos) == Some(&b'|') {
                    self.pos += 1;
                    Ok(Token::Concat)
                } else {
                    Err(KkdbError::SyntaxError("unexpected character '|'".into()))
                }
            }
            b'\'' => self.read_string(),
            b'"' => self.read_quoted_identifier(),
            b'x' | b'X' if self.input.get(self.pos + 1) == Some(&b'\'') => self.read_blob_literal(),
            b'0'..=b'9' => self.read_number(),
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.read_identifier_or_keyword(),
            _ => {
                self.pos += 1;
                Err(KkdbError::SyntaxError(format!(
                    "unexpected character '{}'",
                    b as char
                )))
            }
        }
    }

    fn read_string(&mut self) -> Result<Token> {
        self.pos += 1; // skip opening quote
        let start = self.pos;
        // Fast path: scan for closing quote without escaped quotes
        while let Some(&b) = self.input.get(self.pos) {
            if b == b'\'' {
                // Check for escaped quote ''
                if self.input.get(self.pos + 1) == Some(&b'\'') {
                    // Has escaped quotes — fall back to char-by-char
                    let mut s = String::from(unsafe {
                        std::str::from_utf8_unchecked(&self.input[start..self.pos])
                    });
                    self.pos += 2; // skip ''
                    s.push('\'');
                    loop {
                        match self.input.get(self.pos) {
                            Some(&b'\'') => {
                                self.pos += 1;
                                if self.input.get(self.pos) == Some(&b'\'') {
                                    self.pos += 1;
                                    s.push('\'');
                                } else {
                                    return Ok(Token::StringLiteral(s));
                                }
                            }
                            Some(&b) => {
                                self.pos += 1;
                                s.push(b as char);
                            }
                            None => {
                                return Err(KkdbError::SyntaxError(
                                    "unterminated string literal".into(),
                                ))
                            }
                        }
                    }
                } else {
                    // No escaped quotes — zero-copy slice
                    let s = unsafe { std::str::from_utf8_unchecked(&self.input[start..self.pos]) }
                        .to_string();
                    self.pos += 1; // skip closing quote
                    return Ok(Token::StringLiteral(s));
                }
            }
            self.pos += 1;
        }
        Err(KkdbError::SyntaxError("unterminated string literal".into()))
    }

    fn read_quoted_identifier(&mut self) -> Result<Token> {
        self.pos += 1; // skip opening "
        let start = self.pos;
        // Fast path: scan for closing quote without escaped quotes
        while let Some(&b) = self.input.get(self.pos) {
            if b == b'"' {
                if self.input.get(self.pos + 1) == Some(&b'"') {
                    // Has escaped quotes — fall back to char-by-char
                    let mut s = String::from(unsafe {
                        std::str::from_utf8_unchecked(&self.input[start..self.pos])
                    });
                    self.pos += 2;
                    s.push('"');
                    loop {
                        match self.input.get(self.pos) {
                            Some(&b'"') => {
                                self.pos += 1;
                                if self.input.get(self.pos) == Some(&b'"') {
                                    self.pos += 1;
                                    s.push('"');
                                } else {
                                    return Ok(Token::Identifier(s));
                                }
                            }
                            Some(&b) => {
                                self.pos += 1;
                                s.push(b as char);
                            }
                            None => {
                                return Err(KkdbError::SyntaxError(
                                    "unterminated quoted identifier".into(),
                                ))
                            }
                        }
                    }
                } else {
                    // No escaped quotes — zero-copy slice
                    let s = unsafe { std::str::from_utf8_unchecked(&self.input[start..self.pos]) }
                        .to_string();
                    self.pos += 1;
                    return Ok(Token::Identifier(s));
                }
            }
            self.pos += 1;
        }
        Err(KkdbError::SyntaxError(
            "unterminated quoted identifier".into(),
        ))
    }

    fn read_blob_literal(&mut self) -> Result<Token> {
        self.advance(); // skip 'x'
        self.advance(); // skip '\''
        let mut hex = String::new();
        loop {
            match self.advance() {
                Some('\'') => {
                    if hex.len() % 2 != 0 {
                        return Err(KkdbError::SyntaxError(
                            "blob literal must have even number of hex digits".into(),
                        ));
                    }
                    let bytes = (0..hex.len())
                        .step_by(2)
                        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16))
                        .collect::<std::result::Result<Vec<u8>, _>>()
                        .map_err(|_| {
                            KkdbError::SyntaxError("invalid hex in blob literal".into())
                        })?;
                    return Ok(Token::BlobLiteral(bytes));
                }
                Some(ch) if ch.is_ascii_hexdigit() => hex.push(ch),
                Some(ch) => {
                    return Err(KkdbError::SyntaxError(format!(
                        "invalid character '{}' in blob literal",
                        ch
                    )))
                }
                None => return Err(KkdbError::SyntaxError("unterminated blob literal".into())),
            }
        }
    }

    fn read_number(&mut self) -> Result<Token> {
        let start = self.pos;
        let mut is_real = false;
        let mut has_exp = false;

        while let Some(&b) = self.input.get(self.pos) {
            if b.is_ascii_digit() {
                self.pos += 1;
            } else if b == b'.' && !is_real {
                is_real = true;
                self.pos += 1;
            } else if (b == b'e' || b == b'E') && !has_exp {
                is_real = true;
                has_exp = true;
                self.pos += 1;
                if let Some(&sign) = self.input.get(self.pos) {
                    if sign == b'+' || sign == b'-' {
                        self.pos += 1;
                    }
                }
            } else {
                break;
            }
        }

        if is_real {
            let s = unsafe { std::str::from_utf8_unchecked(&self.input[start..self.pos]) };
            let v: f64 = s
                .parse()
                .map_err(|_| KkdbError::SyntaxError(format!("invalid number: {}", s)))?;
            Ok(Token::RealLiteral(v))
        } else {
            // Fast path: parse integer directly from bytes (avoids str::parse overhead)
            let mut v: i64 = 0;
            for &b in &self.input[start..self.pos] {
                v = v * 10 + (b - b'0') as i64;
            }
            Ok(Token::IntegerLiteral(v))
        }
    }

    fn read_identifier_or_keyword(&mut self) -> Result<Token> {
        let start = self.pos;
        while let Some(&b) = self.input.get(self.pos) {
            if b.is_ascii_alphanumeric() || b == b'_' {
                self.pos += 1;
            } else {
                break;
            }
        }
        // SAFETY: input is &str bytes so this slice is valid UTF-8 for ASCII ident chars
        let s = unsafe { std::str::from_utf8_unchecked(&self.input[start..self.pos]) };
        if let Some(kw) = Token::is_keyword(s) {
            Ok(kw)
        } else {
            Ok(Token::Identifier(s.to_string()))
        }
    }
}

#[cfg(test)]
#[path = "tokenizer_tests.rs"]
mod tests;
