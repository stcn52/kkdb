use super::*;

// ---- JOIN ----

#[test]
fn test_cross_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1, t2");
    assert_eq!(rows.len(), 4); // 2 * 2
}

#[test]
fn test_left_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY, a_ref INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 LEFT JOIN t2 ON a = a_ref");
    assert_eq!(rows.len(), 2);
    // One row should have a matched t2 columns, the other should have NULLs
    let has_null = rows.iter().any(|r| r.iter().any(|v| *v == Value::Null));
    let has_match = rows.iter().any(|r| r.contains(&Value::Integer(10)));
    assert!(
        has_null,
        "LEFT JOIN should produce NULL for unmatched right side"
    );
    assert!(has_match, "LEFT JOIN should include matched rows");
}

// ---- Transaction stubs ----

#[test]
fn test_begin_commit_rollback() {
    let mut vm = VM::new_memory();
    assert!(matches!(
        vm.execute_sql("BEGIN").unwrap(),
        ExecResult::Ok { .. }
    ));
    assert!(matches!(
        vm.execute_sql("COMMIT").unwrap(),
        ExecResult::Ok { .. }
    ));
    assert!(matches!(
        vm.execute_sql("ROLLBACK").unwrap(),
        ExecResult::Ok { .. }
    ));
}

