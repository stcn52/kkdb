use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};

fn assert_query_result(
    vm: &mut VM,
    sql: &str,
    expected_cols: &[&str],
    expected_rows: Vec<Vec<Value>>,
) {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { columns, rows } => {
            assert_eq!(
                columns,
                expected_cols
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>(),
                "Column mismatch for: {}",
                sql
            );
            assert_eq!(
                rows.len(),
                expected_rows.len(),
                "Row count mismatch for: {}",
                sql
            );
            for (i, (actual, expected)) in rows.iter().zip(expected_rows.iter()).enumerate() {
                assert_eq!(
                    actual.len(),
                    expected.len(),
                    "Row {} column count mismatch for: {}",
                    i,
                    sql
                );
                for (j, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
                    assert_eq!(
                        format!("{}", a),
                        format!("{}", e),
                        "Row {} col {} mismatch for: {}",
                        i,
                        j,
                        sql
                    );
                }
            }
        }
        other => panic!("Expected QueryResult for '{}', got {:?}", sql, other),
    }
}

fn assert_rows_affected(vm: &mut VM, sql: &str, expected_count: usize) {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::RowsAffected { count, .. } => {
            assert_eq!(count, expected_count, "Rows affected mismatch for: {}", sql);
        }
        other => panic!("Expected RowsAffected for '{}', got {:?}", sql, other),
    }
}

fn assert_ok(vm: &mut VM, sql: &str) {
    match vm.execute_sql(sql) {
        Ok(ExecResult::Ok { .. }) => {}
        Ok(other) => panic!("Expected Ok for '{}', got {:?}", sql, other),
        Err(e) => panic!("Error for '{}': {}", sql, e),
    }
}

// ---- Basic Tests ----

#[test]
fn test_create_table() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);",
    );
    assert!(vm.schema.tables.contains_key("users"));
    assert_eq!(vm.schema.tables["users"].columns.len(), 3);
}

#[test]
fn test_create_table_if_not_exists() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER);");
    // Should not error
    assert_ok(&mut vm, "CREATE TABLE IF NOT EXISTS t1 (id INTEGER);");
}

#[test]
fn test_create_table_already_exists() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER);");
    assert!(vm.execute_sql("CREATE TABLE t1 (id INTEGER);").is_err());
}

#[test]
fn test_drop_table() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER);");
    assert_ok(&mut vm, "DROP TABLE t1;");
    assert!(!vm.schema.tables.contains_key("t1"));
}

#[test]
fn test_drop_table_if_exists() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "DROP TABLE IF EXISTS nonexistent;");
}

// ---- INSERT & SELECT ----

#[test]
fn test_insert_and_select() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, age INTEGER);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO users VALUES (1, 'Alice', 30);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO users VALUES (2, 'Bob', 25);", 1);

    assert_query_result(
        &mut vm,
        "SELECT * FROM users;",
        &["id", "name", "age"],
        vec![
            vec![
                Value::Integer(1),
                Value::Text("Alice".into()),
                Value::Integer(30),
            ],
            vec![
                Value::Integer(2),
                Value::Text("Bob".into()),
                Value::Integer(25),
            ],
        ],
    );
}

#[test]
fn test_insert_with_columns() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (a INTEGER, b TEXT, c REAL);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 (b, a) VALUES ('hello', 42);", 1);

    assert_query_result(
        &mut vm,
        "SELECT a, b, c FROM t1;",
        &["a", "b", "c"],
        vec![vec![
            Value::Integer(42),
            Value::Text("hello".into()),
            Value::Null,
        ]],
    );
}

#[test]
fn test_insert_multiple_rows() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT);",
    );
    assert_rows_affected(
        &mut vm,
        "INSERT INTO t1 VALUES (1, 'a'), (2, 'b'), (3, 'c');",
        3,
    );

    match vm.execute_sql("SELECT * FROM t1;").unwrap() {
        ExecResult::QueryResult { rows, .. } => assert_eq!(rows.len(), 3),
        _ => panic!("Expected query result"),
    }
}

// ---- WHERE Clause ----

#[test]
fn test_select_where() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, score INTEGER);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'Alice', 90);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'Bob', 75);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (3, 'Carol', 85);", 1);

    assert_query_result(
        &mut vm,
        "SELECT name FROM t1 WHERE score > 80;",
        &["name"],
        vec![
            vec![Value::Text("Alice".into())],
            vec![Value::Text("Carol".into())],
        ],
    );
}

#[test]
fn test_where_and_or() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (a INTEGER, b INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 10);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 20);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (3, 30);", 1);

    assert_query_result(
        &mut vm,
        "SELECT a FROM t1 WHERE a = 1 OR b = 30;",
        &["a"],
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]],
    );
}

