use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use std::fs;

#[test]
fn fetch_ast() {
    let sql = "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW INSERT INTO audit (log) VALUES ('inserted');";
    let ast = Parser::parse_sql(&SQLiteDialect{}, sql).unwrap();
    fs::write("e:/ai/kkdb/ast_output.txt", format!("{:#?}", ast)).unwrap();
}
