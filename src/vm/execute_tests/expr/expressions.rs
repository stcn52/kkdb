use super::*;

// ---- INNER JOIN ----

#[test]
fn test_inner_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY, a_ref INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 3)").unwrap(); // no match

    let rows = query_rows(
        &mut vm,
        "SELECT t1.name, t2.b FROM t1 INNER JOIN t2 ON a = a_ref",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_join_using() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (1, 'ok')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (3, 'skip')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT t1.name, t2.val FROM t1 JOIN t2 USING (id) ORDER BY t1.id",
    );
    assert_eq!(
        rows,
        vec![vec![Value::Text("Alice".into()), Value::Text("ok".into())]]
    );
}

#[test]
fn test_join_using_inner_join_tree() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, tag TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t3 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (1, 'x')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (2, 'y')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (2, 'ok')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (3, 'skip')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT t1.id, t1.name, t2.tag, t3.val \
         FROM t1 JOIN t2 USING (id) JOIN t3 USING (id) \
         ORDER BY t1.id",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::Integer(2),
            Value::Text("b".into()),
            Value::Text("y".into()),
            Value::Text("ok".into())
        ]]
    );
}

#[test]
fn test_join_using_right_join_tree() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, tag TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t3 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (2, 'r')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (2, 'ok')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT t2.id, t2.tag, t3.val \
         FROM t1 RIGHT JOIN t2 ON t1.id = t2.id JOIN t3 USING (id)",
    );
    assert_eq!(
        rows,
        vec![vec![
            Value::Integer(2),
            Value::Text("r".into()),
            Value::Text("ok".into())
        ]]
    );
}

#[test]
fn test_join_using_left_join_tree() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, tag TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t3 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (2, 'hit')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (1, 'one')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (2, 'two')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT t1.id, t3.val \
         FROM t1 LEFT JOIN t2 ON t1.id = t2.id JOIN t3 USING (id) \
         ORDER BY t1.id",
    );
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(1), Value::Text("one".into())],
            vec![Value::Integer(2), Value::Text("two".into())]
        ]
    );
}

#[test]
fn test_join_using_cross_join_tree() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, tag TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t3 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (1, 'one')").unwrap();
    vm.execute_sql("INSERT INTO t3 VALUES (2, 'two')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT t1.id, t2.id, t3.val \
         FROM (t1 CROSS JOIN t2) JOIN t3 USING (id)",
    );
    // USING key is COALESCE(t1.id, t2.id), so left key wins here (1).
    assert_eq!(
        rows,
        vec![vec![
            Value::Integer(1),
            Value::Integer(2),
            Value::Text("one".into())
        ]]
    );
}

// ---- Nested parentheses ----

#[test]
fn test_nested_parentheses() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT (1 + 2) * (3 + 4)");
    assert_eq!(rows[0][0], Value::Integer(21));
}

// ---- VM file-based ----

#[test]
fn test_vm_open_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_vm_open_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    {
        let mut vm = VM::open(path.to_str().unwrap()).unwrap();
        vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
            .unwrap();
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_create_index_persists_without_extra_writes() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_vm_index_persist_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    {
        let mut vm = VM::open(path.to_str().unwrap()).unwrap();
        vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
            .unwrap();
        vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
            .unwrap();
    }

    {
        let vm = VM::open(path.to_str().unwrap()).unwrap();
        assert!(vm.schema.indexes.contains_key("idx_name"));
    }

    let _ = std::fs::remove_file(&path);
}

// ---- Multiple values in INSERT ----

#[test]
fn test_insert_multiple_value_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A'), (2, 'B'), (3, 'C')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[2][1], Value::Text("C".into()));
}

// ---- NULL subtraction/multiply ----

#[test]
fn test_null_arithmetic_variants() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT NULL - 1")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT NULL * 1")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT NULL / 1")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT NULL % 1")[0][0], Value::Null);
    assert_eq!(query_rows(&mut vm, "SELECT NULL || 'x'")[0][0], Value::Null);
}

// ---- COUNT without GROUP BY ----

#[test]
fn test_count_with_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT cat, COUNT(*) FROM t1 GROUP BY cat ORDER BY cat",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("A".into()));
    assert_eq!(rows[0][1], Value::Integer(2)); // A has 2 rows
    assert_eq!(rows[1][0], Value::Text("B".into()));
    assert_eq!(rows[1][1], Value::Integer(1)); // B has 1 row
}

// ---- RIGHT JOIN ----

#[test]
fn test_right_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY, a_ref INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 999)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 RIGHT JOIN t2 ON t1.a = t2.a_ref");
    assert_eq!(rows.len(), 2);
}

// ---- Mixed-type division ----

#[test]
fn test_mixed_type_division() {
    let mut vm = VM::new_memory();
    // Integer / Real
    let rows = query_rows(&mut vm, "SELECT 10 / 3.0");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 3.3333).abs() < 0.01);
    }
    // Real / Integer
    let rows = query_rows(&mut vm, "SELECT 10.0 / 3");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 3.3333).abs() < 0.01);
    }
    // Real / 0 integer
    let rows = query_rows(&mut vm, "SELECT 10.0 / 0");
    assert_eq!(rows[0][0], Value::Null);
    // Integer / 0.0 real
    let rows = query_rows(&mut vm, "SELECT 10 / 0.0");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- GreaterThan / NotEqual comparisons ----

#[test]
fn test_comparison_gt_ne() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val > 15");
    assert_eq!(rows.len(), 2);

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val != 20");
    assert_eq!(rows.len(), 2);
}

// ---- Modulo operator ----

#[test]
fn test_modulo_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 % 3");
    assert_eq!(rows[0][0], Value::Integer(1));

    let rows = query_rows(&mut vm, "SELECT 10 % 0");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- Blob literal ----

#[test]
fn test_blob_literal_expr() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, data BLOB)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, X'DEADBEEF')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT data FROM t1");
    assert_eq!(rows.len(), 1);
    if let Value::Blob(b) = &rows[0][0] {
        assert_eq!(b, &vec![0xDE, 0xAD, 0xBE, 0xEF]);
    } else {
        panic!("expected Blob");
    }
}

// ---- IS NOT NULL ----

#[test]
fn test_is_not_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, NULL)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val IS NOT NULL");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ---- LessThanOrEqual / GreaterThanOrEqual ----

#[test]
fn test_comparison_lte_gte() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val <= 20");
    assert_eq!(rows.len(), 2);

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val >= 20");
    assert_eq!(rows.len(), 2);
}

// ---- PK non-integer insert fallback ----