#[test]
fn test_where_like() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (name TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('Alice');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('Bob');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('Alex');", 1);

    assert_query_result(
        &mut vm,
        "SELECT name FROM t1 WHERE name LIKE 'Al%';",
        &["name"],
        vec![
            vec![Value::Text("Alice".into())],
            vec![Value::Text("Alex".into())],
        ],
    );
}

#[test]
fn test_where_in() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER, name TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'a');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'b');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (3, 'c');", 1);

    assert_query_result(
        &mut vm,
        "SELECT name FROM t1 WHERE id IN (1, 3);",
        &["name"],
        vec![vec![Value::Text("a".into())], vec![Value::Text("c".into())]],
    );
}

#[test]
fn test_where_is_null() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (a INTEGER, b TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 (a) VALUES (1);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'hello');", 1);

    assert_query_result(
        &mut vm,
        "SELECT a FROM t1 WHERE b IS NULL;",
        &["a"],
        vec![vec![Value::Integer(1)]],
    );

    assert_query_result(
        &mut vm,
        "SELECT a FROM t1 WHERE b IS NOT NULL;",
        &["a"],
        vec![vec![Value::Integer(2)]],
    );
}

// ---- UPDATE ----

#[test]
fn test_update() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'old');", 1);

    assert_rows_affected(&mut vm, "UPDATE t1 SET val = 'new' WHERE id = 1;", 1);

    assert_query_result(
        &mut vm,
        "SELECT val FROM t1 WHERE id = 1;",
        &["val"],
        vec![vec![Value::Text("new".into())]],
    );
}

// ---- DELETE ----

#[test]
fn test_delete() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'a');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'b');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (3, 'c');", 1);

    assert_rows_affected(&mut vm, "DELETE FROM t1 WHERE id = 2;", 1);

    assert_query_result(
        &mut vm,
        "SELECT id FROM t1;",
        &["id"],
        vec![vec![Value::Integer(1)], vec![Value::Integer(3)]],
    );
}

#[test]
fn test_delete_all() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1);", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2);", 1);

    assert_rows_affected(&mut vm, "DELETE FROM t1;", 2);

    assert_query_result(&mut vm, "SELECT * FROM t1;", &["id"], vec![]);
}

// ---- ORDER BY / LIMIT / OFFSET ----

#[test]
fn test_order_by() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER, name TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (3, 'c');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'a');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'b');", 1);

    assert_query_result(
        &mut vm,
        "SELECT name FROM t1 ORDER BY id ASC;",
        &["name"],
        vec![
            vec![Value::Text("a".into())],
            vec![Value::Text("b".into())],
            vec![Value::Text("c".into())],
        ],
    );

    assert_query_result(
        &mut vm,
        "SELECT name FROM t1 ORDER BY id DESC;",
        &["name"],
        vec![
            vec![Value::Text("c".into())],
            vec![Value::Text("b".into())],
            vec![Value::Text("a".into())],
        ],
    );
}

#[test]
fn test_limit_offset() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER PRIMARY KEY);");
    for i in 1..=10 {
        assert_rows_affected(&mut vm, &format!("INSERT INTO t1 VALUES ({});", i), 1);
    }

    assert_query_result(
        &mut vm,
        "SELECT id FROM t1 LIMIT 3;",
        &["id"],
        vec![
            vec![Value::Integer(1)],
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
        ],
    );

    assert_query_result(
        &mut vm,
        "SELECT id FROM t1 LIMIT 3 OFFSET 2;",
        &["id"],
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)],
        ],
    );
}

// ---- Expressions & Functions ----

#[test]
fn test_arithmetic_expressions() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (a INTEGER, b INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (10, 3);", 1);

    assert_query_result(
        &mut vm,
        "SELECT a + b, a - b, a * b, a / b, a % b FROM t1;",
        &["?", "?", "?", "?", "?"],
        vec![vec![
            Value::Integer(13),
            Value::Integer(7),
            Value::Integer(30),
            Value::Integer(3),
            Value::Integer(1),
        ]],
    );
}

#[test]
fn test_string_concat() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (first TEXT, last TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('John', 'Doe');", 1);

    assert_query_result(
        &mut vm,
        "SELECT first || ' ' || last FROM t1;",
        &["?"],
        vec![vec![Value::Text("John Doe".into())]],
    );
}

#[test]
fn test_builtin_functions() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (val TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('Hello World');", 1);

    assert_query_result(
        &mut vm,
        "SELECT UPPER(val) FROM t1;",
        &["UPPER"],
        vec![vec![Value::Text("HELLO WORLD".into())]],
    );

    assert_query_result(
        &mut vm,
        "SELECT LOWER(val) FROM t1;",
        &["LOWER"],
        vec![vec![Value::Text("hello world".into())]],
    );

    assert_query_result(
        &mut vm,
        "SELECT LENGTH(val) FROM t1;",
        &["LENGTH"],
        vec![vec![Value::Integer(11)]],
    );
}

