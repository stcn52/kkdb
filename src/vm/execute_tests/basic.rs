use super::*;

use super::*;




// ---- like_match ----

#[test]
fn test_like_exact() {
    assert!(like_match("hello", "hello", None, false));
    assert!(!like_match("hello", "world", None, false));
}

#[test]
fn test_like_percent() {
    assert!(like_match("hello", "%", None, false));
    assert!(like_match("hello", "h%", None, false));
    assert!(like_match("hello", "%o", None, false));
    assert!(like_match("hello", "%ell%", None, false));
    assert!(!like_match("hello", "x%", None, false));
}

#[test]
fn test_like_underscore() {
    assert!(like_match("hello", "_ello", None, false));
    assert!(like_match("hello", "hell_", None, false));
    assert!(like_match("hello", "_____", None, false));
    assert!(!like_match("hello", "____", None, false));
    assert!(!like_match("hello", "______", None, false));
}

#[test]
fn test_like_mixed() {
    assert!(like_match("hello world", "hello%", None, false));
    assert!(like_match("hello world", "%world", None, false));
    assert!(like_match("hello world", "h_llo%", None, false));
    assert!(like_match("abc", "a_c", None, false));
}

#[test]
fn test_like_case_insensitive() {
    assert!(like_match("Hello", "hello", None, true));
    assert!(like_match("HELLO", "h%", None, true));
}

#[test]
fn test_like_empty() {
    assert!(like_match("", "", None, false));
    assert!(like_match("", "%", None, false));
    assert!(!like_match("", "_", None, false));
    assert!(!like_match("a", "", None, false));
}

// ---- VM basic ----

#[test]
fn test_vm_new_memory() {
    let vm = VM::new_memory();
    assert!(vm.pager.is_memory);
}

// ---- CREATE / DROP ----

#[test]
fn test_create_and_drop_table() {
    let results = exec_multi(&[
        "CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)",
        "DROP TABLE t1",
    ]);
    assert!(matches!(results[0], ExecResult::Ok { .. }));
    assert!(matches!(results[1], ExecResult::Ok { .. }));
}

#[test]
fn test_create_table_if_not_exists() {
    let results = exec_multi(&[
        "CREATE TABLE t1 (id INTEGER)",
        "CREATE TABLE IF NOT EXISTS t1 (id INTEGER)",
    ]);
    assert!(matches!(results[1], ExecResult::Ok { .. }));
}

#[test]
fn test_drop_table_if_exists() {
    let result = exec("DROP TABLE IF EXISTS nonexistent");
    assert!(matches!(result, ExecResult::Ok { .. }));
}

// ---- INSERT ----

#[test]
fn test_insert_and_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_insert_with_columns() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 (name, id) VALUES ('Alice', 1)")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Null);
}

#[test]
fn test_insert_autoincrement() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 (name) VALUES ('A')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 (name) VALUES ('B')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(2));
}

#[test]
fn test_insert_column_count_mismatch() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER, name TEXT)")
        .unwrap();
    let result = vm.execute_sql("INSERT INTO t1 VALUES (1)");
    assert!(result.is_err());
}

#[test]
fn test_insert_not_null_violation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .unwrap();
    let result = vm.execute_sql("INSERT INTO t1 VALUES (1, NULL)");
    assert!(result.is_err());
}

// ---- SELECT expressions ----

#[test]
fn test_select_without_from() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1 + 2");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_select_string_concat() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    assert_eq!(rows[0][0], Value::Text("hello world".into()));
}

#[test]
fn test_select_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val > 15");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_select_distinct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B')").unwrap();

    let rows = query_rows(&mut vm, "SELECT DISTINCT val FROM t1");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_select_order_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Charlie')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Bob')").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY name ASC");
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[1][1], Value::Text("Bob".into()));
    assert_eq!(rows[2][1], Value::Text("Charlie".into()));
}

#[test]
fn test_select_order_by_desc() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 30)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 20)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY val DESC");
    assert_eq!(rows[0][1], Value::Integer(30));
}

#[test]
fn test_select_limit_offset() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({})", i))
            .unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT * FROM t1 LIMIT 3 OFFSET 2");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_select_offset_beyond_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 LIMIT 10 OFFSET 100");
    assert!(rows.is_empty());
}

#[test]
fn test_order_by_limit_topn_path() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for (id, val) in [(1, 30), (2, 10), (3, 50), (4, 20), (5, 40)] {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", id, val))
            .unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT val FROM t1 ORDER BY val DESC LIMIT 3");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(50)],
            vec![Value::Integer(40)],
            vec![Value::Integer(30)]
        ]
    );
}

#[test]
fn test_order_by_limit_zero() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM t1 ORDER BY val LIMIT 0");
    assert!(rows.is_empty());
}

#[test]
fn test_order_by_limit_with_offset_topn_path() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for (id, val) in [(1, 30), (2, 10), (3, 50), (4, 20), (5, 40), (6, 60)] {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", id, val))
            .unwrap();
    }

    let rows = query_rows(
        &mut vm,
        "SELECT val FROM t1 ORDER BY val DESC LIMIT 2 OFFSET 2",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Integer(40)], vec![Value::Integer(30)]]
    );
}

