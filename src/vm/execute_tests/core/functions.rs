use super::*;

// ---- Scalar functions ----

#[test]
fn test_func_upper_lower() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT UPPER('hello')")[0][0],
        Value::Text("HELLO".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT LOWER('HELLO')")[0][0],
        Value::Text("hello".into())
    );
}

#[test]
fn test_func_length() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT LENGTH('hello')")[0][0],
        Value::Integer(5)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT LENGTH(NULL)")[0][0],
        Value::Null
    );
}

#[test]
#[allow(clippy::approx_constant)]
fn test_func_abs() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT ABS(-42)")[0][0],
        Value::Integer(42)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT ABS(3.14)")[0][0],
        Value::Real(3.14)
    );
}

#[test]
fn test_func_typeof() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(42)")[0][0],
        Value::Text("integer".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(3.14)")[0][0],
        Value::Text("real".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF('hi')")[0][0],
        Value::Text("text".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(NULL)")[0][0],
        Value::Text("null".into())
    );
}

#[test]
fn test_func_coalesce() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT COALESCE(NULL, NULL, 3)")[0][0],
        Value::Integer(3)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT COALESCE(1, 2)")[0][0],
        Value::Integer(1)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT COALESCE(NULL, NULL)")[0][0],
        Value::Null
    );
}

#[test]
fn test_func_ifnull() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT IFNULL(NULL, 'default')")[0][0],
        Value::Text("default".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT IFNULL('val', 'default')")[0][0],
        Value::Text("val".into())
    );
}

#[test]
fn test_func_substr() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT SUBSTR('hello', 2, 3)")[0][0],
        Value::Text("ell".into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT SUBSTR('hello', 2)")[0][0],
        Value::Text("ello".into())
    );
}

#[test]
fn test_func_replace() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT REPLACE('hello world', 'world', 'rust')")[0][0],
        Value::Text("hello rust".into())
    );
}

#[test]
fn test_func_trim() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT TRIM('  hello  ')")[0][0],
        Value::Text("hello".into())
    );
}

#[test]
fn test_func_unknown() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SELECT UNKNOWN_FUNC(1)");
    assert!(result.is_err());
}

// ---- Aggregate functions ----

#[test]
fn test_aggregate_count() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 GROUP BY 1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_group_by_with_having() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 5)").unwrap();

    // A: sum=30 > 25, B: sum=5
    let rows = query_rows(
        &mut vm,
        "SELECT cat, SUM(val) FROM t1 GROUP BY cat HAVING SUM(val) > 25",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}
