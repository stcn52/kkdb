use super::*;

fn tokenize(input: &str) -> Vec<Token> {
    let mut t = Tokenizer::new(input);
    t.tokenize().unwrap()
}

// ---- Keywords ----

#[test]
fn test_all_keywords() {
    let kws = vec![
        ("SELECT", Token::Select),
        ("FROM", Token::From),
        ("WHERE", Token::Where),
        ("INSERT", Token::Insert),
        ("INTO", Token::Into),
        ("VALUES", Token::Values),
        ("UPDATE", Token::Update),
        ("SET", Token::Set),
        ("DELETE", Token::Delete),
        ("CREATE", Token::Create),
        ("DROP", Token::Drop),
        ("TABLE", Token::Table),
        ("INDEX", Token::Index),
        ("IF", Token::If),
        ("NOT", Token::Not),
        ("EXISTS", Token::Exists),
        ("AND", Token::And),
        ("OR", Token::Or),
        ("IS", Token::Is),
        ("NULL", Token::Null),
        ("IN", Token::In),
        ("LIKE", Token::Like),
        ("ORDER", Token::Order),
        ("BY", Token::By),
        ("ASC", Token::Asc),
        ("DESC", Token::Desc),
        ("LIMIT", Token::Limit),
        ("OFFSET", Token::Offset),
        ("PRIMARY", Token::Primary),
        ("KEY", Token::Key),
        ("AUTOINCREMENT", Token::Autoincrement),
        ("UNIQUE", Token::Unique),
        ("DEFAULT", Token::Default),
        ("INTEGER", Token::Integer),
        ("INT", Token::Integer),
        ("REAL", Token::Real),
        ("FLOAT", Token::Real),
        ("DOUBLE", Token::Real),
        ("TEXT", Token::Text),
        ("VARCHAR", Token::Text),
        ("CHAR", Token::Text),
        ("BLOB", Token::Blob),
        ("AS", Token::As),
        ("JOIN", Token::Join),
        ("ON", Token::On),
        ("INNER", Token::Inner),
        ("LEFT", Token::Left),
        ("RIGHT", Token::Right),
        ("OUTER", Token::Outer),
        ("GROUP", Token::Group),
        ("HAVING", Token::Having),
        ("DISTINCT", Token::Distinct),
        ("COUNT", Token::Count),
        ("SUM", Token::Sum),
        ("AVG", Token::Avg),
        ("MIN", Token::Min),
        ("MAX", Token::Max),
        ("BEGIN", Token::Begin),
        ("COMMIT", Token::Commit),
        ("ROLLBACK", Token::Rollback),
        ("TRANSACTION", Token::Transaction),
        ("EXPLAIN", Token::Explain),
        ("BETWEEN", Token::Between),
        ("ALTER", Token::Alter),
        ("ADD", Token::Add),
        ("RENAME", Token::Rename),
        ("COLUMN", Token::Column),
        ("TO", Token::To),
    ];
    for (input, expected) in kws {
        let tokens = tokenize(input);
        assert_eq!(tokens[0], expected, "keyword: {}", input);
    }
}

#[test]
fn test_keyword_case_insensitive() {
    let tokens = tokenize("select FROM where");
    assert_eq!(tokens[0], Token::Select);
    assert_eq!(tokens[1], Token::From);
    assert_eq!(tokens[2], Token::Where);
}

#[test]
fn test_non_keyword_identifier() {
    let tokens = tokenize("my_table");
    assert_eq!(tokens[0], Token::Identifier("my_table".into()));
    assert!(Token::is_keyword("foobar").is_none());
}

// ---- Literals ----

#[test]
fn test_integer_literal() {
    let tokens = tokenize("42");
    assert_eq!(tokens[0], Token::IntegerLiteral(42));
}

#[test]
fn test_real_literal_dot() {
    let tokens = tokenize("3.14");
    assert_eq!(tokens[0], Token::RealLiteral(3.14));
}

#[test]
fn test_real_literal_exponent() {
    let tokens = tokenize("1e10");
    assert_eq!(tokens[0], Token::RealLiteral(1e10));
}

#[test]
fn test_real_literal_exponent_sign() {
    let tokens = tokenize("2E-3");
    assert_eq!(tokens[0], Token::RealLiteral(2e-3));
    let tokens = tokenize("5E+2");
    assert_eq!(tokens[0], Token::RealLiteral(5e2));
}

#[test]
fn test_string_literal() {
    let tokens = tokenize("'hello world'");
    assert_eq!(tokens[0], Token::StringLiteral("hello world".into()));
}

#[test]
fn test_string_literal_escaped_quote() {
    let tokens = tokenize("'it''s'");
    assert_eq!(tokens[0], Token::StringLiteral("it's".into()));
}

