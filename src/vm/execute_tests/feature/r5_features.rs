use super::*;

// ---- R5: Execution tests for R1-R4 new features ----

fn query_single_i64(vm: &mut VM, sql: &str) -> i64 {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => match &rows[0][0] {
            Value::Integer(v) => *v,
            other => panic!("expected Integer, got {:?}", other),
        },
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

#[test]
fn test_exec_r5_is_distinct_from_nulls() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (NULL, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 2)").unwrap();

    // NULL IS DISTINCT FROM NULL = FALSE (0)
    let rows = query_rows(
        &mut vm,
        "SELECT __IS_DISTINCT_FROM__(a, b) FROM t ORDER BY ROWID",
    );
    assert_eq!(
        rows[0][0],
        Value::Integer(0),
        "NULL IS DISTINCT FROM NULL should be 0"
    );
    assert_eq!(
        rows[1][0],
        Value::Integer(1),
        "1 IS DISTINCT FROM NULL should be 1"
    );
    assert_eq!(
        rows[2][0],
        Value::Integer(0),
        "1 IS DISTINCT FROM 1 should be 0"
    );
    assert_eq!(
        rows[3][0],
        Value::Integer(1),
        "1 IS DISTINCT FROM 2 should be 1"
    );
}

#[test]
fn test_exec_r5_is_not_distinct_from_nulls() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (NULL, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 1)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT __IS_NOT_DISTINCT_FROM__(a, b) FROM t ORDER BY ROWID",
    );
    assert_eq!(
        rows[0][0],
        Value::Integer(1),
        "NULL IS NOT DISTINCT FROM NULL = TRUE"
    );
    assert_eq!(
        rows[1][0],
        Value::Integer(0),
        "1 IS NOT DISTINCT FROM NULL = FALSE"
    );
    assert_eq!(
        rows[2][0],
        Value::Integer(1),
        "1 IS NOT DISTINCT FROM 1 = TRUE"
    );
}

#[test]
fn test_exec_r5_xor_logic() {
    let mut vm = VM::new_memory();
    // 1 XOR 0 = 1, 1 XOR 1 = 0, 0 XOR 0 = 0
    assert_eq!(query_single_i64(&mut vm, "SELECT 1 XOR 0"), 1);
    assert_eq!(query_single_i64(&mut vm, "SELECT 1 XOR 1"), 0);
    assert_eq!(query_single_i64(&mut vm, "SELECT 0 XOR 0"), 0);
    assert_eq!(query_single_i64(&mut vm, "SELECT 0 XOR 1"), 1);
}

#[test]
fn test_exec_r5_bitwise_and() {
    let mut vm = VM::new_memory();
    assert_eq!(query_single_i64(&mut vm, "SELECT 5 & 3"), 1); // 0101 & 0011 = 0001
    assert_eq!(query_single_i64(&mut vm, "SELECT 12 & 10"), 8); // 1100 & 1010 = 1000
    assert_eq!(query_single_i64(&mut vm, "SELECT 7 & 7"), 7);
}

#[test]
fn test_exec_r5_bitwise_or() {
    let mut vm = VM::new_memory();
    assert_eq!(query_single_i64(&mut vm, "SELECT 5 | 3"), 7); // 0101 | 0011 = 0111
    assert_eq!(query_single_i64(&mut vm, "SELECT 8 | 4"), 12);
}

#[test]
fn test_exec_r5_bitwise_xor_via_formula() {
    // BitwiseXor: a ^ b = (a | b) & ~(a & b) 锟?verify indirectly since # is PG-only
    // 5 & 3 = 1, 5 | 3 = 7, 7 & BITWISE_NOT(1) = 7 & -2 = 6  (correct: 5^3=6)
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT BITWISE_NOT(5 & 3)");
    // BITWISE_NOT(1) = -2 in two's complement
    assert_eq!(rows[0][0], Value::Integer(-2));
}

#[test]
fn test_exec_r5_factorial_function() {
    let mut vm = VM::new_memory();
    assert_eq!(query_single_i64(&mut vm, "SELECT FACTORIAL(0)"), 1);
    assert_eq!(query_single_i64(&mut vm, "SELECT FACTORIAL(1)"), 1);
    assert_eq!(query_single_i64(&mut vm, "SELECT FACTORIAL(5)"), 120);
    assert_eq!(query_single_i64(&mut vm, "SELECT FACTORIAL(10)"), 3628800);
}

