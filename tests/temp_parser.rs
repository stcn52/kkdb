use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

fn main() {
    let sqls = [
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW INSERT INTO log (x) VALUES(1)",
        "CREATE TRIGGER t AFTER INSERT ON main FOR EACH ROW BEGIN INSERT INTO log (x) VALUES(1); END"
    ];
    for sql in sqls {
        match Parser::parse_sql(&SQLiteDialect{}, sql) {
            Ok(ast) => {
                if let sqlparser::ast::Statement::CreateTrigger(c) = &ast[0] {
                    println!("SQL: {}", sql);
                    if let Some(s) = &c.statements {
                        println!("body: {}", s);
                    }
                }
            }
            Err(e) => println!("Error: {}", e)
        }
    }
}
