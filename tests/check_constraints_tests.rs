use kkdb::vm::execute::VM;
use kkdb::error::KkdbError;

#[test]
fn test_check_constraints_column_level() {
    let mut vm = VM::new_memory();
    
    // 1. Column-level CHECK
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, age INTEGER CHECK (age >= 18))").unwrap();
    
    // Valid insert
    vm.execute_sql("INSERT INTO t1 (id, age) VALUES (1, 18)").unwrap();
    vm.execute_sql("INSERT INTO t1 (id, age) VALUES (2, 20)").unwrap();
    
    // Invalid insert
    let err = vm.execute_sql("INSERT INTO t1 (id, age) VALUES (3, 17)").unwrap_err();
    assert!(matches!(err, KkdbError::ConstraintViolation(_)));
    
    // Valid update
    vm.execute_sql("UPDATE t1 SET age = 19 WHERE id = 1").unwrap();
    
    // Invalid update
    let err = vm.execute_sql("UPDATE t1 SET age = 15 WHERE id = 2").unwrap_err();
    assert!(matches!(err, KkdbError::ConstraintViolation(_)));
    
    // NULL passes CHECK dynamically (SQL standard)
    vm.execute_sql("INSERT INTO t1 (id, age) VALUES (4, NULL)").unwrap();
}

#[test]
fn test_check_constraints_table_level() {
    let mut vm = VM::new_memory();

    // 2. Table-level CHECK
    vm.execute_sql("CREATE TABLE t2 (start_val INTEGER, end_val INTEGER, CHECK (start_val <= end_val))").unwrap();
    
    // Valid insert
    vm.execute_sql("INSERT INTO t2 VALUES (10, 20)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (15, 15)").unwrap();
    
    // Invalid insert
    let err = vm.execute_sql("INSERT INTO t2 VALUES (30, 20)").unwrap_err();
    assert!(matches!(err, KkdbError::ConstraintViolation(_)));

    // Unknown/NULL evaluation passes
    vm.execute_sql("INSERT INTO t2 VALUES (NULL, 20)").unwrap();
}

#[test]
fn test_load_schema_preserves_check() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut vm = VM::open(dir.path().to_str().unwrap()).unwrap();
        vm.execute_sql("CREATE TABLE t1 (x INTEGER CHECK (x > 0))").unwrap();
        vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    }
    // Re-open and verify schema preserved the CHECK constraint
    {
        let mut vm = VM::open(dir.path().to_str().unwrap()).unwrap();
        
        let err = vm.execute_sql("INSERT INTO t1 VALUES (-1)").unwrap_err();
        assert!(matches!(err, KkdbError::ConstraintViolation(_)));
        
        vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    }
}
