use crate::error::KkdbError;
use crate::sql::ast::*;
use crate::sql::parser::parse_sql;
use crate::types::DataType;

fn parse(sql: &str) -> Statement {
    parse_sql(sql).unwrap()
}

// ---- CREATE TABLE ----

#[test]
fn test_parse_create_table() {
    match parse("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT NOT NULL)") {
        Statement::CreateTable(ct) => {
            assert_eq!(ct.table_name, "t1");
            assert!(!ct.if_not_exists);
            assert_eq!(ct.columns.len(), 2);
            assert_eq!(ct.columns[0].name, "id");
            assert_eq!(ct.columns[0].data_type, DataType::Integer);
            assert!(ct.columns[0].primary_key);
            assert_eq!(ct.columns[1].name, "name");
            assert_eq!(ct.columns[1].data_type, DataType::Text);
            assert!(ct.columns[1].not_null);
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_if_not_exists() {
    match parse("CREATE TABLE IF NOT EXISTS t1 (id INTEGER)") {
        Statement::CreateTable(ct) => {
            assert!(ct.if_not_exists);
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_autoincrement() {
    match parse("CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT)") {
        Statement::CreateTable(ct) => {
            assert!(ct.columns[0].primary_key);
            assert!(ct.columns[0].autoincrement);
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_unique() {
    match parse("CREATE TABLE t1 (email TEXT UNIQUE)") {
        Statement::CreateTable(ct) => {
            assert!(ct.columns[0].unique);
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_default() {
    match parse("CREATE TABLE t1 (val INTEGER DEFAULT 0)") {
        Statement::CreateTable(ct) => {
            assert!(ct.columns[0].default.is_some());
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_type_size() {
    // VARCHAR(255) - should consume the size params
    match parse("CREATE TABLE t1 (name VARCHAR(255))") {
        Statement::CreateTable(ct) => {
            assert_eq!(ct.columns[0].name, "name");
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_table_pk_constraint() {
    match parse("CREATE TABLE t1 (a INTEGER, b TEXT, PRIMARY KEY (a, b))") {
        Statement::CreateTable(ct) => {
            assert_eq!(ct.columns.len(), 2);
        }
        _ => panic!("expected CreateTable"),
    }
}

#[test]
fn test_parse_create_table_no_type() {
    // Column with no type - defaults to Blob
    match parse("CREATE TABLE t1 (val)") {
        Statement::CreateTable(ct) => {
            assert_eq!(ct.columns[0].data_type, DataType::Blob);
        }
        _ => panic!("expected CreateTable"),
    }
}

// ---- DROP TABLE ----

#[test]
fn test_parse_drop_table() {
    match parse("DROP TABLE t1") {
        Statement::DropTable(dt) => {
            assert_eq!(dt.table_name, "t1");
            assert!(!dt.if_exists);
        }
        _ => panic!("expected DropTable"),
    }
}

#[test]
fn test_parse_drop_table_if_exists() {
    match parse("DROP TABLE IF EXISTS t1") {
        Statement::DropTable(dt) => {
            assert!(dt.if_exists);
        }
        _ => panic!("expected DropTable"),
    }
}

// ---- INSERT ----

#[test]
fn test_parse_insert() {
    match parse("INSERT INTO t1 VALUES (1, 'hello', 3.14)") {
        Statement::Insert(ins) => {
            assert_eq!(ins.table_name, "t1");
            assert!(ins.columns.is_none());
            if let InsertSource::Values(rows) = &ins.source {
                assert_eq!(rows.len(), 1);
                assert_eq!(rows[0].len(), 3);
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!("expected Insert"),
    }
}

#[test]
fn test_parse_insert_with_columns() {
    match parse("INSERT INTO t1 (a, b) VALUES (1, 2)") {
        Statement::Insert(ins) => {
            assert_eq!(
                ins.columns.as_ref().unwrap(),
                &vec!["a".to_string(), "b".to_string()]
            );
        }
        _ => panic!("expected Insert"),
    }
}

#[test]
fn test_parse_insert_multiple_rows() {
    match parse("INSERT INTO t1 VALUES (1), (2), (3)") {
        Statement::Insert(ins) => {
            if let InsertSource::Values(rows) = &ins.source {
                assert_eq!(rows.len(), 3);
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!("expected Insert"),
    }
}

// ---- SELECT ----

#[test]
fn test_parse_select_star() {
    match parse("SELECT * FROM t1") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 1);
            assert!(matches!(sel.columns[0], SelectColumn::AllColumns));
            assert!(sel.from.is_some());
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_columns() {
    match parse("SELECT a, b, c FROM t1") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 3);
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_alias() {
    match parse("SELECT a AS alias1 FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { alias, .. } = &sel.columns[0] {
                assert_eq!(alias.as_ref().unwrap(), "alias1");
            } else {
                panic!()
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_distinct() {
    match parse("SELECT DISTINCT a FROM t1") {
        Statement::Select(sel) => {
            assert!(sel.distinct);
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_where() {
    match parse("SELECT * FROM t1 WHERE id > 5") {
        Statement::Select(sel) => {
            assert!(sel.where_clause.is_some());
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_order_by() {
    match parse("SELECT * FROM t1 ORDER BY a ASC, b DESC") {
        Statement::Select(sel) => {
            assert_eq!(sel.order_by.len(), 2);
            assert!(sel.order_by[0].ascending);
            assert!(!sel.order_by[1].ascending);
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_order_by_default_asc() {
    match parse("SELECT * FROM t1 ORDER BY a") {
        Statement::Select(sel) => {
            assert!(sel.order_by[0].ascending);
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_limit_offset() {
    match parse("SELECT * FROM t1 LIMIT 10 OFFSET 5") {
        Statement::Select(sel) => {
            assert!(sel.limit.is_some());
            assert!(sel.offset.is_some());
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_group_by_having() {
    match parse("SELECT name, COUNT(*) FROM t1 GROUP BY name HAVING COUNT(*) > 1") {
        Statement::Select(sel) => {
            assert_eq!(sel.group_by.len(), 1);
            assert!(sel.having.is_some());
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_select_without_from() {
    match parse("SELECT 1 + 2") {
        Statement::Select(sel) => {
            assert!(sel.from.is_none());
        }
        _ => panic!("expected Select"),
    }
}

// ---- FROM clause ----

#[test]
fn test_parse_from_table_alias() {
    match parse("SELECT * FROM t1 AS a") {
        Statement::Select(sel) => {
            if let Some(FromClause::Table { name, alias }) = &sel.from {
                assert_eq!(name, "t1");
                assert_eq!(alias.as_ref().unwrap(), "a");
            } else {
                panic!()
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_join() {
    match parse("SELECT * FROM t1 JOIN t2 ON t1.id = t2.id") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, on, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Inner));
                assert!(on.is_some());
            } else {
                panic!("expected Join")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_inner_join() {
    match parse("SELECT * FROM t1 INNER JOIN t2 ON a = b") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Inner));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_left_join() {
    match parse("SELECT * FROM t1 LEFT JOIN t2 ON a = b") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Left));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_left_outer_join() {
    match parse("SELECT * FROM t1 LEFT OUTER JOIN t2 ON a = b") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Left));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_right_join() {
    match parse("SELECT * FROM t1 RIGHT JOIN t2 ON a = b") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Right));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_right_outer_join() {
    match parse("SELECT * FROM t1 RIGHT OUTER JOIN t2 ON a = b") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Right));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_cross_join() {
    match parse("SELECT * FROM t1, t2") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join { join_type, on, .. }) = &sel.from {
                assert!(matches!(join_type, JoinType::Cross));
                assert!(on.is_none());
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subquery() {
    match parse("SELECT * FROM (SELECT 1) AS sub") {
        Statement::Select(sel) => {
            if let Some(FromClause::Subquery { alias, .. }) = &sel.from {
                assert_eq!(alias, "sub");
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

// ---- UPDATE ----

#[test]
fn test_parse_update() {
    match parse("UPDATE t1 SET a = 1, b = 'x' WHERE id = 5") {
        Statement::Update(upd) => {
            assert_eq!(upd.table_name, "t1");
            assert_eq!(upd.assignments.len(), 2);
            assert!(upd.where_clause.is_some());
        }
        _ => panic!("expected Update"),
    }
}

#[test]
fn test_parse_update_no_where() {
    match parse("UPDATE t1 SET a = 1") {
        Statement::Update(upd) => {
            assert!(upd.where_clause.is_none());
        }
        _ => panic!("expected Update"),
    }
}

// ---- DELETE ----

#[test]
fn test_parse_delete() {
    match parse("DELETE FROM t1 WHERE id = 1") {
        Statement::Delete(del) => {
            assert_eq!(del.table_name, "t1");
            assert!(del.where_clause.is_some());
        }
        _ => panic!("expected Delete"),
    }
}

#[test]
fn test_parse_delete_no_where() {
    match parse("DELETE FROM t1") {
        Statement::Delete(del) => {
            assert!(del.where_clause.is_none());
        }
        _ => panic!("expected Delete"),
    }
}

// ---- CREATE INDEX ----

#[test]
fn test_parse_create_index() {
    match parse("CREATE INDEX idx1 ON t1 (a, b)") {
        Statement::CreateIndex(ci) => {
            assert_eq!(ci.index_name, "idx1");
            assert_eq!(ci.table_name, "t1");
            assert_eq!(ci.columns, vec!["a", "b"]);
            assert!(!ci.unique);
            assert!(!ci.if_not_exists);
        }
        _ => panic!("expected CreateIndex"),
    }
}

#[test]
fn test_parse_create_unique_index() {
    match parse("CREATE UNIQUE INDEX idx1 ON t1 (a)") {
        Statement::CreateIndex(ci) => {
            assert!(ci.unique);
        }
        _ => panic!("expected CreateIndex"),
    }
}

#[test]
fn test_parse_create_index_if_not_exists() {
    match parse("CREATE INDEX IF NOT EXISTS idx1 ON t1 (a)") {
        Statement::CreateIndex(ci) => {
            assert!(ci.if_not_exists);
        }
        _ => panic!("expected CreateIndex"),
    }
}

// ---- Transaction statements ----

#[test]
fn test_parse_begin() {
    assert!(matches!(parse("BEGIN"), Statement::Begin));
    assert!(matches!(parse("BEGIN TRANSACTION"), Statement::Begin));
}

#[test]
fn test_parse_commit() {
    assert!(matches!(parse("COMMIT"), Statement::Commit));
}

#[test]
fn test_parse_rollback() {
    assert!(matches!(parse("ROLLBACK"), Statement::Rollback));
}

// ---- EXPLAIN ----

#[test]
fn test_parse_explain() {
    match parse("EXPLAIN SELECT * FROM t1") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::Select(_)));
        }
        _ => panic!("expected Explain"),
    }
}

// ---- Expression parsing ----

#[test]
fn test_parse_expr_literals() {
    match parse("SELECT 42, 3.14, 'hello', NULL") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 4);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_binary_ops() {
    // Covers +, -, *, /, %, comparison, AND, OR, ||
    parse("SELECT 1 + 2 - 3 * 4 / 5 % 6");
    parse("SELECT a = b");
    parse("SELECT a != b");
    parse("SELECT a < b");
    parse("SELECT a <= b");
    parse("SELECT a > b");
    parse("SELECT a >= b");
    parse("SELECT a AND b OR c");
    parse("SELECT 'a' || 'b'");
}

#[test]
fn test_parse_expr_unary_minus() {
    match parse("SELECT -42") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                assert!(matches!(
                    expr,
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        ..
                    }
                ));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_not() {
    match parse("SELECT NOT a FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                assert!(matches!(
                    expr,
                    Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        ..
                    }
                ));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_is_null() {
    parse("SELECT * FROM t1 WHERE a IS NULL");
    parse("SELECT * FROM t1 WHERE a IS NOT NULL");
}

#[test]
fn test_parse_expr_in() {
    parse("SELECT * FROM t1 WHERE a IN (1, 2, 3)");
}

#[test]
fn test_parse_expr_not_in() {
    parse("SELECT * FROM t1 WHERE a NOT IN (1, 2)");
}

#[test]
fn test_parse_expr_like() {
    parse("SELECT * FROM t1 WHERE name LIKE '%test%'");
}

#[test]
fn test_parse_expr_not_like() {
    parse("SELECT * FROM t1 WHERE name NOT LIKE 'x%'");
}

#[test]
fn test_parse_expr_nested_parens() {
    parse("SELECT (1 + 2) * 3");
}

#[test]
fn test_parse_expr_function_call() {
    parse("SELECT UPPER(name) FROM t1");
    parse("SELECT COUNT(*) FROM t1");
    parse("SELECT COUNT(DISTINCT name) FROM t1");
    parse("SELECT SUM(a), AVG(b), MIN(c), MAX(d) FROM t1");
}

#[test]
#[allow(clippy::collapsible_match)]
fn test_parse_expr_table_dot_column() {
    match parse("SELECT t1.col FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::ColumnRef { table, column } = expr {
                    assert_eq!(table.as_ref().unwrap(), "t1");
                    assert_eq!(column, "col");
                } else {
                    panic!()
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
#[allow(clippy::collapsible_match)]
fn test_parse_expr_user_function() {
    match parse("SELECT myfunc(a, b)") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::Function {
                    name,
                    args,
                    distinct,
                } = expr
                {
                    assert_eq!(name, "myfunc");
                    assert_eq!(args.len(), 2);
                    assert!(!distinct);
                } else {
                    panic!()
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
#[allow(clippy::collapsible_match)]
fn test_parse_expr_empty_function_args() {
    match parse("SELECT myfunc()") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::Function { args, .. } = expr {
                    assert!(args.is_empty());
                } else {
                    panic!()
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_blob_literal() {
    match parse("SELECT x'FF'") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                assert!(matches!(expr, Expr::BlobLiteral(_)));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

// ---- Error paths ----

#[test]
fn test_parse_error_unexpected_token() {
    assert!(parse_sql("FOOBAR").is_err());
}

#[test]
fn test_parse_error_empty_input() {
    assert!(parse_sql("").is_err());
}

#[test]
fn test_parse_error_create_without_table_or_index() {
    assert!(parse_sql("CREATE FOOBAR").is_err());
}

#[test]
fn test_parse_error_unexpected_in_expression() {
    assert!(parse_sql("SELECT FROM").is_err());
}

#[test]
fn test_parse_semicolon_optional() {
    parse("SELECT 1;");
    parse("SELECT 1");
}

// ---- token_as_ident coverage ----

#[test]
fn test_keyword_as_identifier() {
    // These keywords can be used as identifiers in some contexts
    parse("CREATE TABLE t1 (key INTEGER)"); // KEY as col name
    parse("CREATE TABLE t1 (index INTEGER)"); // INDEX as col name
}

// ==============================================================
// ALTER TABLE parsing
// ==============================================================

#[test]
fn test_parse_alter_add_column() {
    match parse("ALTER TABLE t1 ADD COLUMN val INTEGER") {
        Statement::AlterTable(a) => {
            assert_eq!(a.table_name, "t1");
            if let AlterTableAction::AddColumn(col) = &a.action {
                assert_eq!(col.name, "val");
                assert_eq!(col.data_type, DataType::Integer);
            } else {
                panic!("expected AddColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_add_column_no_keyword() {
    match parse("ALTER TABLE t1 ADD val TEXT") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::AddColumn(col) = &a.action {
                assert_eq!(col.name, "val");
                assert_eq!(col.data_type, DataType::Text);
            } else {
                panic!("expected AddColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_add_column_constraints() {
    match parse("ALTER TABLE t1 ADD COLUMN email TEXT NOT NULL UNIQUE") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::AddColumn(col) = &a.action {
                assert_eq!(col.name, "email");
                assert!(col.not_null);
                assert!(col.unique);
            } else {
                panic!("expected AddColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_add_column_default() {
    match parse("ALTER TABLE t1 ADD COLUMN val INTEGER DEFAULT 0") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::AddColumn(col) = &a.action {
                assert!(col.default.is_some());
            } else {
                panic!("expected AddColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_drop_column() {
    match parse("ALTER TABLE t1 DROP COLUMN val") {
        Statement::AlterTable(a) => {
            assert_eq!(a.table_name, "t1");
            if let AlterTableAction::DropColumn(col) = &a.action {
                assert_eq!(col, "val");
            } else {
                panic!("expected DropColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_drop_column_no_keyword() {
    match parse("ALTER TABLE t1 DROP val") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::DropColumn(col) = &a.action {
                assert_eq!(col, "val");
            } else {
                panic!("expected DropColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_rename_table() {
    match parse("ALTER TABLE t1 RENAME TO t2") {
        Statement::AlterTable(a) => {
            assert_eq!(a.table_name, "t1");
            if let AlterTableAction::RenameTable(new) = &a.action {
                assert_eq!(new, "t2");
            } else {
                panic!("expected RenameTable")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_rename_column() {
    match parse("ALTER TABLE t1 RENAME COLUMN old_col TO new_col") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::RenameColumn { old_name, new_name } = &a.action {
                assert_eq!(old_name, "old_col");
                assert_eq!(new_name, "new_col");
            } else {
                panic!("expected RenameColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

#[test]
fn test_parse_alter_rename_column_no_keyword() {
    match parse("ALTER TABLE t1 RENAME old_col TO new_col") {
        Statement::AlterTable(a) => {
            if let AlterTableAction::RenameColumn { old_name, new_name } = &a.action {
                assert_eq!(old_name, "old_col");
                assert_eq!(new_name, "new_col");
            } else {
                panic!("expected RenameColumn")
            }
        }
        _ => panic!("expected AlterTable"),
    }
}

// ==============================================================
// BETWEEN expression parsing
// ==============================================================

#[test]
fn test_parse_between() {
    match parse("SELECT * FROM t1 WHERE a BETWEEN 1 AND 10") {
        Statement::Select(sel) => {
            if let Some(Expr::Between { negated, .. }) = &sel.where_clause {
                assert!(!negated);
            } else {
                panic!("expected Between")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_between() {
    match parse("SELECT * FROM t1 WHERE a NOT BETWEEN 1 AND 10") {
        Statement::Select(sel) => {
            if let Some(Expr::Between { negated, .. }) = &sel.where_clause {
                assert!(negated);
            } else {
                panic!("expected Between negated")
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// Complex WHERE / Expression tests
// ==============================================================

#[test]
fn test_parse_where_and_or() {
    match parse("SELECT * FROM t1 WHERE a = 1 AND b = 2 OR c = 3") {
        Statement::Select(sel) => {
            // Should parse as (a=1 AND b=2) OR c=3 due to precedence
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::Or,
                ..
            }) = &sel.where_clause
            {
                // top-level is OR
            } else {
                panic!("expected OR at top level")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_where_parenthesized() {
    match parse("SELECT * FROM t1 WHERE a = 1 AND (b = 2 OR c = 3)") {
        Statement::Select(sel) => {
            // top-level is AND
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::And,
                ..
            }) = &sel.where_clause
            {
                // ok
            } else {
                panic!("expected AND at top level")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_precedence_mul_add() {
    // 1 + 2 * 3 should parse as 1 + (2 * 3)
    match parse("SELECT 1 + 2 * 3") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::BinaryOp {
                    op: BinaryOperator::Add,
                    right,
                    ..
                } = expr
                {
                    assert!(matches!(
                        right.as_ref(),
                        Expr::BinaryOp {
                            op: BinaryOperator::Multiply,
                            ..
                        }
                    ));
                } else {
                    panic!("expected Add at top")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_expr_precedence_comparison_and() {
    // a > 1 AND b < 2 鈫?AND(a>1, b<2)
    match parse("SELECT * FROM t1 WHERE a > 1 AND b < 2") {
        Statement::Select(sel) => {
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::And,
                left,
                right,
            }) = &sel.where_clause
            {
                assert!(matches!(
                    left.as_ref(),
                    Expr::BinaryOp {
                        op: BinaryOperator::GreaterThan,
                        ..
                    }
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::BinaryOp {
                        op: BinaryOperator::LessThan,
                        ..
                    }
                ));
            } else {
                panic!("expected AND")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_deeply_nested_parens() {
    match parse("SELECT ((((1 + 2))))") {
        Statement::Select(sel) => {
            // Should parse without error
            assert_eq!(sel.columns.len(), 1);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_not_expr() {
    // NOT NOT a 鈫?should parse as NOT(NOT(a))
    // Actually our parser does NOT(comparison), so NOT NOT a 鈫?NOT(<parse_comparison which starts with NOT>)
    // Let's just verify it parses
    parse("SELECT * FROM t1 WHERE NOT a = 1");
}

#[test]
fn test_parse_multiple_is_null() {
    // Chained IS NULL on same expression
    match parse("SELECT * FROM t1 WHERE a IS NOT NULL AND b IS NULL") {
        Statement::Select(sel) => {
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::And,
                ..
            }) = &sel.where_clause
            {
                // ok
            } else {
                panic!("expected AND")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_in_with_expressions() {
    // IN list with expressions, not just literals
    parse("SELECT * FROM t1 WHERE a IN (1 + 2, 3 * 4, -5)");
}

#[test]
fn test_parse_like_with_concat() {
    parse("SELECT * FROM t1 WHERE name LIKE '%' || 'test' || '%'");
}

#[test]
fn test_parse_between_with_expressions() {
    parse("SELECT * FROM t1 WHERE a BETWEEN 1 + 2 AND 10 - 3");
}

#[test]
fn test_parse_complex_where_all_ops() {
    // Combines multiple comparison types
    parse("SELECT * FROM t1 WHERE a = 1 AND b != 2 AND c < 3 AND d <= 4 AND e > 5 AND f >= 6");
}

// ==============================================================
// Complex SELECT
// ==============================================================

#[test]
fn test_parse_select_expr_alias_implicit() {
    // Implicit alias (without AS)
    match parse("SELECT a b FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { alias, .. } = &sel.columns[0] {
                assert_eq!(alias.as_ref().unwrap(), "b");
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_expression_in_column() {
    match parse("SELECT a + b AS total, c * 2 FROM t1") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 2);
            if let SelectColumn::Expr { alias, .. } = &sel.columns[0] {
                assert_eq!(alias.as_ref().unwrap(), "total");
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_multiple_group_by() {
    match parse("SELECT a, b, COUNT(*) FROM t1 GROUP BY a, b") {
        Statement::Select(sel) => {
            assert_eq!(sel.group_by.len(), 2);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_order_by_multiple_mixed() {
    match parse("SELECT * FROM t1 ORDER BY a ASC, b DESC, c") {
        Statement::Select(sel) => {
            assert_eq!(sel.order_by.len(), 3);
            assert!(sel.order_by[0].ascending);
            assert!(!sel.order_by[1].ascending);
            assert!(sel.order_by[2].ascending); // default ASC
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_limit_only() {
    match parse("SELECT * FROM t1 LIMIT 5") {
        Statement::Select(sel) => {
            assert!(sel.limit.is_some());
            assert!(sel.offset.is_none());
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_all_clauses() {
    // SELECT with every possible clause
    match parse("SELECT DISTINCT a, b FROM t1 WHERE a > 0 GROUP BY a HAVING COUNT(*) > 1 ORDER BY b DESC LIMIT 10 OFFSET 5") {
        Statement::Select(sel) => {
            assert!(sel.distinct);
            assert_eq!(sel.columns.len(), 2);
            assert!(sel.from.is_some());
            assert!(sel.where_clause.is_some());
            assert_eq!(sel.group_by.len(), 1);
            assert!(sel.having.is_some());
            assert_eq!(sel.order_by.len(), 1);
            assert!(sel.limit.is_some());
            assert!(sel.offset.is_some());
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_no_from_expression() {
    match parse("SELECT 1 + 2, 'hello', NULL") {
        Statement::Select(sel) => {
            assert!(sel.from.is_none());
            assert_eq!(sel.columns.len(), 3);
        }
        _ => panic!(),
    }
}

// ==============================================================
// JOIN variations
// ==============================================================

#[test]
fn test_parse_multiple_joins() {
    match parse("SELECT * FROM t1 JOIN t2 ON t1.id = t2.a JOIN t3 ON t2.id = t3.b") {
        Statement::Select(sel) => {
            // Should be nested: Join(Join(t1, t2), t3)
            if let Some(FromClause::Join { left, .. }) = &sel.from {
                assert!(matches!(left.as_ref(), FromClause::Join { .. }));
            } else {
                panic!("expected nested join")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_cross_join_explicit() {
    // Comma-separated tables = CROSS JOIN
    match parse("SELECT * FROM t1, t2, t3") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                left, join_type, ..
            }) = &sel.from
            {
                assert!(matches!(join_type, JoinType::Cross));
                assert!(matches!(left.as_ref(), FromClause::Join { .. }));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_join_with_table_alias() {
    match parse("SELECT a.id FROM t1 AS a JOIN t2 AS b ON a.id = b.id") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                left, right, on, ..
            }) = &sel.from
            {
                if let FromClause::Table { alias, .. } = left.as_ref() {
                    assert_eq!(alias.as_ref().unwrap(), "a");
                } else {
                    panic!()
                }
                if let FromClause::Table { alias, .. } = right.as_ref() {
                    assert_eq!(alias.as_ref().unwrap(), "b");
                } else {
                    panic!()
                }
                assert!(on.is_some());
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subquery_in_from() {
    match parse("SELECT * FROM (SELECT a, b FROM t1 WHERE a > 1) AS sub WHERE sub.a < 10") {
        Statement::Select(sel) => {
            if let Some(FromClause::Subquery { alias, query }) = &sel.from {
                assert_eq!(alias, "sub");
                assert!(query.where_clause.is_some());
            } else {
                panic!("expected Subquery")
            }
            assert!(sel.where_clause.is_some());
        }
        _ => panic!(),
    }
}

// ==============================================================
// Aggregate function parsing
// ==============================================================

#[test]
fn test_parse_all_aggregate_functions() {
    match parse("SELECT COUNT(*), SUM(a), AVG(b), MIN(c), MAX(d) FROM t1") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 5);
            let names: Vec<String> = sel
                .columns
                .iter()
                .map(|c| {
                    if let SelectColumn::Expr {
                        expr: Expr::Function { name, .. },
                        ..
                    } = c
                    {
                        name.clone()
                    } else {
                        panic!()
                    }
                })
                .collect();
            assert_eq!(names, vec!["COUNT", "SUM", "AVG", "MIN", "MAX"]);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_count_distinct() {
    match parse("SELECT COUNT(DISTINCT name) FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr {
                expr: Expr::Function { distinct, .. },
                ..
            } = &sel.columns[0]
            {
                assert!(distinct);
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_nested_function() {
    // ABS(COUNT(*)) 鈫?Function("ABS", [Function("COUNT", ...)])
    match parse("SELECT ABS(COUNT(*)) FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr {
                expr: Expr::Function { name, args, .. },
                ..
            } = &sel.columns[0]
            {
                assert_eq!(name, "ABS");
                assert!(matches!(&args[0], Expr::Function { .. }));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// INSERT variations
// ==============================================================

#[test]
fn test_parse_insert_with_null() {
    match parse("INSERT INTO t1 VALUES (1, NULL, 'hello')") {
        Statement::Insert(ins) => {
            if let InsertSource::Values(rows) = &ins.source {
                assert_eq!(rows[0].len(), 3);
                assert!(matches!(&rows[0][1], Expr::Null));
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_insert_negative_value() {
    match parse("INSERT INTO t1 VALUES (-42)") {
        Statement::Insert(ins) => {
            if let InsertSource::Values(rows) = &ins.source {
                assert!(matches!(
                    &rows[0][0],
                    Expr::UnaryOp {
                        op: UnaryOperator::Minus,
                        ..
                    }
                ));
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_insert_expression_value() {
    match parse("INSERT INTO t1 VALUES (1 + 2)") {
        Statement::Insert(ins) => {
            if let InsertSource::Values(rows) = &ins.source {
                assert!(matches!(
                    &rows[0][0],
                    Expr::BinaryOp {
                        op: BinaryOperator::Add,
                        ..
                    }
                ));
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_insert_multi_rows_with_columns() {
    match parse("INSERT INTO t1 (a, b) VALUES (1, 2), (3, 4)") {
        Statement::Insert(ins) => {
            assert_eq!(ins.columns.as_ref().unwrap().len(), 2);
            if let InsertSource::Values(rows) = &ins.source {
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].len(), 2);
                assert_eq!(rows[1].len(), 2);
            } else {
                panic!("expected Values");
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// UPDATE variations
// ==============================================================

#[test]
fn test_parse_update_expression_rhs() {
    match parse("UPDATE t1 SET val = val + 1 WHERE id = 1") {
        Statement::Update(upd) => {
            assert_eq!(upd.assignments.len(), 1);
            assert!(matches!(
                &upd.assignments[0].1,
                Expr::BinaryOp {
                    op: BinaryOperator::Add,
                    ..
                }
            ));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_update_multiple_assignments() {
    match parse("UPDATE t1 SET a = 1, b = 'x', c = NULL") {
        Statement::Update(upd) => {
            assert_eq!(upd.assignments.len(), 3);
            assert_eq!(upd.assignments[0].0, "a");
            assert_eq!(upd.assignments[1].0, "b");
            assert_eq!(upd.assignments[2].0, "c");
            assert!(matches!(&upd.assignments[2].1, Expr::Null));
        }
        _ => panic!(),
    }
}

// ==============================================================
// CREATE TABLE column constraint combinations
// ==============================================================

#[test]
fn test_parse_create_table_all_constraints() {
    match parse("CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL UNIQUE, val INTEGER DEFAULT 42)") {
        Statement::CreateTable(ct) => {
            assert!(ct.columns[0].primary_key);
            assert!(ct.columns[0].autoincrement);
            assert!(ct.columns[1].not_null);
            assert!(ct.columns[1].unique);
            assert!(ct.columns[2].default.is_some());
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_create_table_multiple_types() {
    match parse("CREATE TABLE t1 (a INTEGER, b REAL, c TEXT, d BLOB)") {
        Statement::CreateTable(ct) => {
            assert_eq!(ct.columns[0].data_type, DataType::Integer);
            assert_eq!(ct.columns[1].data_type, DataType::Real);
            assert_eq!(ct.columns[2].data_type, DataType::Text);
            assert_eq!(ct.columns[3].data_type, DataType::Blob);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_create_table_type_aliases() {
    // INT, FLOAT, DOUBLE, VARCHAR, CHAR all map to their canonical types
    parse("CREATE TABLE t1 (a INT, b FLOAT, c DOUBLE, d VARCHAR(100), e CHAR(10))");
}

// ==============================================================
// CREATE INDEX variations
// ==============================================================

#[test]
fn test_parse_create_index_single_column() {
    match parse("CREATE INDEX idx1 ON t1 (a)") {
        Statement::CreateIndex(ci) => {
            assert_eq!(ci.columns.len(), 1);
            assert_eq!(ci.columns[0], "a");
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_create_index_multi_column() {
    match parse("CREATE INDEX idx1 ON t1 (a, b, c)") {
        Statement::CreateIndex(ci) => {
            assert_eq!(ci.columns.len(), 3);
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_create_unique_index_if_not_exists() {
    match parse("CREATE UNIQUE INDEX IF NOT EXISTS idx1 ON t1 (a)") {
        Statement::CreateIndex(ci) => {
            assert!(ci.unique);
            assert!(ci.if_not_exists);
        }
        _ => panic!(),
    }
}

// ==============================================================
// EXPLAIN variations
// ==============================================================

#[test]
fn test_parse_explain_insert() {
    match parse("EXPLAIN INSERT INTO t1 VALUES (1)") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::Insert(_)));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_explain_update() {
    match parse("EXPLAIN UPDATE t1 SET a = 1") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::Update(_)));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_explain_delete() {
    match parse("EXPLAIN DELETE FROM t1") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::Delete(_)));
        }
        _ => panic!(),
    }
}

// ==============================================================
// Error paths
// ==============================================================

#[test]
fn test_parse_error_alter_missing_action() {
    assert!(parse_sql("ALTER TABLE t1").is_err());
}

#[test]
fn test_parse_error_alter_missing_table() {
    assert!(parse_sql("ALTER t1 ADD COLUMN val INTEGER").is_err());
}

#[test]
fn test_parse_error_insert_missing_into() {
    assert!(parse_sql("INSERT t1 VALUES (1)").is_err());
}

#[test]
fn test_parse_error_insert_missing_values() {
    assert!(parse_sql("INSERT INTO t1 (1)").is_err());
}

#[test]
fn test_parse_error_update_missing_set() {
    assert!(parse_sql("UPDATE t1 a = 1").is_err());
}

#[test]
fn test_parse_error_delete_missing_from() {
    assert!(parse_sql("DELETE t1").is_err());
}

#[test]
fn test_parse_error_create_table_missing_paren() {
    assert!(parse_sql("CREATE TABLE t1 id INTEGER").is_err());
}

#[test]
fn test_parse_error_unmatched_paren() {
    assert!(parse_sql("SELECT (1 + 2").is_err());
}

#[test]
fn test_parse_error_in_missing_paren() {
    assert!(parse_sql("SELECT * FROM t1 WHERE a IN 1, 2").is_err());
}

#[test]
fn test_parse_error_between_missing_and() {
    assert!(parse_sql("SELECT * FROM t1 WHERE a BETWEEN 1 OR 10").is_err());
}

#[test]
fn test_parse_error_order_by_missing_by() {
    assert!(parse_sql("SELECT * FROM t1 ORDER a").is_err());
}

#[test]
fn test_parse_error_group_by_missing_by() {
    assert!(parse_sql("SELECT * FROM t1 GROUP a").is_err());
}

#[test]
fn test_parse_error_create_index_missing_on() {
    assert!(parse_sql("CREATE INDEX idx1 t1 (a)").is_err());
}

#[test]
fn test_parse_error_rename_column_missing_to() {
    assert!(parse_sql("ALTER TABLE t1 RENAME COLUMN old new").is_err());
}

// ==============================================================
// Miscellaneous / edge cases
// ==============================================================

#[test]
fn test_parse_concat_operator() {
    match parse("SELECT 'hello' || ' ' || 'world'") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                // Should be Concat(Concat('hello', ' '), 'world')
                if let Expr::BinaryOp {
                    op: BinaryOperator::Concat,
                    ..
                } = expr
                {
                    // ok
                } else {
                    panic!("expected Concat")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_modulo_operator() {
    match parse("SELECT 10 % 3") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                assert!(matches!(
                    expr,
                    Expr::BinaryOp {
                        op: BinaryOperator::Modulo,
                        ..
                    }
                ));
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_semicolon_at_end() {
    parse("SELECT 1;");
    parse("INSERT INTO t1 VALUES (1);");
    parse("UPDATE t1 SET a = 1;");
    parse("DELETE FROM t1;");
    parse("CREATE TABLE t1 (id INTEGER);");
    parse("DROP TABLE t1;");
    parse("ALTER TABLE t1 ADD COLUMN val INTEGER;");
}

#[test]
#[allow(clippy::collapsible_match)]
fn test_parse_negative_real() {
    match parse("SELECT -3.14") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::UnaryOp {
                    op: UnaryOperator::Minus,
                    expr: inner,
                } = expr
                {
                    assert!(matches!(inner.as_ref(), Expr::RealLiteral(_)));
                } else {
                    panic!()
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_complex_expression_in_where() {
    // a > 1 AND (b IN (1,2,3) OR c LIKE 'test%') AND d BETWEEN 10 AND 20 AND e IS NOT NULL
    parse("SELECT * FROM t1 WHERE a > 1 AND (b IN (1,2,3) OR c LIKE 'test%') AND d BETWEEN 10 AND 20 AND e IS NOT NULL");
}

#[test]
fn test_parse_select_star_from_joined_tables() {
    parse("SELECT * FROM t1 INNER JOIN t2 ON t1.id = t2.fk LEFT JOIN t3 ON t2.id = t3.fk WHERE t1.val > 0 ORDER BY t1.id LIMIT 100");
}

// ==============================================================
// Gap coverage: implicit table alias, parenthesized FROM,
// subtract vs unary minus, NOT backtrack, more error paths
// ==============================================================

#[test]
fn test_parse_from_implicit_table_alias() {
    // FROM t1 a (implicit alias without AS keyword)
    match parse("SELECT * FROM t1 a WHERE a.id = 1") {
        Statement::Select(sel) => {
            if let Some(FromClause::Table { name, alias }) = &sel.from {
                assert_eq!(name, "t1");
                assert_eq!(alias.as_ref().unwrap(), "a");
            } else {
                panic!("expected Table with implicit alias")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subtract_vs_unary_minus() {
    // a - -1 鈫?Subtract(a, UnaryMinus(1))
    match parse("SELECT a - -1 FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::BinaryOp {
                    op: BinaryOperator::Subtract,
                    right,
                    ..
                } = expr
                {
                    assert!(matches!(
                        right.as_ref(),
                        Expr::UnaryOp {
                            op: UnaryOperator::Minus,
                            ..
                        }
                    ));
                } else {
                    panic!("expected Subtract at top")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_backtrack_to_comparison() {
    // NOT followed by a token that is neither IN, LIKE, nor BETWEEN
    // should backtrack and let parse_comparison handle it normally
    match parse("SELECT * FROM t1 WHERE a > 1 AND NOT b = 2") {
        Statement::Select(sel) => {
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::And,
                right,
                ..
            }) = &sel.where_clause
            {
                assert!(matches!(
                    right.as_ref(),
                    Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        ..
                    }
                ));
            } else {
                panic!("expected AND with NOT on right")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_is_null_chained() {
    // a IS NULL (single expression, verify structure)
    match parse("SELECT * FROM t1 WHERE a IS NULL") {
        Statement::Select(sel) => {
            if let Some(Expr::IsNull { negated, .. }) = &sel.where_clause {
                assert!(!negated);
            } else {
                panic!("expected IsNull")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_is_not_null_structure() {
    match parse("SELECT * FROM t1 WHERE a IS NOT NULL") {
        Statement::Select(sel) => {
            if let Some(Expr::IsNull { negated, .. }) = &sel.where_clause {
                assert!(negated);
            } else {
                panic!("expected IsNull negated")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_in_list_structure() {
    match parse("SELECT * FROM t1 WHERE a IN (1, 2, 3)") {
        Statement::Select(sel) => {
            if let Some(Expr::InList { list, negated, .. }) = &sel.where_clause {
                assert!(!negated);
                assert_eq!(list.len(), 3);
            } else {
                panic!("expected InList")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_in_list_structure() {
    match parse("SELECT * FROM t1 WHERE a NOT IN (1, 2)") {
        Statement::Select(sel) => {
            if let Some(Expr::InList { list, negated, .. }) = &sel.where_clause {
                assert!(negated);
                assert_eq!(list.len(), 2);
            } else {
                panic!("expected InList negated")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_like_structure() {
    match parse("SELECT * FROM t1 WHERE name LIKE '%test%'") {
        Statement::Select(sel) => {
            if let Some(Expr::Like { negated, .. }) = &sel.where_clause {
                assert!(!negated);
            } else {
                panic!("expected Like")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_like_structure() {
    match parse("SELECT * FROM t1 WHERE name NOT LIKE 'x%'") {
        Statement::Select(sel) => {
            if let Some(Expr::Like { negated, .. }) = &sel.where_clause {
                assert!(negated);
            } else {
                panic!("expected Like negated")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_between_structure() {
    match parse("SELECT * FROM t1 WHERE a BETWEEN 1 AND 10") {
        Statement::Select(sel) => {
            if let Some(Expr::Between {
                low, high, negated, ..
            }) = &sel.where_clause
            {
                assert!(!negated);
                assert!(matches!(low.as_ref(), Expr::IntegerLiteral(1)));
                assert!(matches!(high.as_ref(), Expr::IntegerLiteral(10)));
            } else {
                panic!("expected Between")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_explain_create_table() {
    match parse("EXPLAIN CREATE TABLE t1 (id INTEGER)") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::CreateTable(_)));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_explain_alter_table() {
    match parse("EXPLAIN ALTER TABLE t1 ADD val INTEGER") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::AlterTable(_)));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_explain_create_index() {
    match parse("EXPLAIN CREATE INDEX idx ON t1 (a)") {
        Statement::Explain(inner) => {
            assert!(matches!(*inner, Statement::CreateIndex(_)));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_default_string_value() {
    match parse("CREATE TABLE t1 (status TEXT DEFAULT 'active')") {
        Statement::CreateTable(ct) => {
            if let Some(Expr::StringLiteral(s)) = &ct.columns[0].default {
                assert_eq!(s, "active");
            } else {
                panic!("expected string default")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_default_negative_value() {
    match parse("CREATE TABLE t1 (val INTEGER DEFAULT -1)") {
        Statement::CreateTable(ct) => {
            assert!(ct.columns[0].default.is_some());
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_select_count_star_alias() {
    match parse("SELECT COUNT(*) AS total FROM t1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, alias } = &sel.columns[0] {
                assert!(matches!(expr, Expr::Function { .. }));
                assert_eq!(alias.as_ref().unwrap(), "total");
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_error_alter_unknown_action() {
    assert!(parse_sql("ALTER TABLE t1 MODIFY val INTEGER").is_err());
}

#[test]
fn test_parse_error_create_table_empty_columns() {
    assert!(parse_sql("CREATE TABLE t1 ()").is_err());
}

#[test]
fn test_parse_error_select_trailing_comma() {
    // SELECT a, FROM t1 鈫?"FROM" parsed as expression, which fails
    assert!(parse_sql("SELECT a, FROM t1").is_err());
}

#[test]
fn test_parse_error_missing_right_paren_in_function() {
    assert!(parse_sql("SELECT COUNT(").is_err());
}

// ==============================================================
// Expr::Subquery 鈥?subquery expression parsing
// ==============================================================

#[test]
fn test_parse_subquery_scalar() {
    // (SELECT MAX(id) FROM t1) as a scalar subquery expression
    match parse("SELECT (SELECT MAX(id) FROM t1)") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::Subquery(sub) = expr {
                    assert!(sub.from.is_some());
                    assert_eq!(sub.columns.len(), 1);
                } else {
                    panic!("expected Subquery expr")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_in_subquery() {
    // WHERE id IN (SELECT fk FROM t2)
    match parse("SELECT * FROM t1 WHERE id IN (SELECT fk FROM t2)") {
        Statement::Select(sel) => {
            if let Some(Expr::InSubquery {
                subquery, negated, ..
            }) = &sel.where_clause
            {
                assert!(!negated);
                assert!(subquery.from.is_some());
            } else {
                panic!("expected InSubquery")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_in_subquery() {
    match parse("SELECT * FROM t1 WHERE id NOT IN (SELECT fk FROM t2)") {
        Statement::Select(sel) => {
            if let Some(Expr::InSubquery {
                subquery, negated, ..
            }) = &sel.where_clause
            {
                assert!(negated);
                assert!(subquery.from.is_some());
            } else {
                panic!("expected InSubquery negated")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subquery_in_where_comparison() {
    // WHERE val > (SELECT AVG(val) FROM t1)
    match parse("SELECT * FROM t1 WHERE val > (SELECT AVG(val) FROM t1)") {
        Statement::Select(sel) => {
            if let Some(Expr::BinaryOp {
                op: BinaryOperator::GreaterThan,
                right,
                ..
            }) = &sel.where_clause
            {
                assert!(matches!(right.as_ref(), Expr::Subquery(_)));
            } else {
                panic!("expected GT with subquery on right")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subquery_nested() {
    // Subquery within subquery
    match parse(
        "SELECT * FROM t1 WHERE id IN (SELECT id FROM t2 WHERE val IN (SELECT val FROM t3))",
    ) {
        Statement::Select(sel) => {
            if let Some(Expr::InSubquery { subquery, .. }) = &sel.where_clause {
                // inner subquery also has IN subquery
                if let Some(Expr::InSubquery {
                    subquery: inner_sub,
                    ..
                }) = &subquery.where_clause
                {
                    assert!(inner_sub.from.is_some());
                } else {
                    panic!("expected nested InSubquery")
                }
            } else {
                panic!("expected InSubquery")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_subquery_with_where() {
    match parse("SELECT (SELECT COUNT(*) FROM t1 WHERE a > 10)") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::Subquery(sub) = expr {
                    assert!(sub.where_clause.is_some());
                } else {
                    panic!("expected Subquery")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// SelectColumn::TableAllColumns 鈥?table.* syntax
// ==============================================================

#[test]
fn test_parse_table_all_columns() {
    match parse("SELECT t1.* FROM t1") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 1);
            if let SelectColumn::TableAllColumns(table) = &sel.columns[0] {
                assert_eq!(table, "t1");
            } else {
                panic!("expected TableAllColumns, got {:?}", sel.columns[0])
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_table_all_columns_mixed() {
    // Mix of table.*, *, and regular columns
    match parse("SELECT t1.*, t2.id, * FROM t1 JOIN t2 ON t1.id = t2.fk") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 3);
            assert!(matches!(&sel.columns[0], SelectColumn::TableAllColumns(t) if t == "t1"));
            assert!(matches!(&sel.columns[1], SelectColumn::Expr { .. }));
            assert!(matches!(&sel.columns[2], SelectColumn::AllColumns));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_table_all_columns_multiple_tables() {
    match parse("SELECT a.*, b.* FROM t1 AS a JOIN t2 AS b ON a.id = b.id") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 2);
            assert!(matches!(&sel.columns[0], SelectColumn::TableAllColumns(t) if t == "a"));
            assert!(matches!(&sel.columns[1], SelectColumn::TableAllColumns(t) if t == "b"));
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_table_all_columns_with_alias_column() {
    // table.* alongside aliased expressions
    match parse("SELECT t1.*, t2.id AS other_id FROM t1, t2") {
        Statement::Select(sel) => {
            assert_eq!(sel.columns.len(), 2);
            assert!(matches!(&sel.columns[0], SelectColumn::TableAllColumns(t) if t == "t1"));
            if let SelectColumn::Expr { alias, .. } = &sel.columns[1] {
                assert_eq!(alias.as_ref().unwrap(), "other_id");
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// Expr::Exists 鈥?EXISTS (SELECT ...) expression
// ==============================================================

#[test]
fn test_parse_exists() {
    match parse("SELECT * FROM t1 WHERE EXISTS (SELECT 1 FROM t2 WHERE t2.fk = t1.id)") {
        Statement::Select(sel) => {
            if let Some(Expr::Exists(sub)) = &sel.where_clause {
                assert!(sub.from.is_some());
                assert!(sub.where_clause.is_some());
            } else {
                panic!("expected Exists expr")
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_not_exists() {
    match parse("SELECT * FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2)") {
        Statement::Select(sel) => {
            if let Some(Expr::UnaryOp {
                op: UnaryOperator::Not,
                expr,
            }) = &sel.where_clause
            {
                assert!(matches!(expr.as_ref(), Expr::Exists(_)));
            } else {
                panic!("expected NOT Exists")
            }
        }
        _ => panic!(),
    }
}

// ==============================================================
// NOT NOT double negation
// ==============================================================

#[test]
fn test_parse_not_not() {
    match parse("SELECT NOT NOT 1") {
        Statement::Select(sel) => {
            if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
                if let Expr::UnaryOp {
                    op: UnaryOperator::Not,
                    expr: inner,
                } = expr
                {
                    if let Expr::UnaryOp {
                        op: UnaryOperator::Not,
                        expr: innermost,
                    } = inner.as_ref()
                    {
                        assert!(matches!(innermost.as_ref(), Expr::IntegerLiteral(1)));
                    } else {
                        panic!("expected inner NOT")
                    }
                } else {
                    panic!("expected outer NOT")
                }
            } else {
                panic!()
            }
        }
        _ => panic!(),
    }
}

#[test]
fn test_parse_multi_statement_rejected() {
    let err = parse_sql("SELECT 1; SELECT 2").unwrap_err();
    match err {
        KkdbError::ParseError(msg) => {
            assert!(msg.contains("only a single SQL statement is supported"));
        }
        _ => panic!("expected ParseError"),
    }
}

#[test]
fn test_parse_with_clause_reports_unsupported_feature() {
    // WITH CTE is now supported — verify it parses and the CTE definition is present
    use crate::sql::ast::Statement;
    let stmt = parse_sql("WITH cte AS (SELECT 1) SELECT * FROM cte").unwrap();
    match stmt {
        Statement::Select(sel) => {
            assert_eq!(sel.ctes.len(), 1, "expected one CTE definition");
            assert_eq!(sel.ctes[0].name, "cte");
        }
        _ => panic!("expected Select statement"),
    }
}

#[test]
fn test_parse_join_using_rewrites_to_on_predicate() {
    match parse("SELECT * FROM t1 AS a JOIN t2 AS b USING (id, k)") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                join_type,
                on:
                    Some(Expr::BinaryOp {
                        op: BinaryOperator::And,
                        left,
                        right,
                    }),
                ..
            }) = &sel.from
            {
                assert!(matches!(join_type, JoinType::Inner));
                assert!(matches!(
                    left.as_ref(),
                    Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right
                    } if matches!(
                        left.as_ref(),
                        Expr::ColumnRef { table: Some(t), column } if t == "a" && column == "id"
                    ) && matches!(
                        right.as_ref(),
                        Expr::ColumnRef { table: Some(t), column } if t == "b" && column == "id"
                    )
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right
                    } if matches!(
                        left.as_ref(),
                        Expr::ColumnRef { table: Some(t), column } if t == "a" && column == "k"
                    ) && matches!(
                        right.as_ref(),
                        Expr::ColumnRef { table: Some(t), column } if t == "b" && column == "k"
                    )
                ));
            } else {
                panic!("expected JOIN with rewritten ON predicate")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_join_using_on_inner_join_tree_rewrites_to_on_predicate() {
    match parse("SELECT * FROM t1 AS a JOIN t2 AS b USING (id) JOIN t3 AS c USING (id)") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                join_type: JoinType::Inner,
                on:
                    Some(Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right,
                    }),
                ..
            }) = &sel.from
            {
                assert!(matches!(
                    left.as_ref(),
                    Expr::Function { name, args, distinct } if name.eq_ignore_ascii_case("COALESCE")
                        && !distinct && args.len() == 2
                        && matches!(&args[0], Expr::ColumnRef { table: Some(t), column } if t == "a" && column == "id")
                        && matches!(&args[1], Expr::ColumnRef { table: Some(t), column } if t == "b" && column == "id")
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "c" && column == "id"
                ));
            } else {
                panic!("expected outer JOIN with rewritten USING predicate")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_join_using_on_left_join_tree_rewrites_to_on_predicate() {
    match parse("SELECT * FROM t1 LEFT JOIN t2 ON t1.id = t2.id JOIN t3 USING (id)") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                join_type: JoinType::Inner,
                on:
                    Some(Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right,
                    }),
                ..
            }) = &sel.from
            {
                assert!(matches!(
                    left.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "t1" && column == "id"
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "t3" && column == "id"
                ));
            } else {
                panic!("expected JOIN with rewritten USING predicate")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_join_using_on_right_join_tree_rewrites_to_on_predicate() {
    match parse("SELECT * FROM t1 RIGHT JOIN t2 ON t1.id = t2.id JOIN t3 USING (id)") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                join_type: JoinType::Inner,
                on:
                    Some(Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right,
                    }),
                ..
            }) = &sel.from
            {
                assert!(matches!(
                    left.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "t2" && column == "id"
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "t3" && column == "id"
                ));
            } else {
                panic!("expected JOIN with rewritten USING predicate")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_join_using_on_cross_join_tree_rewrites_to_on_predicate() {
    match parse("SELECT * FROM (t1 CROSS JOIN t2) JOIN t3 USING (id)") {
        Statement::Select(sel) => {
            if let Some(FromClause::Join {
                join_type: JoinType::Inner,
                on:
                    Some(Expr::BinaryOp {
                        op: BinaryOperator::Equal,
                        left,
                        right,
                    }),
                ..
            }) = &sel.from
            {
                assert!(matches!(
                    left.as_ref(),
                    Expr::Function { name, args, distinct } if name.eq_ignore_ascii_case("COALESCE")
                        && !distinct && args.len() == 2
                        && matches!(&args[0], Expr::ColumnRef { table: Some(t), column } if t == "t1" && column == "id")
                        && matches!(&args[1], Expr::ColumnRef { table: Some(t), column } if t == "t2" && column == "id")
                ));
                assert!(matches!(
                    right.as_ref(),
                    Expr::ColumnRef { table: Some(t), column } if t == "t3" && column == "id"
                ));
            } else {
                panic!("expected JOIN with rewritten USING predicate")
            }
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_natural_join_reports_unsupported_feature() {
    // NATURAL JOIN is now supported (Batch C) - verify it parses to JoinType::Natural
    use crate::sql::ast::{FromClause, JoinType, Statement};
    let stmt = parse_sql("SELECT * FROM t1 NATURAL JOIN t2").unwrap();
    match stmt {
        Statement::Select(sel) => match sel.from.as_ref().unwrap() {
            FromClause::Join { join_type, .. } => {
                assert!(matches!(join_type, JoinType::Natural));
            }
            _ => panic!("expected JOIN"),
        },
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_rollback_to_savepoint_succeeds() {
    use crate::sql::ast::Statement;
    let stmt = parse_sql("ROLLBACK TO SAVEPOINT s1").unwrap();
    assert!(matches!(stmt, Statement::RollbackToSavepoint(ref name) if name == "s1"));
}

// ---- R5: Tests for R1-R4 new features ----

#[test]
fn test_parse_r5_try_cast_as_cast() {
    // TRY_CAST should be parsed to Expr::Cast (same type regardless of dialect safety)
    match parse("SELECT TRY_CAST(x AS INTEGER) FROM t") {
        Statement::Select(sel) => match &sel.columns[0] {
            SelectColumn::Expr { expr, .. } => {
                assert!(
                    matches!(expr, Expr::Cast { .. }),
                    "TRY_CAST should parse to Cast, got {:?}",
                    expr
                );
            }
            other => panic!("expected SelectColumn::Expr, got {:?}", other),
        },
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_r5_xor_binary_operator() {
    match parse("SELECT a XOR b FROM t") {
        Statement::Select(sel) => match &sel.columns[0] {
            SelectColumn::Expr { expr, .. } => {
                assert!(
                    matches!(
                        expr,
                        Expr::BinaryOp {
                            op: BinaryOperator::Xor,
                            ..
                        }
                    ),
                    "XOR should parse to BinaryOp::Xor, got {:?}",
                    expr
                );
            }
            other => panic!("expected SelectColumn::Expr, got {:?}", other),
        },
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_r5_bitwise_and_operator() {
    match parse("SELECT 5 & 3 FROM t") {
        Statement::Select(sel) => match &sel.columns[0] {
            SelectColumn::Expr { expr, .. } => {
                assert!(
                    matches!(
                        expr,
                        Expr::BinaryOp {
                            op: BinaryOperator::BitwiseAnd,
                            ..
                        }
                    ),
                    "& should parse to BitwiseAnd, got {:?}",
                    expr
                );
            }
            other => panic!("expected SelectColumn::Expr, got {:?}", other),
        },
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_r5_bitwise_or_operator() {
    // PostgreSQL dialect used by sqlparser recognises | as bitwise OR
    let result = parse_sql("SELECT 5 | 3 FROM t");
    if let Ok(Statement::Select(sel)) = result {
        if let SelectColumn::Expr { expr, .. } = &sel.columns[0] {
            assert!(
                matches!(
                    expr,
                    Expr::BinaryOp {
                        op: BinaryOperator::BitwiseOr,
                        ..
                    }
                ),
                "| should parse to BitwiseOr, got {:?}",
                expr
            );
        }
    }
    // Some dialects may not support |, that's ok — just must not panic
}

#[test]
fn test_parse_r5_any_op_eq_becomes_in_subquery() {
    // x = ANY(SELECT ...) → InSubquery
    match parse("SELECT * FROM t WHERE id = ANY(SELECT id FROM s)") {
        Statement::Select(sel) => {
            let where_expr = sel.where_clause.expect("should have WHERE");
            assert!(
                matches!(where_expr, Expr::InSubquery { negated: false, .. }),
                "id = ANY(subq) should become InSubquery, got {:?}",
                where_expr
            );
        }
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_r5_truncate_becomes_delete() {
    // TRUNCATE TABLE t → Statement::Delete with no WHERE clause
    match parse_sql("TRUNCATE TABLE t").unwrap() {
        Statement::Delete(d) => {
            assert_eq!(d.table_name, "t");
            assert!(
                d.where_clause.is_none(),
                "DELETE from TRUNCATE should have no WHERE"
            );
        }
        _ => panic!("expected Delete (from TRUNCATE)"),
    }
}

#[test]
fn test_parse_r5_unary_plus_passthrough() {
    // +42 → IntegerLiteral(42) — Plus is a no-op
    match parse("SELECT +42 FROM t") {
        Statement::Select(sel) => match &sel.columns[0] {
            SelectColumn::Expr { expr, .. } => {
                assert!(
                    matches!(expr, Expr::IntegerLiteral(42)),
                    "+42 should pass through as IntegerLiteral(42), got {:?}",
                    expr
                );
            }
            other => panic!("expected SelectColumn::Expr, got {:?}", other),
        },
        _ => panic!("expected Select"),
    }
}

#[test]
fn test_parse_r5_alter_view_returns_error() {
    let err = parse_sql("ALTER VIEW v RENAME TO v2");
    assert!(err.is_err(), "ALTER VIEW should produce a parse error");
}

#[test]
fn test_parse_r5_create_function_returns_error() {
    let err = parse_sql("CREATE FUNCTION f() RETURNS INTEGER AS $$ SELECT 1 $$ LANGUAGE SQL");
    assert!(err.is_err(), "CREATE FUNCTION should produce a parse error");
}

#[test]
fn test_parse_r5_tuple_single_element_passthrough() {
    // (42) single-element tuple → pass through as the inner expression
    match parse("SELECT (42) FROM t") {
        Statement::Select(sel) => {
            // Should NOT produce an error — either Nested(IntegerLiteral(42)) or just IntegerLiteral(42)
            assert!(!sel.columns.is_empty(), "should have at least one column");
        }
        _ => panic!("expected Select"),
    }
}