#[test]
fn test_insert_pk_non_integer() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES ('text_pk', 'A')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

// ---- Distinct aggregate (COUNT DISTINCT) ----

#[test]
fn test_count_distinct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B')").unwrap();

    // Use HAVING with COUNT(DISTINCT cat) to verify the aggregate works
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING COUNT(DISTINCT cat) >= 1 ORDER BY cat",
    );
    assert_eq!(rows.len(), 2);
}

// ---- SUM/AVG with Real values in GROUP BY ----

#[test]
fn test_aggregate_sum_real() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val REAL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 1.5)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 2.5)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 3.0)")
        .unwrap();

    // SUM(A)=4.0, SUM(B)=3.0. HAVING SUM(val) > 3.5 => only A
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING SUM(val) > 3.5 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

#[test]
fn test_aggregate_avg_real() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val REAL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 2.0)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 4.0)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 10.0)")
        .unwrap();

    // AVG(A)=3.0, AVG(B)=10.0. HAVING AVG(val) > 5.0 => only B
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING AVG(val) > 5.0 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("B".into()));
}

#[test]
fn test_aggregate_sum_null_only() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', NULL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 10)")
        .unwrap();

    // SUM(A)=NULL (all nulls), SUM(B)=10. HAVING SUM(val) > 0 => only B
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING SUM(val) > 0 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("B".into()));
}

#[test]
fn test_aggregate_avg_null_only() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', NULL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 10)")
        .unwrap();

    // AVG(A)=NULL (all nulls), AVG(B)=10. HAVING AVG(val) > 0 => only B
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING AVG(val) > 0 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("B".into()));
}

// ---- COALESCE all null ----

#[test]
fn test_coalesce_all_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT COALESCE(NULL, NULL, NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- IFNULL with non-null first arg ----

#[test]
fn test_ifnull_non_null_first() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT IFNULL(42, 99)");
    assert_eq!(rows[0][0], Value::Integer(42));
}

// ---- SUBSTR edge cases ----

#[test]
fn test_substr_beyond_length() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SUBSTR('hello', 100)");
    assert_eq!(rows[0][0], Value::Text("".into()));
}

#[test]
fn test_substr_no_length() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SUBSTR('hello', 2)");
    assert_eq!(rows[0][0], Value::Text("ello".into()));
}

// ---- Unary minus on Real ----

#[test]
#[allow(clippy::approx_constant)]
fn test_unary_minus_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT -3.14");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v + 3.14).abs() < 0.001);
    } else {
        panic!("expected Real");
    }
}

#[test]
fn test_unary_minus_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT -NULL");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- HAVING with binary op on aggregates ----

#[test]
fn test_having_binary_comparison() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 5)").unwrap();

    // HAVING SUM(val) > 10 uses BinaryOp path in eval_expr_with_aggregates
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING SUM(val) > 10 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

// ---- SELECT with expression alias display ----

#[test]
fn test_select_real_literal_display() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SELECT 3.14").unwrap();
    if let ExecResult::QueryResult { columns, .. } = result {
        assert!(!columns.is_empty());
    }
}

#[test]
fn test_select_string_literal_display() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SELECT 'hello'").unwrap();
    if let ExecResult::QueryResult { columns, .. } = result {
        assert!(!columns.is_empty());
    }
}

// ---- NULL AND false / NULL OR true ----

#[test]
fn test_null_and_false() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL AND 0");
    // NULL AND false should be 0
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_null_or_true() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL OR 1");
    // NULL OR true should be 1
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_null_and_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL AND NULL");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_null_or_false() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL OR 0");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- Concat operator ----

#[test]
fn test_concat_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    assert_eq!(rows[0][0], Value::Text("hello world".into()));
}

// ---- DISTINCT with duplicates ----

#[test]
fn test_select_distinct_with_dupes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B')").unwrap();

    let rows = query_rows(&mut vm, "SELECT DISTINCT val FROM t1");
    assert_eq!(rows.len(), 2);
}

// ---- Nested subquery expression ----

#[test]
fn test_select_star_from_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT * FROM (SELECT val FROM t1 ORDER BY id) AS sub",
    );
    assert_eq!(rows.len(), 2);
}

// ---- LEFT JOIN with unmatched rows (null padding) ----

#[test]
fn test_left_join_unmatched() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY, a_ref INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 LEFT JOIN t2 ON t1.a = t2.a_ref");
    assert_eq!(rows.len(), 2);
    // Bob has no match in t2 - should get NULL padding
    let bob_row = rows
        .iter()
        .find(|r| r[1] == Value::Text("Bob".into()))
        .unwrap();
    assert_eq!(bob_row[2], Value::Null); // t2.b is NULL
}

// ---- Real-Real division by zero ----

#[test]
fn test_real_real_division_by_zero() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 3.14 / 0.0");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- UPDATE without WHERE (updates all) ----

#[test]
fn test_update_without_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B')").unwrap();

    let result = vm.execute_sql("UPDATE t1 SET val = 'X'").unwrap();
    if let ExecResult::RowsAffected { count, .. } = result {
        assert_eq!(count, 2);
    }
    let rows = query_rows(&mut vm, "SELECT val FROM t1");
    assert!(rows.iter().all(|r| r[0] == Value::Text("X".into())));
}

// ---- Mixed Real subtraction/multiplication ----

#[test]
fn test_mixed_real_subtraction() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 - 3.5");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 6.5).abs() < 0.001);
    }
    let rows = query_rows(&mut vm, "SELECT 10.5 - 3");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 7.5).abs() < 0.001);
    }
}

#[test]
fn test_mixed_real_multiplication() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 3 * 2.5");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 7.5).abs() < 0.001);
    }
    let rows = query_rows(&mut vm, "SELECT 2.5 * 3");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 7.5).abs() < 0.001);
    }
}

// ---- Mixed Real addition ----

#[test]
fn test_mixed_real_addition() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 + 2.5");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 12.5).abs() < 0.001);
    }
    let rows = query_rows(&mut vm, "SELECT 2.5 + 10");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 12.5).abs() < 0.001);
    }
}

// ---- Concat with non-string values ----

#[test]
fn test_concat_int_values() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 42 || 'abc'");
    assert_eq!(rows[0][0], Value::Text("42abc".into()));
}

// ---- NOT IN list ----

#[test]
fn test_not_in_list() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE val NOT IN (10, 30)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Integer(20));
}

// ---- NOT LIKE ----