#[test]
fn test_string_literal_unterminated() {
    let mut t = Tokenizer::new("'unterminated");
    assert!(t.tokenize().is_err());
}

#[test]
fn test_blob_literal() {
    let tokens = tokenize("x'DEADBEEF'");
    assert_eq!(tokens[0], Token::BlobLiteral(vec![0xDE, 0xAD, 0xBE, 0xEF]));
}

#[test]
fn test_blob_literal_uppercase_x() {
    let tokens = tokenize("X'AB'");
    assert_eq!(tokens[0], Token::BlobLiteral(vec![0xAB]));
}

#[test]
fn test_blob_literal_odd_digits() {
    let mut t = Tokenizer::new("x'ABC'");
    assert!(t.tokenize().is_err());
}

#[test]
fn test_blob_literal_invalid_hex() {
    let mut t = Tokenizer::new("x'GG'");
    assert!(t.tokenize().is_err());
}

#[test]
fn test_blob_literal_unterminated() {
    let mut t = Tokenizer::new("x'AB");
    assert!(t.tokenize().is_err());
}

// ---- Quoted identifiers ----

#[test]
fn test_quoted_identifier() {
    let tokens = tokenize("\"my table\"");
    assert_eq!(tokens[0], Token::Identifier("my table".into()));
}

#[test]
fn test_quoted_identifier_escaped() {
    let tokens = tokenize("\"a\"\"b\"");
    assert_eq!(tokens[0], Token::Identifier("a\"b".into()));
}

#[test]
fn test_quoted_identifier_unterminated() {
    let mut t = Tokenizer::new("\"unterminated");
    assert!(t.tokenize().is_err());
}

// ---- Operators ----

#[test]
fn test_operators() {
    let tokens = tokenize("+ - * / % = < <= > >= !=");
    assert_eq!(tokens[0], Token::Plus);
    assert_eq!(tokens[1], Token::Minus);
    assert_eq!(tokens[2], Token::Star);
    assert_eq!(tokens[3], Token::Slash);
    assert_eq!(tokens[4], Token::Percent);
    assert_eq!(tokens[5], Token::Equal);
    assert_eq!(tokens[6], Token::Less);
    assert_eq!(tokens[7], Token::LessEqual);
    assert_eq!(tokens[8], Token::Greater);
    assert_eq!(tokens[9], Token::GreaterEqual);
    assert_eq!(tokens[10], Token::NotEqual);
}

#[test]
fn test_not_equal_diamond() {
    let tokens = tokenize("<>");
    assert_eq!(tokens[0], Token::NotEqual);
}

#[test]
fn test_concat_operator() {
    let tokens = tokenize("||");
    assert_eq!(tokens[0], Token::Concat);
}

#[test]
fn test_single_pipe_error() {
    let mut t = Tokenizer::new("|");
    assert!(t.tokenize().is_err());
}

#[test]
fn test_single_bang_error() {
    let mut t = Tokenizer::new("!");
    assert!(t.tokenize().is_err());
}

// ---- Punctuation ----

#[test]
fn test_punctuation() {
    let tokens = tokenize("( ) , ; .");
    assert_eq!(tokens[0], Token::LeftParen);
    assert_eq!(tokens[1], Token::RightParen);
    assert_eq!(tokens[2], Token::Comma);
    assert_eq!(tokens[3], Token::Semicolon);
    assert_eq!(tokens[4], Token::Dot);
}

// ---- Comments ----

#[test]
fn test_line_comment() {
    let tokens = tokenize("SELECT -- this is a comment\n42");
    assert_eq!(tokens[0], Token::Select);
    assert_eq!(tokens[1], Token::IntegerLiteral(42));
}

#[test]
fn test_block_comment() {
    let tokens = tokenize("SELECT /* block */ 42");
    assert_eq!(tokens[0], Token::Select);
    assert_eq!(tokens[1], Token::IntegerLiteral(42));
}

#[test]
fn test_nested_block_comment() {
    let tokens = tokenize("SELECT /* outer /* inner */ still comment */ 42");
    assert_eq!(tokens[0], Token::Select);
    assert_eq!(tokens[1], Token::IntegerLiteral(42));
}

// ---- Whitespace ----

#[test]
fn test_whitespace_skipping() {
    let tokens = tokenize("  \t\n  42  \r\n  ");
    assert_eq!(tokens[0], Token::IntegerLiteral(42));
    assert_eq!(tokens[1], Token::Eof);
}

// ---- Empty / EOF ----

#[test]
fn test_empty_input() {
    let tokens = tokenize("");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0], Token::Eof);
}

// ---- Unknown character ----