#[test]
fn test_substr_function() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (val TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('Hello World');", 1);

    assert_query_result(
        &mut vm,
        "SELECT SUBSTR(val, 1, 5) FROM t1;",
        &["SUBSTR"],
        vec![vec![Value::Text("Hello".into())]],
    );
}

#[test]
fn test_coalesce() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (a INTEGER, b INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 (b) VALUES (42);", 1);

    assert_query_result(
        &mut vm,
        "SELECT COALESCE(a, b) FROM t1;",
        &["COALESCE"],
        vec![vec![Value::Integer(42)]],
    );
}

// ---- DISTINCT ----

#[test]
fn test_distinct() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (val TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('a');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('b');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('a');", 1);

    assert_query_result(
        &mut vm,
        "SELECT DISTINCT val FROM t1;",
        &["val"],
        vec![vec![Value::Text("a".into())], vec![Value::Text("b".into())]],
    );
}

// ---- JOIN ----

#[test]
fn test_inner_join() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE users (user_id INTEGER PRIMARY KEY, name TEXT);",
    );
    assert_ok(
        &mut vm,
        "CREATE TABLE orders (order_id INTEGER PRIMARY KEY, uid INTEGER, item TEXT);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO users VALUES (1, 'Alice');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO users VALUES (2, 'Bob');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO orders VALUES (1, 1, 'Book');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO orders VALUES (2, 1, 'Pen');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO orders VALUES (3, 2, 'Notebook');", 1);

    match vm
        .execute_sql("SELECT name, item FROM users JOIN orders ON user_id = uid;")
        .unwrap()
    {
        ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 3);
        }
        _ => panic!("Expected query result"),
    }
}

// ---- Autoincrement ----

#[test]
fn test_autoincrement() {
    let mut vm = VM::new_memory();
    assert_ok(
        &mut vm,
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT);",
    );
    assert_rows_affected(&mut vm, "INSERT INTO t1 (name) VALUES ('Alice');", 1);
    assert_rows_affected(&mut vm, "INSERT INTO t1 (name) VALUES ('Bob');", 1);

    assert_query_result(
        &mut vm,
        "SELECT id, name FROM t1;",
        &["id", "name"],
        vec![
            vec![Value::Integer(1), Value::Text("Alice".into())],
            vec![Value::Integer(2), Value::Text("Bob".into())],
        ],
    );
}

// ---- Explain ----

#[test]
fn test_explain() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (id INTEGER);");
    match vm
        .execute_sql("EXPLAIN SELECT * FROM t1 WHERE id > 5;")
        .unwrap()
    {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("SCAN"));
            assert!(plan.contains("FILTER"));
        }
        _ => panic!("Expected Explain result"),
    }
}

// ---- File-based DB ----

#[test]
fn test_file_persistence() {
    let path = "test_persist.db";
    // Clean up
    let _ = std::fs::remove_file(path);

    // Create and insert
    {
        let mut vm = VM::open(path).unwrap();
        assert_ok(
            &mut vm,
            "CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT);",
        );
        assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (1, 'hello');", 1);
        assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (2, 'world');", 1);
    }

    // Reopen and verify
    {
        let mut vm = VM::open(path).unwrap();
        assert_query_result(
            &mut vm,
            "SELECT * FROM t1;",
            &["id", "val"],
            vec![
                vec![Value::Integer(1), Value::Text("hello".into())],
                vec![Value::Integer(2), Value::Text("world".into())],
            ],
        );
    }

    // Clean up
    let _ = std::fs::remove_file(path);
}

// ---- SQL Parsing Edge Cases ----

#[test]
fn test_string_escaping() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (val TEXT);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES ('it''s a test');", 1);

    assert_query_result(
        &mut vm,
        "SELECT val FROM t1;",
        &["val"],
        vec![vec![Value::Text("it's a test".into())]],
    );
}

#[test]
fn test_negative_numbers() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (val INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (-42);", 1);

    assert_query_result(
        &mut vm,
        "SELECT val FROM t1;",
        &["val"],
        vec![vec![Value::Integer(-42)]],
    );
}

#[test]
fn test_select_alias() {
    let mut vm = VM::new_memory();
    assert_ok(&mut vm, "CREATE TABLE t1 (value INTEGER);");
    assert_rows_affected(&mut vm, "INSERT INTO t1 VALUES (10);", 1);

    assert_query_result(
        &mut vm,
        "SELECT value AS v FROM t1;",
        &["v"],
        vec![vec![Value::Integer(10)]],
    );
}