#[test]
fn test_not_like_query() {
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

// ---- SELECT with column alias ----

#[test]
fn test_select_with_alias() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SELECT 1 + 2 AS total").unwrap();
    if let ExecResult::QueryResult { columns, rows } = result {
        assert_eq!(columns[0], "total");
        assert_eq!(rows[0][0], Value::Integer(3));
    }
}

// ---- Real-Real subtraction and multiplication ----

#[test]
fn test_real_real_subtraction() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5.5 - 2.5");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 3.0).abs() < 0.001);
    }
}

#[test]
fn test_real_real_multiplication() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 2.5 * 3.0");
    if let Value::Real(v) = &rows[0][0] {
        assert!((*v - 7.5).abs() < 0.001);
    }
}

// ---- Integer division ----

#[test]
fn test_integer_division() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 / 3");
    assert_eq!(rows[0][0], Value::Integer(3));
    let rows = query_rows(&mut vm, "SELECT 10 / 0");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- PK null on non-autoincrement table (line 164-168) ----

#[test]
fn test_insert_null_pk_non_autoincrement() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    // Insert with NULL PK on non-autoincrement table triggers fallback
    vm.execute_sql("INSERT INTO t1 VALUES (NULL, 'hello')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);
    // id should be auto-assigned
    if let Value::Integer(v) = &rows[0][0] {
        assert!(*v >= 1);
    }
}

// ---- COUNT(col) non-distinct in GROUP BY (lines 655-658) ----

#[test]
fn test_count_col_non_distinct_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', NULL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'B', 20)")
        .unwrap();

    // COUNT(val) without DISTINCT: counts non-null values per group
    // A has 1 non-null val, B has 1. HAVING COUNT(val) >= 1 => both
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING COUNT(val) >= 1 ORDER BY cat",
    );
    assert_eq!(rows.len(), 2);
}

// ---- MIN with NULL values in GROUP BY (lines 727, 733) ----

#[test]
fn test_min_with_nulls_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', NULL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 5)").unwrap();

    // MIN(val) should skip NULLs, so MIN(A) = 5
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MIN(val) = 5",
    );
    assert_eq!(rows.len(), 1);
}

// ---- MAX with NULL values in GROUP BY (lines 750, 758) ----

#[test]
fn test_max_with_nulls_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', NULL)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 20)")
        .unwrap();

    // MAX(val) should skip NULLs, so MAX(A) = 20
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MAX(val) = 20",
    );
    assert_eq!(rows.len(), 1);
}

// ---- INSERT many rows to trigger root page change (lines 207, 219-237) ----

#[test]
fn test_insert_triggers_root_page_change() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, data TEXT)")
        .unwrap();

    // Insert enough big rows to trigger a B-tree split and root page change
    let big_val = "X".repeat(200);
    for i in 1..=20 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, '{}')", i, big_val))
            .unwrap();
    }

    // Verify all rows are accessible after potential root page change
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 20);
}

// ---- EXPLAIN for CREATE TABLE (line 948 catch-all) ----

#[test]
fn test_explain_create_table() {
    let mut vm = VM::new_memory();
    let result = vm
        .execute_sql("EXPLAIN CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = result {
        assert!(!rows.is_empty());
    }
}

// ---- MIN/MAX comparison branches (lines 733, 758) ----

#[test]
fn test_min_multiple_values() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 20)")
        .unwrap();

    // MIN should find 10 (less-than branch)
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MIN(val) < 15",
    );
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_max_multiple_values() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'A', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 20)")
        .unwrap();

    // MAX should find 30 (greater-than branch), then 20 hits not-greater (current.clone())
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING MAX(val) > 25",
    );
    assert_eq!(rows.len(), 1);
}

// ---- Table-qualified column ref (lines 988-992) ----

#[test]
fn test_table_qualified_column_ref() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();

    let rows = query_rows(&mut vm, "SELECT t1.val FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
}

// ---- Non-grouped aggregate (COUNT/SUM/AVG/MIN/MAX without GROUP BY) ----

#[test]
fn test_aggregate_count_star_no_group() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    // Implicit aggregation: single row with total count
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_aggregate_sum_no_group() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    // Implicit aggregation: single row with total sum
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(30));
}

#[test]
fn test_aggregate_avg_no_group() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();

    // Implicit aggregation: single row with average
    let rows = query_rows(&mut vm, "SELECT AVG(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Real(15.0));
}

#[test]
fn test_aggregate_min_max_no_group() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 30)").unwrap();

    // Implicit aggregation: single row with min/max
    let rows = query_rows(&mut vm, "SELECT MIN(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(10));

    let rows = query_rows(&mut vm, "SELECT MAX(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(30));
}

// ---- TYPEOF for blob (line 1127) ----

#[test]
fn test_typeof_blob() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, data BLOB)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, X'ABCD')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT TYPEOF(data) FROM t1");
    assert_eq!(rows[0][0], Value::Text("blob".into()));
}

// ---- EXPLAIN with JOIN (lines 956-957) ----

#[test]
fn test_explain_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (a INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (b INTEGER PRIMARY KEY)")
        .unwrap();

    let result = vm
        .execute_sql("EXPLAIN SELECT * FROM t1 JOIN t2 ON t1.a = t2.b")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = result {
        assert!(!rows.is_empty());
    }
}

// ---- EXPLAIN with subquery (line 959) ----

#[test]
fn test_explain_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();

    let result = vm
        .execute_sql("EXPLAIN SELECT * FROM (SELECT id FROM t1) AS sub")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = result {
        assert!(!rows.is_empty());
    }
}

// ---- ORDER BY with equal values (line 337) ----

#[test]
fn test_order_by_with_equal_values() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'C', 20)")
        .unwrap();

    // ORDER BY val will have equal values (10, 10) - hits Equal branch
    let rows = query_rows(&mut vm, "SELECT cat FROM t1 ORDER BY val");
    assert_eq!(rows.len(), 3);
}

// ---- SUBSTR/REPLACE/TRIM with null ----

#[test]
fn test_substr_null_arg() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SUBSTR(NULL, 1, 2)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_replace_null_arg() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT REPLACE(NULL, 'a', 'b')");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_trim_null_arg() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT TRIM(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- Comparison with NULL values (lines 1257, 1264, 1271) ----

#[test]
fn test_comparison_with_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL < 5");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT NULL > 5");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT NULL = 5");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- UPDATE with many rows to trigger root change (lines 845-847) ----

#[test]
fn test_update_triggers_root_change() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, data TEXT)")
        .unwrap();

    let big_val = "Y".repeat(200);
    for i in 1..=20 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, 'x')", i))
            .unwrap();
    }
    // Update all rows with big data to potentially trigger root page change
    vm.execute_sql(&format!("UPDATE t1 SET data = '{}'", big_val))
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 20);
}

