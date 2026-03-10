use super::*;

// ---- EXPLAIN ----

#[test]
fn test_explain_select() {
    let _result = exec("EXPLAIN SELECT * FROM t1");
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    match vm
        .execute_sql("EXPLAIN SELECT * FROM t1 WHERE id > 1 ORDER BY id LIMIT 5")
        .unwrap()
    {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("SCAN"));
            assert!(plan.contains("FILTER"));
            assert!(plan.contains("SORT"));
            assert!(plan.contains("LIMIT"));
        }
        _ => panic!("expected Explain"),
    }
}

#[test]
fn test_explain_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    match vm.execute_sql("EXPLAIN INSERT INTO t1 VALUES (1)").unwrap() {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("INSERT INTO t1"));
        }
        _ => panic!("expected Explain"),
    }
}

#[test]
fn test_explain_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    match vm
        .execute_sql("EXPLAIN UPDATE t1 SET val = 1 WHERE id = 1")
        .unwrap()
    {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("UPDATE t1"));
            assert!(plan.contains("FILTER"));
        }
        _ => panic!("expected Explain"),
    }
}

#[test]
fn test_explain_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    match vm
        .execute_sql("EXPLAIN DELETE FROM t1 WHERE id = 1")
        .unwrap()
    {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("DELETE FROM t1"));
            assert!(plan.contains("FILTER"));
        }
        _ => panic!("expected Explain"),
    }
}

// ---- CREATE INDEX ----

#[test]
fn test_create_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    match vm.execute_sql("CREATE INDEX idx1 ON t1 (name)").unwrap() {
        ExecResult::Ok { message } => {
            assert!(message.contains("Index"));
        }
        _ => panic!("expected Ok"),
    }
}

#[test]
fn test_create_unique_index_rejects_existing_duplicates() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, email TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a@example.com')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'a@example.com')")
        .unwrap();

    let result = vm.execute_sql("CREATE UNIQUE INDEX uq_email ON t1 (email)");
    assert!(result.is_err());
}

#[test]
fn test_unique_index_insert_conflict() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, email TEXT)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX uq_email ON t1 (email)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a@example.com')")
        .unwrap();

    let result = vm.execute_sql("INSERT INTO t1 VALUES (2, 'a@example.com')");
    assert!(result.is_err());
}

#[test]
fn test_unique_index_update_conflict() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, email TEXT)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX uq_email ON t1 (email)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a@example.com')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'b@example.com')")
        .unwrap();

    let result = vm.execute_sql("UPDATE t1 SET email = 'a@example.com' WHERE id = 2");
    assert!(result.is_err());
}

#[test]
fn test_unique_index_multi_column_allows_same_prefix() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX uq_ab ON t1 (a, b)")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 10, 100)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 10, 200)")
        .unwrap();
}

#[test]
fn test_unique_index_multi_column_rejects_full_duplicate() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX uq_ab ON t1 (a, b)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10, 100)")
        .unwrap();

    let result = vm.execute_sql("INSERT INTO t1 VALUES (2, 10, 100)");
    assert!(result.is_err());
}

// ---- expr_display_name coverage ----

#[test]
fn test_select_alias() {
    let mut vm = VM::new_memory();
    match vm
        .execute_sql("SELECT 1 AS one, 'hello' AS greeting")
        .unwrap()
    {
        ExecResult::QueryResult { columns, .. } => {
            assert_eq!(columns[0], "one");
            assert_eq!(columns[1], "greeting");
        }
        _ => panic!(),
    }
}

#[test]
fn test_select_expr_display_names() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    match vm
        .execute_sql("SELECT id, 42, 3.14, 'hello' FROM t1")
        .unwrap()
    {
        ExecResult::QueryResult { columns, .. } => {
            assert_eq!(columns[0], "id");
            assert_eq!(columns[1], "42");
            assert_eq!(columns[2], "3.14");
            assert_eq!(columns[3], "'hello'");
        }
        _ => panic!(),
    }
}

// ---- Subquery expr ----
#[test]
fn test_subquery_from() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT * FROM (SELECT 1) AS sub");
    assert_eq!(rows.len(), 1);
}

// ---- Update without WHERE ----
#[test]
fn test_update_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    match vm.execute_sql("UPDATE t1 SET val = 0").unwrap() {
        ExecResult::RowsAffected { count, .. } => assert_eq!(count, 2),
        _ => panic!(),
    }
}

// ---- Length of non-text ----
#[test]
fn test_func_length_blob() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT LENGTH(x'ABCD')")[0][0],
        Value::Integer(2)
    );
}

#[test]
fn test_func_length_integer() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT LENGTH(42)")[0][0],
        Value::Integer(2)
    );
}

// ---- ABS of non-numeric ----
#[test]
fn test_func_abs_null() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT ABS(NULL)")[0][0], Value::Null);
}

#[test]
fn test_func_abs_text() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT ABS('hello')")[0][0],
        Value::Null
    );
}

// ---- Real division/subtraction/multiply paths ----

#[test]
fn test_real_division() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT 7.0 / 2.0")[0][0],
        Value::Real(3.5)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 7 / 2.0")[0][0],
        Value::Real(3.5)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 7.0 / 2")[0][0],
        Value::Real(3.5)
    );
    // Division by zero with reals
    assert_eq!(query_rows(&mut vm, "SELECT 7.0 / 0.0")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT 7 / 0.0")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT 7.0 / 0")[0][0], Value::Null);
}

#[test]
fn test_real_subtraction() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT 5.0 - 2.0")[0][0],
        Value::Real(3.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 5 - 2.0")[0][0],
        Value::Real(3.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 5.0 - 2")[0][0],
        Value::Real(3.0)
    );
}

#[test]
fn test_real_multiplication() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT 3.0 * 2.0")[0][0],
        Value::Real(6.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 3 * 2.0")[0][0],
        Value::Real(6.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT 3.0 * 2")[0][0],
        Value::Real(6.0)
    );
}

// ---- BETWEEN ----

#[test]
fn test_between_via_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 5)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 15)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 25)").unwrap();

    // BETWEEN not supported in tokenizer, use AND equivalent
    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val >= 10 AND val <= 20");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Integer(15));

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val < 10 OR val > 20");
    assert_eq!(rows.len(), 2);
}

// ---- NOT LIKE / NOT IN ----

#[test]
fn test_not_like() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE name NOT LIKE 'A%'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Text("Bob".into()));
}

#[test]
fn test_not_in() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE id NOT IN (1, 3)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ---- Aggregates: SUM, AVG, MIN, MAX ----

#[test]
fn test_aggregate_sum_having() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 5)").unwrap();

    // SUM is correctly evaluated in HAVING clause
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING SUM(val) > 10 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

#[test]
fn test_aggregate_avg_having() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 5)").unwrap();

    // AVG(A)=15, AVG(B)=5. HAVING AVG(val) > 10 => only A
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING AVG(val) > 10 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

#[test]
fn test_aggregate_min_having() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 20)")
        .unwrap();

    // MIN(A)=10, MIN(B)=20. HAVING MIN(val) >= 20 => only B
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MIN(val) >= 20 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("B".into()));
}

#[test]
fn test_aggregate_max_having() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 20)")
        .unwrap();

    // MAX(A)=30, MAX(B)=20. HAVING MAX(val) > 25 => only A
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MAX(val) > 25 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