#[test]
fn test_unknown_character() {
    let mut t = Tokenizer::new("@");
    assert!(t.tokenize().is_err());
}

// ---- Full SQL ----

#[test]
fn test_full_select_tokenization() {
    let tokens = tokenize("SELECT id, name FROM users WHERE age > 18;");
    assert_eq!(tokens[0], Token::Select);
    assert_eq!(tokens[1], Token::Identifier("id".into()));
    assert_eq!(tokens[2], Token::Comma);
    assert_eq!(tokens[3], Token::Identifier("name".into()));
    assert_eq!(tokens[4], Token::From);
    assert_eq!(tokens[5], Token::Identifier("users".into()));
    assert_eq!(tokens[6], Token::Where);
    assert_eq!(tokens[7], Token::Identifier("age".into()));
    assert_eq!(tokens[8], Token::Greater);
    assert_eq!(tokens[9], Token::IntegerLiteral(18));
    assert_eq!(tokens[10], Token::Semicolon);
    assert_eq!(tokens[11], Token::Eof);
}

#[test]
fn test_create_table_tokenization() {
    let tokens = tokenize("CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT);");
    assert_eq!(tokens[0], Token::Create);
    assert_eq!(tokens[1], Token::Table);
    assert_eq!(tokens[2], Token::Identifier("t1".into()));
    assert_eq!(tokens[3], Token::LeftParen);
    assert_eq!(tokens[4], Token::Identifier("id".into()));
    assert_eq!(tokens[5], Token::Integer);
    assert_eq!(tokens[6], Token::Primary);
    assert_eq!(tokens[7], Token::Key);
    assert_eq!(tokens[8], Token::Autoincrement);
    assert_eq!(tokens[9], Token::RightParen);
    assert_eq!(tokens[10], Token::Semicolon);
}

#[test]
fn test_line_comment_at_eof() {
    let tokens = tokenize("42 -- comment no newline");
    assert_eq!(tokens[0], Token::IntegerLiteral(42));
    assert_eq!(tokens[1], Token::Eof);
}

#[test]
fn test_block_comment_unterminated() {
    // Should consume till EOF without error (just stops)
    let tokens = tokenize("42 /* unterminated");
    assert_eq!(tokens[0], Token::IntegerLiteral(42));
    assert_eq!(tokens[1], Token::Eof);
}

// ---- New keywords (BETWEEN, ALTER TABLE support) ----

#[test]
fn test_between_keyword() {
    let tokens = tokenize("BETWEEN");
    assert_eq!(tokens[0], Token::Between);
}

#[test]
fn test_alter_keywords() {
    let kws = vec![
        ("ALTER", Token::Alter),
        ("ADD", Token::Add),
        ("RENAME", Token::Rename),
        ("COLUMN", Token::Column),
        ("TO", Token::To),
    ];
    for (input, expected) in kws {
        let tokens = tokenize(input);
        assert_eq!(tokens[0], expected, "keyword: {}", input);
    }
}

#[test]
fn test_alter_keywords_case_insensitive() {
    assert_eq!(tokenize("alter")[0], Token::Alter);
    assert_eq!(tokenize("Add")[0], Token::Add);
    assert_eq!(tokenize("rEnAmE")[0], Token::Rename);
    assert_eq!(tokenize("column")[0], Token::Column);
    assert_eq!(tokenize("To")[0], Token::To);
}

#[test]
fn test_alter_table_full_tokenization() {
    let tokens = tokenize("ALTER TABLE t1 ADD COLUMN val INTEGER");
    assert_eq!(tokens[0], Token::Alter);
    assert_eq!(tokens[1], Token::Table);
    assert_eq!(tokens[2], Token::Identifier("t1".into()));
    assert_eq!(tokens[3], Token::Add);
    assert_eq!(tokens[4], Token::Column);
    assert_eq!(tokens[5], Token::Identifier("val".into()));
    assert_eq!(tokens[6], Token::Integer);
    assert_eq!(tokens[7], Token::Eof);
}

#[test]
fn test_rename_table_tokenization() {
    let tokens = tokenize("ALTER TABLE t1 RENAME TO t2");
    assert_eq!(tokens[0], Token::Alter);
    assert_eq!(tokens[3], Token::Rename);
    assert_eq!(tokens[4], Token::To);
    assert_eq!(tokens[5], Token::Identifier("t2".into()));
}

#[test]
fn test_between_in_expression() {
    let tokens = tokenize("a BETWEEN 1 AND 10");
    assert_eq!(tokens[0], Token::Identifier("a".into()));
    assert_eq!(tokens[1], Token::Between);
    assert_eq!(tokens[2], Token::IntegerLiteral(1));
    assert_eq!(tokens[3], Token::And);
    assert_eq!(tokens[4], Token::IntegerLiteral(10));
}