// ---- Division/modulo with non-numeric (lines 1303, 1314) ----

#[test]
fn test_division_non_numeric() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'abc' / 2");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_modulo_non_numeric() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'abc' % 2");
    assert_eq!(rows[0][0], Value::Null);
}

// ---- Comparison non-comparable types (lines 1257, etc) ----

#[test]
fn test_comparison_text_integer() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'abc' < 5");
    // Non-comparable types should return 0
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ---- HAVING with non-aggregate function (line 786) ----

#[test]
fn test_having_with_non_aggregate_function() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 20)")
        .unwrap();

    // HAVING with LENGTH (non-aggregate) -> falls through to eval_expr in eval_expr_with_aggregates
    let rows = query_rows(
        &mut vm,
        "SELECT cat FROM t1 GROUP BY cat HAVING LENGTH(cat) > 3 ORDER BY cat",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

// ==============================================================
// Aggregate tests 锟?implicit aggregation, nested, empty, DISTINCT
// ==============================================================

#[test]
fn test_aggregate_empty_table_count() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    // COUNT(*) on empty table should return 0
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_aggregate_empty_table_sum_avg_min_max() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    // SUM/AVG/MIN/MAX on empty table should return NULL
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT AVG(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT MIN(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT MAX(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_aggregate_nested_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    // 1 + COUNT(*) should be 3
    let rows = query_rows(&mut vm, "SELECT 1 + COUNT(*) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
    // SUM(val) * 2 should be 60
    let rows = query_rows(&mut vm, "SELECT SUM(val) * 2 FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(60));
}

#[test]
fn test_aggregate_abs_of_count() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    // ABS(COUNT(*)) 锟?aggregate nested in non-aggregate function
    let rows = query_rows(&mut vm, "SELECT ABS(COUNT(*)) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_aggregate_multiple_in_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();
    // Multiple aggregates in one SELECT
    let rows = query_rows(
        &mut vm,
        "SELECT COUNT(*), SUM(val), AVG(val), MIN(val), MAX(val) FROM t1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
    assert_eq!(rows[0][1], Value::Integer(60));
    assert_eq!(rows[0][2], Value::Real(20.0));
    assert_eq!(rows[0][3], Value::Integer(10));
    assert_eq!(rows[0][4], Value::Integer(30));
}

#[test]
fn test_aggregate_count_distinct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (4, 'C')").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(DISTINCT cat) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_aggregate_sum_with_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();
    // SUM should skip NULL
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(40));
    // COUNT(val) should skip NULL
    let rows = query_rows(&mut vm, "SELECT COUNT(val) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_aggregate_group_by_with_count_sum() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (4, 'B', 40)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT cat, COUNT(*), SUM(val) FROM t1 GROUP BY cat ORDER BY cat",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("A".into()));
    assert_eq!(rows[0][1], Value::Integer(2));
    assert_eq!(rows[0][2], Value::Integer(40));
    assert_eq!(rows[1][0], Value::Text("B".into()));
    assert_eq!(rows[1][1], Value::Integer(2));
    assert_eq!(rows[1][2], Value::Integer(60));
}

#[test]
fn test_aggregate_having_with_count() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT cat, COUNT(*) FROM t1 GROUP BY cat HAVING COUNT(*) > 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("A".into()));
    assert_eq!(rows[0][1], Value::Integer(2));
}

#[test]
fn test_aggregate_nested_in_group_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 30)")
        .unwrap();
    // SUM(val) + 100 锟?nested aggregate in GROUP BY projection
    let rows = query_rows(
        &mut vm,
        "SELECT cat, SUM(val) + 100 FROM t1 GROUP BY cat ORDER BY cat",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Integer(140)); // 10+30+100
    assert_eq!(rows[1][1], Value::Integer(120)); // 20+100
}

// ==============================================================
// CREATE INDEX 锟?post-insert query still works
// ==============================================================

#[test]
fn test_create_index_and_query_after() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 20)")
        .unwrap();
    // CREATE INDEX should succeed
    let result = vm
        .execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("Index created"));
        }
        _ => panic!("expected Ok result for CREATE INDEX"),
    }
    // Table should still work normally after index creation
    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY name");
    assert_eq!(rows.len(), 2);
    // Insert more data after index creation
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie', 30)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ==============================================================
// ABS overflow edge case
// ==============================================================

#[test]
fn test_abs_negative_large() {
    let mut vm = VM::new_memory();
    // ABS of large negative number
    let rows = query_rows(&mut vm, "SELECT ABS(-9223372036854775807)");
    assert_eq!(rows[0][0], Value::Integer(9223372036854775807));
    // ABS of small negative
    let rows = query_rows(&mut vm, "SELECT ABS(-42)");
    assert_eq!(rows[0][0], Value::Integer(42));
}

// ==============================================================
// Aggregate with InList / Between / negation
// ==============================================================

#[test]
fn test_aggregate_negation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    // -COUNT(*) should be -2
    let rows = query_rows(&mut vm, "SELECT -COUNT(*) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(-2));
}

#[test]
fn test_aggregate_is_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    // SUM(val) IS NOT NULL should be true (1)
    let rows = query_rows(&mut vm, "SELECT SUM(val) IS NOT NULL FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_aggregate_between() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    // COUNT(*) BETWEEN 1 AND 5 should be true
    let rows = query_rows(&mut vm, "SELECT COUNT(*) BETWEEN 1 AND 5 FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
    // COUNT(*) BETWEEN 5 AND 10 should be false
    let rows = query_rows(&mut vm, "SELECT COUNT(*) BETWEEN 5 AND 10 FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_aggregate_in_list() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();
    // COUNT(*) IN (1, 2, 3) should be true (COUNT=3)
    let rows = query_rows(&mut vm, "SELECT COUNT(*) IN (1, 2, 3) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
    // COUNT(*) IN (1, 2) should be false (COUNT=3)
    let rows = query_rows(&mut vm, "SELECT COUNT(*) IN (1, 2) FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ==============================================================
// Large insert + aggregate 锟?triggers B-tree splits under VM
// ==============================================================

#[test]
fn test_aggregate_after_many_inserts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=100 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(100));
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1");
    // SUM(10+20+...+1000) = 10*(1+2+...+100) = 10*5050 = 50500
    assert_eq!(rows[0][0], Value::Integer(50500));
    let rows = query_rows(&mut vm, "SELECT MIN(val) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(10));
    let rows = query_rows(&mut vm, "SELECT MAX(val) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(1000));
}

// ==============================================================
// Delete + aggregate 锟?verify consistency after deletes
// ==============================================================

#[test]
fn test_aggregate_after_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i))
            .unwrap();
    }
    vm.execute_sql("DELETE FROM t1 WHERE val > 5").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*), SUM(val) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(5));
    assert_eq!(rows[0][1], Value::Integer(15)); // 1+2+3+4+5
}