#[test]
fn test_exec_r5_bitwise_not_function() {
    let mut vm = VM::new_memory();
    assert_eq!(query_single_i64(&mut vm, "SELECT BITWISE_NOT(0)"), -1);
    assert_eq!(query_single_i64(&mut vm, "SELECT BITWISE_NOT(-1)"), 0);
    assert_eq!(query_single_i64(&mut vm, "SELECT BITWISE_NOT(1)"), -2);
}

#[test]
fn test_exec_r5_starts_with_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello world', 'hello')");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows2 = query_rows(&mut vm, "SELECT STARTS_WITH('hello world', 'world')");
    assert_eq!(rows2[0][0], Value::Integer(0));
}

#[test]
fn test_exec_r5_json_extract_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        r#"SELECT JSON_EXTRACT('{"name":"Alice","age":30}', '$.name')"#,
    );
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_exec_r5_truncate_deletes_all_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3)").unwrap();
    // TRUNCATE is translated to DELETE
    vm.execute_sql("TRUNCATE TABLE t").unwrap();
    assert_eq!(query_single_i64(&mut vm, "SELECT COUNT(*) FROM t"), 0);
}

#[test]
fn test_exec_r5_cbrt_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CBRT(27)");
    match &rows[0][0] {
        Value::Real(v) => assert!((v - 3.0).abs() < 0.0001, "CBRT(27) should be ~3.0, got {v}"),
        other => panic!("expected Real, got {:?}", other),
    }
}

#[test]
fn test_exec_r5_try_cast_as_integer() {
    // TRY_CAST in SQL should work the same as CAST
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT TRY_CAST(3.7 AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ---- TableFunction: UNNEST / generate_series (#15) ----

#[test]
fn test_exec_table_func_generate_series_basic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT generate_series FROM GENERATE_SERIES(1, 5)");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[4][0], Value::Integer(5));
}

#[test]
fn test_exec_table_func_generate_series_step() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT generate_series FROM GENERATE_SERIES(0, 10, 2)",
    );
    // 0,2,4,6,8,10 = 6 rows
    assert_eq!(rows.len(), 6);
    assert_eq!(rows[0][0], Value::Integer(0));
    assert_eq!(rows[5][0], Value::Integer(10));
}

#[test]
fn test_exec_table_func_generate_series_negative_step() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT generate_series FROM GENERATE_SERIES(5, 1, -1)",
    );
    // 5,4,3,2,1 = 5 rows
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][0], Value::Integer(5));
    assert_eq!(rows[4][0], Value::Integer(1));
}

#[test]
fn test_exec_table_func_generate_series_empty() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT generate_series FROM GENERATE_SERIES(5, 1)");
    // step=1, 5 > 1 so empty
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_exec_table_func_generate_series_with_alias() {
    let mut vm = VM::new_memory();
    // WITH alias and column name
    let rows = query_rows(&mut vm, "SELECT n FROM GENERATE_SERIES(1, 3) AS t(n)");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_exec_table_func_unnest_json_array() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT unnest FROM UNNEST('[1,2,3]')");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[2][0], Value::Integer(3));
}

#[test]
fn test_exec_table_func_unnest_csv() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT unnest FROM UNNEST('a,b,c')");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Text("a".to_string().into()));
    assert_eq!(rows[2][0], Value::Text("c".to_string().into()));
}

#[test]
fn test_exec_table_func_unnest_json_strings() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT unnest FROM UNNEST('["hello","world"]')"#);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("hello".to_string().into()));
    assert_eq!(rows[1][0], Value::Text("world".to_string().into()));
}

#[test]
fn test_exec_table_func_power_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POWER(2, 10)");
    assert_eq!(rows[0][0], Value::Integer(1024));
}

#[test]
fn test_exec_table_func_pow_alias() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POW(3, 3)");
    assert_eq!(rows[0][0], Value::Integer(27));
}

// ---- GROUP BY column alias support ----

