use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

#[test]
fn test_fts_parser() {
    let sqls = [
        "CREATE VIRTUAL TABLE fts_table USING fts5(title, body);",
        "CREATE TABLE t (id INT, body TEXT, FULLTEXT(body));",
        "SELECT * FROM fts_table WHERE fts_table MATCH 'search text';",
        "SELECT * FROM t WHERE MATCH(body) AGAINST('test');"
    ];
    for sql in sqls {
        println!("----");
        println!("SQL: {}", sql);
        match Parser::parse_sql(&SQLiteDialect{}, sql) {
            Ok(ast) => println!("Parsed OK: {:#?}", ast[0]),
            Err(e) => println!("Parse Error: {:?}", e),
        }
    }
}