// ==============================================================
// Update + aggregate 锟?verify consistency after updates
// ==============================================================

#[test]
fn test_aggregate_after_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=5 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }
    vm.execute_sql("UPDATE t1 SET val = val + 100 WHERE id <= 3")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1");
    // Original: 10+20+30+40+50=150, after update: 110+120+130+40+50=450
    assert_eq!(rows[0][0], Value::Integer(450));
}

// ==============================================================
// INDEX tests 锟?creation, maintenance, accelerated queries
// ==============================================================

#[test]
fn test_index_basic_creation_and_query() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie', 30)")
        .unwrap();

    // Create index on name column
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Query using the indexed column
    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE name = 'Bob'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[0][1], Value::Text("Bob".into()));
    assert_eq!(rows[0][2], Value::Integer(20));
}

#[test]
fn test_index_accelerated_equality_query() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    for i in 1..=20 {
        let cat = if i % 3 == 0 {
            "X"
        } else if i % 3 == 1 {
            "Y"
        } else {
            "Z"
        };
        vm.execute_sql(&format!(
            "INSERT INTO t1 VALUES ({}, '{}', {})",
            i,
            cat,
            i * 10
        ))
        .unwrap();
    }

    vm.execute_sql("CREATE INDEX idx_cat ON t1 (cat)").unwrap();

    // WHERE cat = 'X' should use the index
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE cat = 'X' ORDER BY id");
    // i % 3 == 0: 3,6,9,12,15,18
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0][0], Value::Integer(3));
    assert_eq!(rows[5][0], Value::Integer(18));
}

#[test]
fn test_index_insert_maintains_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    // Create index BEFORE inserting more data
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Insert more data AFTER index creation 锟?should be reflected in index
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Alice')")
        .unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE name = 'Alice' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Bob'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_index_root_updates_after_many_post_index_inserts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, tag TEXT)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_tag ON t1 (tag)").unwrap();

    for i in 1..=1200 {
        let tag = if i % 2 == 0 { "EVEN" } else { "ODD" };
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, '{}')", i, tag))
            .unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE tag = 'EVEN'");
    assert_eq!(rows[0][0], Value::Integer(600));
}

#[test]
fn test_index_delete_maintains_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Alice')")
        .unwrap();

    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Delete one Alice
    vm.execute_sql("DELETE FROM t1 WHERE id = 1").unwrap();

    // Should only find one Alice now
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Alice'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_index_update_maintains_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Update Bob to Charlie
    vm.execute_sql("UPDATE t1 SET name = 'Charlie' WHERE id = 2")
        .unwrap();

    // Bob should be gone from index
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Bob'");
    assert_eq!(rows.len(), 0);

    // Charlie should be found
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Charlie'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_index_no_match() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Query for nonexistent value
    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE name = 'Nobody'");
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_index_equal_null_no_match() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, NULL)").unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = NULL");
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_index_on_integer_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i % 3))
            .unwrap();
    }
    vm.execute_sql("CREATE INDEX idx_val ON t1 (val)").unwrap();

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val = 0 ORDER BY id");
    // i % 3 == 0: 3,6,9
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(3));
    assert_eq!(rows[1][0], Value::Integer(6));
    assert_eq!(rows[2][0], Value::Integer(9));
}

#[test]
fn test_ddl_clears_index_cache_for_reused_index_name() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Warm the index cache for idx_name.
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Alice'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));

    // Reuse the same index name after DDL mutations.
    vm.execute_sql("DROP TABLE t1").unwrap();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE name = 'Bob'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_range_index_cache_stays_fresh_after_row_changes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON t1 (val)").unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    // Warm ordered range cache.
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val >= 20 ORDER BY id");
    assert_eq!(rows, vec![vec![Value::Integer(2)], vec![Value::Integer(3)]]);

    // Insert should update cache incrementally.
    vm.execute_sql("INSERT INTO t1 VALUES (4, 40)").unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val >= 20 ORDER BY id");
    assert_eq!(
        rows,
        vec![
            vec![Value::Integer(2)],
            vec![Value::Integer(3)],
            vec![Value::Integer(4)]
        ]
    );

    // Delete should evict from cache.
    vm.execute_sql("DELETE FROM t1 WHERE id = 3").unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val >= 20 ORDER BY id");
    assert_eq!(rows, vec![vec![Value::Integer(2)], vec![Value::Integer(4)]]);

    // Update path (delete old index entry + insert new) should keep cache consistent.
    vm.execute_sql("UPDATE t1 SET val = 5 WHERE id = 2")
        .unwrap();
    vm.execute_sql("UPDATE t1 SET val = 25 WHERE id = 1")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE val >= 20 ORDER BY id");
    assert_eq!(rows, vec![vec![Value::Integer(1)], vec![Value::Integer(4)]]);
}

#[test]
fn test_stmt_cache_fifo_eviction() {
    let mut vm = VM::new_memory();
    for i in 1..=300 {
        vm.execute_sql(&format!("SELECT {}", i)).unwrap();
    }

    assert!(vm.stmt_cache.len() <= 256);
    assert!(!vm.stmt_cache.contains_key("SELECT 1"));
    assert!(vm.stmt_cache.contains_key("SELECT 300"));
}

#[test]
fn test_index_with_aggregate() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'B', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'A', 30)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_cat ON t1 (cat)").unwrap();

    // Aggregate on indexed column filter
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM t1 WHERE cat = 'A'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(40));
}

#[test]
fn test_drop_table_cleans_indexes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    vm.execute_sql("DROP TABLE t1").unwrap();

    // Re-create table with same name 锟?should work cleanly
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Bob')").unwrap();
    let rows = query_rows(&mut vm, "SELECT name FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Bob".into()));
}

