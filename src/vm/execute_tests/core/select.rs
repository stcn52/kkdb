use super::*;

// ---- UPDATE / DELETE ----

#[test]
fn test_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'old')").unwrap();
    vm.execute_sql("UPDATE t1 SET val = 'new' WHERE id = 1")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM t1");
    assert_eq!(rows[0][0], Value::Text("new".into()));
}

#[test]
fn test_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B')").unwrap();
    vm.execute_sql("DELETE FROM t1 WHERE id = 1").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_delete_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("DELETE FROM t1").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert!(rows.is_empty());
}

#[test]
fn test_update_where_in_with_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=4 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 0)", i))
            .unwrap();
    }

    vm.execute_sql("UPDATE t1 SET val = 9 WHERE id IN (1, 3)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, val FROM t1 ORDER BY id");
    assert_eq!(rows[0], vec![Value::Integer(1), Value::Integer(9)]);
    assert_eq!(rows[1], vec![Value::Integer(2), Value::Integer(0)]);
    assert_eq!(rows[2], vec![Value::Integer(3), Value::Integer(9)]);
    assert_eq!(rows[3], vec![Value::Integer(4), Value::Integer(0)]);
}

#[test]
fn test_delete_where_in_with_null_item() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3)").unwrap();

    vm.execute_sql("DELETE FROM t1 WHERE id IN (2, NULL)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM t1 ORDER BY id");
    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(3)]]);
}

#[test]
fn test_select_column_equals_column_falls_back_from_index_path() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE id = id ORDER BY id");
    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(2)]]);
}

#[test]
fn test_select_range_with_index_pushdown() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON t1 (val)").unwrap();
    for i in 1..=6 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val >= 30 ORDER BY id");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)],
            vec![Value::Integer(6)]
        ]
    );
}

#[test]
fn test_update_range_with_index_pushdown() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=5 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 0)", i))
            .unwrap();
    }

    vm.execute_sql("UPDATE t1 SET val = 1 WHERE id > 3")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, val FROM t1 ORDER BY id");
    assert_eq!(rows[0], vec![Value::Integer(1), Value::Integer(0)]);
    assert_eq!(rows[1], vec![Value::Integer(2), Value::Integer(0)]);
    assert_eq!(rows[2], vec![Value::Integer(3), Value::Integer(0)]);
    assert_eq!(rows[3], vec![Value::Integer(4), Value::Integer(1)]);
    assert_eq!(rows[4], vec![Value::Integer(5), Value::Integer(1)]);
}

#[test]
fn test_delete_range_with_index_pushdown() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=5 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({})", i))
            .unwrap();
    }

    vm.execute_sql("DELETE FROM t1 WHERE id <= 2").unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM t1 ORDER BY id");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(3)],
            vec![Value::Integer(4)],
            vec![Value::Integer(5)]
        ]
    );
}

#[test]
fn test_select_between_with_index_pushdown() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON t1 (val)").unwrap();
    for i in 1..=6 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }

    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE val BETWEEN 20 AND 40 ORDER BY id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(4)]
        ]
    );
}

#[test]
fn test_update_between_with_index_pushdown() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=5 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 0)", i))
            .unwrap();
    }

    vm.execute_sql("UPDATE t1 SET val = 7 WHERE id BETWEEN 2 AND 4")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, val FROM t1 ORDER BY id");
    assert_eq!(rows[0], vec![Value::Integer(1), Value::Integer(0)]);
    assert_eq!(rows[1], vec![Value::Integer(2), Value::Integer(7)]);
    assert_eq!(rows[2], vec![Value::Integer(3), Value::Integer(7)]);
    assert_eq!(rows[3], vec![Value::Integer(4), Value::Integer(7)]);
    assert_eq!(rows[4], vec![Value::Integer(5), Value::Integer(0)]);
}

#[test]
fn test_select_range_with_index_pushdown_large_candidate_set() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=150 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i))
            .unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE id >= 1 ORDER BY id");
    assert_eq!(rows.len(), 150);
    assert_eq!(rows[0], vec![Value::Integer(1)]);
    assert_eq!(rows[149], vec![Value::Integer(150)]);
}

#[test]
fn test_update_range_with_index_pushdown_large_candidate_set() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_id ON t1 (id)").unwrap();
    for i in 1..=150 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 0)", i))
            .unwrap();
    }

    vm.execute_sql("UPDATE t1 SET val = 1 WHERE id >= 1")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE val = 1");
    assert_eq!(rows[0][0], Value::Integer(150));
}