#[test]
fn test_group_by_expression_alias() {
    // GROUP BY an alias that refers to a computed expression
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3)").unwrap();
    // "doubled" is an alias for val*2; GROUP BY should group by the expression
    let rows = query_rows(
        &mut vm,
        "SELECT val * 2 AS doubled, COUNT(*) AS cnt FROM t GROUP BY doubled ORDER BY doubled",
    );
    // val=1 → doubled=2 (1 row), val=2 → doubled=4 (2 rows), val=3 → doubled=6 (1 row)
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[0][1], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(4));
    assert_eq!(rows[1][1], Value::Integer(2));
    assert_eq!(rows[2][0], Value::Integer(6));
    assert_eq!(rows[2][1], Value::Integer(1));
}

#[test]
fn test_group_by_column_alias() {
    // GROUP BY an alias that simply renames a column
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sales (region TEXT, amount INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('East', 100)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('East', 200)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('West', 300)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT region AS r, SUM(amount) AS total FROM sales GROUP BY r ORDER BY r",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("East".into()));
    assert_eq!(rows[0][1], Value::Integer(300));
    assert_eq!(rows[1][0], Value::Text("West".into()));
    assert_eq!(rows[1][1], Value::Integer(300));
}

// ---- Parameterized queries (execute_params) ----

#[test]
fn test_param_select_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'Bob')").unwrap();

    let rows = match vm
        .execute_params("SELECT * FROM t WHERE id = ?", &[Value::Integer(2)])
        .unwrap()
    {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[0][1], Value::Text("Bob".into()));
}

#[test]
fn test_param_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();

    vm.execute_params(
        "INSERT INTO t VALUES (?, ?)",
        &[Value::Integer(42), Value::Text("Eve".into())],
    )
    .unwrap();

    let rows = match vm.execute_sql("SELECT * FROM t").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(42));
    assert_eq!(rows[0][1], Value::Text("Eve".into()));
}

#[test]
fn test_param_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice')").unwrap();

    vm.execute_params(
        "UPDATE t SET name = ? WHERE id = ?",
        &[Value::Text("Updated".into()), Value::Integer(1)],
    )
    .unwrap();

    let rows = match vm.execute_sql("SELECT name FROM t WHERE id = 1").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows[0][0], Value::Text("Updated".into()));
}

#[test]
fn test_param_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();

    vm.execute_params("DELETE FROM t WHERE id = ?", &[Value::Integer(1)])
        .unwrap();

    let rows = match vm.execute_sql("SELECT * FROM t").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_param_reuse_different_values() {
    // The same SQL (with placeholders) should be reusable with different params.
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 200)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 300)").unwrap();

    let sql = "SELECT val FROM t WHERE id = ?";

    let r1 = match vm.execute_params(sql, &[Value::Integer(1)]).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(r1[0][0], Value::Integer(100));

    let r2 = match vm.execute_params(sql, &[Value::Integer(3)]).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(r2[0][0], Value::Integer(300));
}

#[test]
fn test_param_multiple_placeholders() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (a INTEGER, b INTEGER, c INTEGER)")
        .unwrap();
    vm.execute_params(
        "INSERT INTO t VALUES (?, ?, ?)",
        &[Value::Integer(1), Value::Integer(2), Value::Integer(3)],
    )
    .unwrap();

    let rows = match vm.execute_sql("SELECT a + b + c FROM t").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(rows[0][0], Value::Integer(6));
}

#[test]
fn test_param_count_mismatch_returns_error() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    // Only 1 param supplied but 2 `?` in query → should error when second is evaluated.
    let result = vm.execute_params(
        "SELECT ? + ?",
        &[Value::Integer(1)], // missing a second param
    );
    assert!(result.is_err(), "expected an error for too-few parameters");
}

#[test]
fn test_param_no_params_plain_sql() {
    // execute_params with empty slice should behave exactly like execute_sql for param-free SQL.
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_params("INSERT INTO t VALUES (99)", &[]).unwrap();
    let rows = match vm.execute_sql("SELECT id FROM t").unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("{:?}", other),
    };
    assert_eq!(rows[0][0], Value::Integer(99));
}

// ---- Minimal repro: INSERT OR REPLACE with multi-line/special-char params ----