#[test]
fn test_index_large_dataset() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();

    // Insert 100 rows with 10 categories
    for i in 1..=100 {
        let cat = format!("cat{}", i % 10);
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, '{}', {})", i, cat, i))
            .unwrap();
    }

    vm.execute_sql("CREATE INDEX idx_cat ON t1 (cat)").unwrap();

    // Each category should have 10 rows
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE cat = 'cat0'");
    assert_eq!(rows[0][0], Value::Integer(10));

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE cat = 'cat5'");
    assert_eq!(rows[0][0], Value::Integer(10));

    // Non-existent category
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE cat = 'cat99'");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_multiple_indexes_on_same_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, city TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 'NYC')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 'LA')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Alice', 'LA')")
        .unwrap();

    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_city ON t1 (city)")
        .unwrap();

    // Query by name
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE name = 'Alice' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);

    // Query by city
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE city = 'LA' ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_index_backfills_existing_data() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    // Insert data BEFORE creating index
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Alice')")
        .unwrap();

    // Create index 锟?should backfill existing data
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Index-accelerated query should find pre-existing rows
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE name = 'Alice' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_index_reversed_equality() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    // Reversed equality: literal = column (should still use index)
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE 'Alice' = name");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ==============================================================
// ALTER TABLE tests
// ==============================================================

#[test]
fn test_alter_add_column_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("ALTER TABLE t1 ADD COLUMN age INTEGER")
        .unwrap();

    // Existing rows should have NULL for the new column
    let rows = query_rows(&mut vm, "SELECT id, name, age FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][2], Value::Null);
    assert_eq!(rows[1][2], Value::Null);

    // New inserts should work with the new column
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie', 30)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, name, age FROM t1 WHERE id = 3");
    assert_eq!(rows[0][2], Value::Integer(30));
}

#[test]
fn test_alter_add_column_default_backfills_existing_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("ALTER TABLE t1 ADD COLUMN score INTEGER DEFAULT 7")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT id, score FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Integer(7));
    assert_eq!(rows[1][1], Value::Integer(7));
}

#[test]
fn test_alter_add_column_without_keyword() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    // ADD without COLUMN keyword
    vm.execute_sql("ALTER TABLE t1 ADD val TEXT").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'hello')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT val FROM t1");
    assert_eq!(rows[0][0], Value::Text("hello".into()));
}

#[test]
fn test_alter_add_column_duplicate_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 ADD COLUMN name TEXT");
    assert!(result.is_err());
}

#[test]
fn test_alter_drop_column_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 20)")
        .unwrap();

    vm.execute_sql("ALTER TABLE t1 DROP COLUMN val").unwrap();

    // Should only have id, name now
    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_alter_drop_column_without_keyword() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    // DROP without COLUMN keyword
    vm.execute_sql("ALTER TABLE t1 DROP val").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0].len(), 2);
}

#[test]
fn test_alter_drop_pk_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 DROP COLUMN id");
    assert!(result.is_err());
}

#[test]
fn test_alter_drop_only_column_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 DROP COLUMN id");
    // Fails either because it's PK or only column
    assert!(result.is_err());
}

#[test]
fn test_alter_drop_nonexistent_column_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 DROP COLUMN xyz");
    assert!(result.is_err());
}

#[test]
fn test_alter_rename_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    vm.execute_sql("ALTER TABLE t1 RENAME TO t2").unwrap();

    // Old name should fail
    let result = vm.execute_sql("SELECT * FROM t1");
    assert!(result.is_err());

    // New name should work
    let rows = query_rows(&mut vm, "SELECT * FROM t2");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_alter_rename_table_conflict_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 RENAME TO t2");
    assert!(result.is_err());
}

#[test]
fn test_alter_rename_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    vm.execute_sql("ALTER TABLE t1 RENAME COLUMN name TO full_name")
        .unwrap();

    // Old column name should fail
    let result = vm.execute_sql("SELECT name FROM t1");
    assert!(result.is_err());

    // New column name should work
    let rows = query_rows(&mut vm, "SELECT full_name FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_alter_rename_column_without_keyword() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    // RENAME old TO new (without COLUMN keyword)
    vm.execute_sql("ALTER TABLE t1 RENAME name TO full_name")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT full_name FROM t1");
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_alter_rename_column_duplicate_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    let result = vm.execute_sql("ALTER TABLE t1 RENAME COLUMN name TO id");
    assert!(result.is_err());
}

#[test]
fn test_alter_add_then_insert_and_query() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();

    vm.execute_sql("ALTER TABLE t1 ADD COLUMN name TEXT")
        .unwrap();
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN val INTEGER")
        .unwrap();

    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob', 20)")
        .unwrap();
    vm.execute_sql("UPDATE t1 SET name = 'Alice', val = 10 WHERE id = 1")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Integer(10));
    assert_eq!(rows[1][1], Value::Text("Bob".into()));
    assert_eq!(rows[1][2], Value::Integer(20));
}

#[test]
fn test_alter_drop_column_cleans_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 10)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON t1 (val)").unwrap();

    // Dropping the indexed column should also drop the index
    vm.execute_sql("ALTER TABLE t1 DROP COLUMN val").unwrap();

    // Table should work with remaining columns
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_alter_rename_table_with_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (name)")
        .unwrap();

    vm.execute_sql("ALTER TABLE t1 RENAME TO t2").unwrap();

    // Index-accelerated query should still work after rename
    let rows = query_rows(&mut vm, "SELECT id FROM t2 WHERE name = 'Alice'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_alter_nonexistent_table_fails() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("ALTER TABLE nonexistent ADD COLUMN val INTEGER");
    assert!(result.is_err());
}

#[test]
fn test_alter_multiple_add_columns() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();

    // Add 5 columns sequentially
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN a TEXT").unwrap();
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN b INTEGER")
        .unwrap();
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN c REAL").unwrap();
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN d TEXT").unwrap();
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN e INTEGER")
        .unwrap();

    // Existing row should have NULLs for all new columns
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0].len(), 6); // id + 5 new
    assert_eq!(rows[0][0], Value::Integer(1));
    for val in &rows[0][1..6] {
        assert_eq!(*val, Value::Null);
    }

    // New insert with all columns
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'x', 42, 3.14, 'y', 99)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT a, b, c, d, e FROM t1 WHERE id = 2");
    assert_eq!(rows[0][0], Value::Text("x".into()));
    assert_eq!(rows[0][1], Value::Integer(42));
    assert_eq!(rows[0][4], Value::Integer(99));
}