// ---- Edge cases ----

#[test]
fn test_adjacent_operators() {
    let tokens = tokenize("1+-2");
    assert_eq!(tokens[0], Token::IntegerLiteral(1));
    assert_eq!(tokens[1], Token::Plus);
    assert_eq!(tokens[2], Token::Minus);
    assert_eq!(tokens[3], Token::IntegerLiteral(2));
}

#[test]
fn test_multiple_semicolons() {
    let tokens = tokenize(";;");
    assert_eq!(tokens[0], Token::Semicolon);
    assert_eq!(tokens[1], Token::Semicolon);
}

#[test]
fn test_number_then_identifier() {
    // "123abc" → integer 123 + identifier abc
    let tokens = tokenize("123 abc");
    assert_eq!(tokens[0], Token::IntegerLiteral(123));
    assert_eq!(tokens[1], Token::Identifier("abc".into()));
}

#[test]
fn test_string_with_spaces() {
    let tokens = tokenize("'  hello  world  '");
    assert_eq!(tokens[0], Token::StringLiteral("  hello  world  ".into()));
}

#[test]
fn test_empty_string_literal() {
    let tokens = tokenize("''");
    assert_eq!(tokens[0], Token::StringLiteral("".into()));
}

#[test]
fn test_complex_sql_tokenization() {
    let sql = "SELECT a, COUNT(DISTINCT b) FROM t1 WHERE c BETWEEN 1 AND 10 GROUP BY a HAVING COUNT(*) > 5 ORDER BY a DESC LIMIT 10 OFFSET 20;";
    let tokens = tokenize(sql);
    assert_eq!(tokens[0], Token::Select);
    // Just verify it completes without error and ends with Eof
    assert_eq!(*tokens.last().unwrap(), Token::Eof);
}

#[test]
fn test_insert_multi_row_tokenization() {
    let tokens = tokenize("INSERT INTO t1 VALUES (1, 'a'), (2, 'b'), (3, 'c')");
    assert_eq!(tokens[0], Token::Insert);
    assert_eq!(tokens[1], Token::Into);
    // Verify commas: 1 within each tuple × 3 + 2 between tuples = 5
    let comma_count = tokens.iter().filter(|t| **t == Token::Comma).count();
    assert_eq!(comma_count, 5);
}

#[test]
fn test_zero_literal() {
    let tokens = tokenize("0");
    assert_eq!(tokens[0], Token::IntegerLiteral(0));
}

#[test]
fn test_large_integer() {
    let tokens = tokenize("9999999999");
    assert_eq!(tokens[0], Token::IntegerLiteral(9999999999));
}

#[test]
fn test_real_no_integer_part() {
    // .5 should be tokenized as Dot + IntegerLiteral
    let tokens = tokenize(".5");
    assert_eq!(tokens[0], Token::Dot);
    assert_eq!(tokens[1], Token::IntegerLiteral(5));
}

#[test]
fn test_underscore_identifier() {
    let tokens = tokenize("_foo");
    assert_eq!(tokens[0], Token::Identifier("_foo".into()));
    let tokens = tokenize("__init");
    assert_eq!(tokens[0], Token::Identifier("__init".into()));
}

#[test]
fn test_identifier_with_digits() {
    let tokens = tokenize("col1");
    assert_eq!(tokens[0], Token::Identifier("col1".into()));
    let tokens = tokenize("table2name");
    assert_eq!(tokens[0], Token::Identifier("table2name".into()));
}

#[test]
fn test_keyword_prefix_is_identifier() {
    // "SELECTING" should NOT match "SELECT"
    let tokens = tokenize("SELECTING");
    assert_eq!(tokens[0], Token::Identifier("SELECTING".into()));
    let tokens = tokenize("ORDERS");
    assert_eq!(tokens[0], Token::Identifier("ORDERS".into()));
    let tokens = tokenize("INSERTED");
    assert_eq!(tokens[0], Token::Identifier("INSERTED".into()));
}

#[test]
fn test_real_dot_and_exponent() {
    let tokens = tokenize("3.14e2");
    assert_eq!(tokens[0], Token::RealLiteral(314.0));
}

#[test]
fn test_empty_blob_literal() {
    let tokens = tokenize("x''");
    assert_eq!(tokens[0], Token::BlobLiteral(vec![]));
}

#[test]
fn test_real_starting_with_zero() {
    let tokens = tokenize("0.5");
    assert_eq!(tokens[0], Token::RealLiteral(0.5));
}

#[test]
fn test_blob_lowercase_hex() {
    let tokens = tokenize("x'ab'");
    assert_eq!(tokens[0], Token::BlobLiteral(vec![0xAB]));
}
