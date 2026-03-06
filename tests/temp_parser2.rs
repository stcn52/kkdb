use kkdb::sql::parser::parse_sql;

#[test]
fn try_parser() {
    let sqls = [
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW BEGIN INSERT INTO audit (log) VALUES ('inserted'); END;",
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW BEGIN INSERT INTO audit (log) VALUES ('inserted'); END",
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW INSERT INTO audit (log) VALUES ('inserted');",
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW EXECUTE PROCEDURE foo();"
    ];
    for sql in sqls {
        println!("----");
        println!("SQL: {}", sql);
        match parse_sql(sql) {
            Ok(ast) => println!("Parsed OK! AST: {:?}", ast),
            Err(e) => println!("Parse Error: {:?}", e),
        }
    }
}