#[test]
fn test_alter_add_then_drop_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    // Add then drop
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN temp INTEGER")
        .unwrap();
    vm.execute_sql("UPDATE t1 SET temp = 999 WHERE id = 1")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT temp FROM t1");
    assert_eq!(rows[0][0], Value::Integer(999));

    vm.execute_sql("ALTER TABLE t1 DROP COLUMN temp").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows[0].len(), 2); // id, name only
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_alter_drop_middle_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'x', 42, 'z')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'p', 99, 'q')")
        .unwrap();

    // Drop middle column 'b'
    vm.execute_sql("ALTER TABLE t1 DROP COLUMN b").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1 ORDER BY id");
    assert_eq!(rows[0].len(), 3); // id, a, c
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("x".into()));
    assert_eq!(rows[0][2], Value::Text("z".into()));
    assert_eq!(rows[1][0], Value::Integer(2));
    assert_eq!(rows[1][1], Value::Text("p".into()));
    assert_eq!(rows[1][2], Value::Text("q".into()));

    // Insert after column drop should work with new column layout
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'm', 'n')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT a, c FROM t1 WHERE id = 3");
    assert_eq!(rows[0][0], Value::Text("m".into()));
    assert_eq!(rows[0][1], Value::Text("n".into()));
}

#[test]
fn test_alter_large_dataset_add_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=50 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }

    vm.execute_sql("ALTER TABLE t1 ADD COLUMN label TEXT")
        .unwrap();

    // All 50 rows should have NULL for new column
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE label IS NULL");
    assert_eq!(rows[0][0], Value::Integer(50));

    // Update some rows
    vm.execute_sql("UPDATE t1 SET label = 'even' WHERE val % 20 = 0")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1 WHERE label = 'even'");
    assert_eq!(rows[0][0], Value::Integer(25));
}

#[test]
fn test_alter_large_dataset_drop_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)")
        .unwrap();
    for i in 1..=50 {
        vm.execute_sql(&format!(
            "INSERT INTO t1 VALUES ({}, {}, {})",
            i,
            i,
            i * 100
        ))
        .unwrap();
    }

    vm.execute_sql("ALTER TABLE t1 DROP COLUMN b").unwrap();

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(50));

    let rows = query_rows(&mut vm, "SELECT * FROM t1 WHERE id = 25");
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][1], Value::Integer(25));
}

#[test]
fn test_alter_rename_table_then_crud() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE old_name (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO old_name VALUES (1, 10)")
        .unwrap();

    vm.execute_sql("ALTER TABLE old_name RENAME TO new_name")
        .unwrap();

    // INSERT on new name
    vm.execute_sql("INSERT INTO new_name VALUES (2, 20)")
        .unwrap();
    // UPDATE on new name
    vm.execute_sql("UPDATE new_name SET val = 100 WHERE id = 1")
        .unwrap();
    // DELETE on new name
    vm.execute_sql("DELETE FROM new_name WHERE id = 2").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM new_name");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(100));
}

#[test]
fn test_alter_rename_column_then_query() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, old_col INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 42)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 99)").unwrap();

    vm.execute_sql("ALTER TABLE t1 RENAME COLUMN old_col TO new_col")
        .unwrap();

    // WHERE on new column name
    let rows = query_rows(&mut vm, "SELECT new_col FROM t1 WHERE new_col > 50");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(99));

    // ORDER BY new column name (must be in SELECT for current ORDER BY impl)
    let rows = query_rows(&mut vm, "SELECT id, new_col FROM t1 ORDER BY new_col DESC");
    assert_eq!(rows[0][0], Value::Integer(2));

    // UPDATE using new column name
    vm.execute_sql("UPDATE t1 SET new_col = new_col + 1 WHERE id = 1")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT new_col FROM t1 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Integer(43));
}

#[test]
fn test_alter_add_column_then_aggregate() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO t1 VALUES ({}, {})", i, i * 10))
            .unwrap();
    }

    vm.execute_sql("ALTER TABLE t1 ADD COLUMN bonus INTEGER")
        .unwrap();
    // Update even rows with bonus
    vm.execute_sql("UPDATE t1 SET bonus = 5 WHERE val % 20 = 0")
        .unwrap();

    // SUM of bonus: 5 rows with bonus=5 锟?25
    let rows = query_rows(&mut vm, "SELECT SUM(bonus) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(25));

    // COUNT non-null bonus
    let rows = query_rows(&mut vm, "SELECT COUNT(bonus) FROM t1");
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_alter_add_column_then_create_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    // Add column, then create index on it
    vm.execute_sql("ALTER TABLE t1 ADD COLUMN category TEXT")
        .unwrap();
    vm.execute_sql("UPDATE t1 SET category = 'A' WHERE id = 1")
        .unwrap();
    vm.execute_sql("UPDATE t1 SET category = 'B' WHERE id = 2")
        .unwrap();

    vm.execute_sql("CREATE INDEX idx_cat ON t1 (category)")
        .unwrap();

    // Index-accelerated query on the added column
    let rows = query_rows(&mut vm, "SELECT name FROM t1 WHERE category = 'A'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));

    // Insert after index creation
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie', 'A')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE category = 'A' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_alter_rename_column_with_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, old_name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON t1 (old_name)")
        .unwrap();

    // Rename the indexed column
    vm.execute_sql("ALTER TABLE t1 RENAME COLUMN old_name TO new_name")
        .unwrap();

    // Index-accelerated query should still work with new column name
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE new_name = 'Alice'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_alter_multiple_renames() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, a TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'hello')")
        .unwrap();

    // Rename column a 锟?b 锟?c
    vm.execute_sql("ALTER TABLE t1 RENAME COLUMN a TO b")
        .unwrap();
    vm.execute_sql("ALTER TABLE t1 RENAME COLUMN b TO c")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT c FROM t1");
    assert_eq!(rows[0][0], Value::Text("hello".into()));

    // Old names should fail
    assert!(vm.execute_sql("SELECT a FROM t1").is_err());
    assert!(vm.execute_sql("SELECT b FROM t1").is_err());
}

