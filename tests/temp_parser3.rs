use kkdb::sql::parser::parse_sql;
use std::fs;

fn main() {
    let sql = "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW INSERT INTO audit (log) VALUES ('inserted');";
    let ast = parse_sql(sql).unwrap();
    fs::write("e:/ai/kkdb/ast_output.txt", format!("{:#?}", ast)).unwrap();
}
