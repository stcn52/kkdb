use super::*;

// ---- Binary operators ----

#[test]
fn test_arithmetic_ops() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT 10 + 3")[0][0],
        Value::Integer(13)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 10 - 3")[0][0],
        Value::Integer(7)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 10 * 3")[0][0],
        Value::Integer(30)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 10 / 3")[0][0],
        Value::Integer(3)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 10 % 3")[0][0],
        Value::Integer(1)
    );
}

#[test]
fn test_division_by_zero() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT 10 / 0")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT 10 % 0")[0][0], Value::Null);
}

#[test]
fn test_real_arithmetic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1.5 + 2.5");
    assert_eq!(rows[0][0], Value::Real(4.0));
}

#[test]
fn test_mixed_int_real_arithmetic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1 + 2.0");
    assert_eq!(rows[0][0], Value::Real(3.0));
}

#[test]
fn test_comparison_ops() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT 1 = 1")[0][0], Value::Integer(1));
    assert_eq!(query_rows(&mut vm, "SELECT 1 = 2")[0][0], Value::Integer(0));
    assert_eq!(
        query_rows(&mut vm, "SELECT 1 != 2")[0][0],
        Value::Integer(1)
    );
    assert_eq!(query_rows(&mut vm, "SELECT 1 < 2")[0][0], Value::Integer(1));
    assert_eq!(
        query_rows(&mut vm, "SELECT 1 <= 1")[0][0],
        Value::Integer(1)
    );
    assert_eq!(query_rows(&mut vm, "SELECT 2 > 1")[0][0], Value::Integer(1));
    assert_eq!(
        query_rows(&mut vm, "SELECT 1 >= 1")[0][0],
        Value::Integer(1)
    );
}

#[test]
fn test_null_propagation() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT NULL + 1")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT NULL = NULL")[0][0], Value::Null);
}

#[test]
fn test_null_and_or() {
    let mut vm = VM::new_memory();
    // NULL AND false => false (0)
    assert_eq!(
        query_rows(&mut vm, "SELECT NULL AND 0")[0][0],
        Value::Integer(0)
    );
    // NULL OR true => true (1)
    assert_eq!(
        query_rows(&mut vm, "SELECT NULL OR 1")[0][0],
        Value::Integer(1)
    );
    // NULL AND NULL => NULL
    assert_eq!(
        query_rows(&mut vm, "SELECT NULL AND NULL")[0][0],
        Value::Null
    );
    // NULL OR false => NULL
    assert_eq!(query_rows(&mut vm, "SELECT NULL OR 0")[0][0], Value::Null);
}

// ---- Unary operators ----

#[test]
fn test_unary_minus() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT -42")[0][0], Value::Integer(-42));
    assert_eq!(
        query_rows(&mut vm, "SELECT -3.14")[0][0],
        Value::Real(-3.14)
    );
    assert_eq!(query_rows(&mut vm, "SELECT -NULL")[0][0], Value::Null);
}

#[test]
fn test_unary_not() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT NOT 1")[0][0], Value::Integer(0));
    assert_eq!(query_rows(&mut vm, "SELECT NOT 0")[0][0], Value::Integer(1));
}

// ---- IS NULL / IN / LIKE / BETWEEN ----

#[test]
fn test_is_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'x')").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val IS NULL");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val IS NOT NULL");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_in_list() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE id IN (1, 3)");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_like() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE name LIKE 'A%'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_between_via_and() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 5)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 15)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 25)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val >= 10 AND val <= 20");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Integer(15));
}