#[test]
fn test_alter_rename_table_twice() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();

    vm.execute_sql("ALTER TABLE t1 RENAME TO t2").unwrap();
    vm.execute_sql("ALTER TABLE t2 RENAME TO t3").unwrap();

    assert!(vm.execute_sql("SELECT * FROM t1").is_err());
    assert!(vm.execute_sql("SELECT * FROM t2").is_err());
    let rows = query_rows(&mut vm, "SELECT * FROM t3");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_alter_complex_workflow() {
    let mut vm = VM::new_memory();
    // Start with simple table
    vm.execute_sql("CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO employees VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO employees VALUES (2, 'Bob')")
        .unwrap();

    // Evolve the schema over time
    vm.execute_sql("ALTER TABLE employees ADD COLUMN dept TEXT")
        .unwrap();
    vm.execute_sql("ALTER TABLE employees ADD COLUMN salary INTEGER")
        .unwrap();
    vm.execute_sql("UPDATE employees SET dept = 'Eng', salary = 100 WHERE id = 1")
        .unwrap();
    vm.execute_sql("UPDATE employees SET dept = 'Sales', salary = 90 WHERE id = 2")
        .unwrap();

    vm.execute_sql("INSERT INTO employees VALUES (3, 'Charlie', 'Eng', 110)")
        .unwrap();

    // Add index on dept
    vm.execute_sql("CREATE INDEX idx_dept ON employees (dept)")
        .unwrap();

    // Rename table
    vm.execute_sql("ALTER TABLE employees RENAME TO staff")
        .unwrap();

    // Rename column
    vm.execute_sql("ALTER TABLE staff RENAME COLUMN dept TO department")
        .unwrap();

    // Query after all modifications
    let rows = query_rows(
        &mut vm,
        "SELECT name, department, salary FROM staff WHERE department = 'Eng' ORDER BY salary",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Integer(100));
    assert_eq!(rows[1][0], Value::Text("Charlie".into()));
    assert_eq!(rows[1][2], Value::Integer(110));

    // Aggregate after schema changes
    let rows = query_rows(&mut vm, "SELECT SUM(salary) FROM staff");
    assert_eq!(rows[0][0], Value::Integer(300));

    // Drop a column
    vm.execute_sql("ALTER TABLE staff DROP COLUMN salary")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM staff ORDER BY id");
    assert_eq!(rows[0].len(), 3); // id, name, department
    assert_eq!(rows[2][2], Value::Text("Eng".into()));
}

// ==============================================================
// Table-qualified column references in JOINs
// ==============================================================

#[test]
fn test_qualified_column_ref_in_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1, 'X')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 2, 'Y')")
        .unwrap();

    // Both tables have 'id' 锟?qualified reference should resolve correctly
    let rows = query_rows(
        &mut vm,
        "SELECT t1.id, t2.id, t1.name, t2.val FROM t1 JOIN t2 ON t1.id = t2.fk",
    );
    assert_eq!(rows.len(), 2);
    // t1.id should be 1, t2.id should be 10
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(10));
    assert_eq!(rows[0][2], Value::Text("Alice".into()));
    assert_eq!(rows[0][3], Value::Text("X".into()));
    assert_eq!(rows[1][0], Value::Integer(2));
    assert_eq!(rows[1][1], Value::Integer(20));
}

#[test]
fn test_qualified_column_ref_in_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 2)").unwrap();

    // WHERE using qualified column on left table
    let rows = query_rows(
        &mut vm,
        "SELECT t1.name FROM t1 JOIN t2 ON t1.id = t2.fk WHERE t1.id = 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

// ==============================================================
// SelectColumn::TableAllColumns (table.*) in execution
// ==============================================================

#[test]
fn test_table_all_columns_single_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice', 30)")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT t1.* FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Integer(30));
}

#[test]
fn test_table_all_columns_join_filters_correctly() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1, 'X')")
        .unwrap();

    // t1.* should return only t1's columns (id, name), not t2's
    let rows = query_rows(&mut vm, "SELECT t1.* FROM t1 JOIN t2 ON t1.id = t2.fk");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 2); // only t1's 2 columns
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));

    // t2.* should return only t2's columns (id, fk, val)
    let rows = query_rows(&mut vm, "SELECT t2.* FROM t1 JOIN t2 ON t1.id = t2.fk");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 3);
    assert_eq!(rows[0][0], Value::Integer(10));
    assert_eq!(rows[0][1], Value::Integer(1));
    assert_eq!(rows[0][2], Value::Text("X".into()));
}

#[test]
fn test_table_all_columns_both_sides_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();

    // t1.*, t2.* 锟?should return t1 cols then t2 cols
    let rows = query_rows(
        &mut vm,
        "SELECT t1.*, t2.* FROM t1 JOIN t2 ON t1.id = t2.fk",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 4); // t1: id,name + t2: id,fk
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Integer(10));
    assert_eq!(rows[0][3], Value::Integer(1));
}

#[test]
fn test_table_all_columns_mixed_with_expressions() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1, 'X')")
        .unwrap();

    // Mix t1.* with explicit t2 columns
    let rows = query_rows(
        &mut vm,
        "SELECT t1.*, t2.val FROM t1 JOIN t2 ON t1.id = t2.fk",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].len(), 3); // t1: id,name + t2.val
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[0][2], Value::Text("X".into()));
}

// ==============================================================
// Subquery execution: scalar, IN, EXISTS
// ==============================================================

#[test]
fn test_scalar_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    // Scalar subquery returning max
    let rows = query_rows(&mut vm, "SELECT (SELECT MAX(val) FROM t1)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(30));
}

#[test]
fn test_scalar_subquery_in_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    // WHERE val > (SELECT AVG(val) FROM t1) 锟?avg is 20, so only val=30 qualifies
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE val > (SELECT AVG(val) FROM t1)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_scalar_subquery_empty_returns_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();

    // Empty table 锟?scalar subquery returns NULL
    let rows = query_rows(&mut vm, "SELECT (SELECT MAX(id) FROM t1)");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 3)").unwrap();

    // id IN (SELECT fk FROM t2) 锟?matches 1 and 3
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE id IN (SELECT fk FROM t2) ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
    assert_eq!(rows[1][0], Value::Text("Charlie".into()));
}

#[test]
fn test_not_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie')")
        .unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (20, 3)").unwrap();

    // id NOT IN (SELECT fk FROM t2) 锟?only Bob (id=2)
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE id NOT IN (SELECT fk FROM t2)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Bob".into()));
}

#[test]
fn test_exists_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY, fk INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t2 VALUES (10, 1)").unwrap();

    // EXISTS: t2 has rows 锟?true for all t1 rows
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE EXISTS (SELECT 1 FROM t2)",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_not_exists_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    // NOT EXISTS on empty table 锟?true
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));

    // Insert into t2 锟?NOT EXISTS becomes false
    vm.execute_sql("INSERT INTO t2 VALUES (1)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE NOT EXISTS (SELECT 1 FROM t2)",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_in_subquery_empty_result() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE t2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    // IN on empty subquery 锟?no matches
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM t1 WHERE id IN (SELECT id FROM t2)",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_not_not_execution() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 0)").unwrap();

    // NOT NOT val: 1 锟?NOT NOT 1 锟?1 (truthy), 0 锟?NOT NOT 0 锟?0 (falsy)
    let rows = query_rows(&mut vm, "SELECT id FROM t1 WHERE NOT NOT val");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}
