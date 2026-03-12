// R6: SELECT … FOR UPDATE tests

use crate::types::Value;

fn exec(vm: &mut crate::vm::execute::VM, sql: &str) -> crate::error::Result<crate::vm::execute::ExecResult> {
    vm.execute_sql(sql)
}

fn setup(vm: &mut crate::vm::execute::VM) {
    exec(vm, "CREATE TABLE t_fu (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    exec(vm, "INSERT INTO t_fu VALUES (1, 'a'), (2, 'b'), (3, 'c')").unwrap();
}

#[test]
fn test_select_for_update_basic() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    let res = exec(&mut vm, "SELECT * FROM t_fu WHERE id = 1 FOR UPDATE").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { columns, rows } => {
            assert!(!columns.is_empty());
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][0], Value::Integer(1));
        }
        _ => panic!("expected QueryResult"),
    }
    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_select_for_update_no_txn() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    let res = exec(&mut vm, "SELECT * FROM t_fu FOR UPDATE").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 3);
        }
        _ => panic!("expected QueryResult"),
    }
}

#[test]
fn test_select_for_update_locks_rows() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    exec(&mut vm, "SELECT * FROM t_fu WHERE id = 2 FOR UPDATE").unwrap();
    let res = exec(&mut vm, "UPDATE t_fu SET val = 'B' WHERE id = 2").unwrap();
    match res {
        crate::vm::execute::ExecResult::RowsAffected { count, .. } => assert_eq!(count, 1),
        _ => panic!("expected RowsAffected"),
    }
    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_select_for_update_with_limit() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    let res = exec(&mut vm, "SELECT * FROM t_fu LIMIT 2 FOR UPDATE").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 2);
        }
        _ => panic!("expected QueryResult"),
    }
    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_select_for_update_with_order_by() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    let res = exec(&mut vm, "SELECT * FROM t_fu ORDER BY id DESC FOR UPDATE").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 3);
            assert_eq!(rows[0][0], Value::Integer(3));
            assert_eq!(rows[1][0], Value::Integer(2));
            assert_eq!(rows[2][0], Value::Integer(1));
        }
        _ => panic!("expected QueryResult"),
    }
    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_select_for_update_rollback_releases() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    exec(&mut vm, "SELECT * FROM t_fu WHERE id = 1 FOR UPDATE").unwrap();
    exec(&mut vm, "ROLLBACK").unwrap();
    exec(&mut vm, "BEGIN").unwrap();
    exec(&mut vm, "SELECT * FROM t_fu WHERE id = 1 FOR UPDATE").unwrap();
    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_for_update_then_update() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    exec(&mut vm, "SELECT * FROM t_fu WHERE id = 3 FOR UPDATE").unwrap();
    exec(&mut vm, "UPDATE t_fu SET val = 'C' WHERE id = 3").unwrap();
    exec(&mut vm, "COMMIT").unwrap();
    let res = exec(&mut vm, "SELECT val FROM t_fu WHERE id = 3").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows[0][0], Value::Text(std::sync::Arc::from("C")));
        }
        _ => panic!("expected QueryResult"),
    }
}

#[test]
fn test_for_update_empty_result() {
    let mut vm = crate::vm::execute::VM::new_memory();
    setup(&mut vm);
    exec(&mut vm, "BEGIN").unwrap();
    let res = exec(&mut vm, "SELECT * FROM t_fu WHERE id = 999 FOR UPDATE").unwrap();
    match res {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 0);
        }
        _ => panic!("expected QueryResult"),
    }
    exec(&mut vm, "COMMIT").unwrap();
}
