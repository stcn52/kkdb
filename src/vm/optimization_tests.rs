// Tests for Q3 (COUNT(*) fast path), Q6 (IN subquery rewrite), L1 (FK constraints)
// Included via #[path = "optimization_tests.rs"] inside execute.rs → super::* = execute module.

use super::*;

fn qrows(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

// ── Q3: COUNT(*) Fast Path ─────────────────────────────────────────────────

#[test]
fn test_q3_count_star_empty() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM t");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_q3_count_star_three_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE things (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("INSERT INTO things VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO things VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO things VALUES (3, 'c')").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM things");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_q3_count_star_column_name() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    match vm.execute_sql("SELECT COUNT(*) FROM t").unwrap() {
        ExecResult::QueryResult { columns, .. } => assert_eq!(columns[0], "COUNT(*)"),
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

/// WHERE present → fast path bypassed, result still correct
#[test]
fn test_q3_count_star_with_where_fallback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    for i in 1..=10i64 {
        vm.execute_sql(&format!("INSERT INTO nums VALUES ({i}, {i})")).unwrap();
    }
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM nums WHERE v > 5");
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_q3_count_star_after_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 30)").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 2").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM t");
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ── Q6: Non-Correlated IN (subquery) Rewrite ──────────────────────────────

#[test]
fn test_q6_in_subquery_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE orders (id INTEGER PRIMARY KEY, uid INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE active (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("INSERT INTO active VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO active VALUES (3)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (2, 2)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (3, 3)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (4, 1)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM orders WHERE uid IN (SELECT id FROM active)");
    let mut ids: Vec<i64> = rows.iter().map(|r| {
        if let Value::Integer(v) = r[0] { v } else { panic!("expected int") }
    }).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3, 4]);
}

#[test]
fn test_q6_not_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("CREATE TABLE excl (id INTEGER PRIMARY KEY)").unwrap();
    for i in 1..=5i64 {
        vm.execute_sql(&format!("INSERT INTO items VALUES ({i})")).unwrap();
    }
    vm.execute_sql("INSERT INTO excl VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO excl VALUES (4)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM items WHERE id NOT IN (SELECT id FROM excl)");
    let mut ids: Vec<i64> = rows.iter().map(|r| {
        if let Value::Integer(v) = r[0] { v } else { panic!("expected int") }
    }).collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3, 5]);
}

#[test]
fn test_q6_in_subquery_empty_returns_nothing() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("CREATE TABLE empty_src (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM t WHERE id IN (SELECT id FROM empty_src)");
    assert!(rows.is_empty(), "IN on empty subquery must return nothing");
}

/// Result of IN (subquery) must equal IN (literal list)
#[test]
fn test_q6_in_subquery_matches_in_list() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE products (id INTEGER PRIMARY KEY, price INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE featured (id INTEGER PRIMARY KEY)").unwrap();
    for i in 1..=6i64 {
        vm.execute_sql(&format!("INSERT INTO products VALUES ({i}, {})", i * 10)).unwrap();
    }
    vm.execute_sql("INSERT INTO featured VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO featured VALUES (5)").unwrap();
    let mut prices_sq: Vec<i64> = qrows(&mut vm, "SELECT price FROM products WHERE id IN (SELECT id FROM featured)")
        .iter().map(|r| if let Value::Integer(v) = r[0] { v } else { panic!() }).collect();
    let mut prices_list: Vec<i64> = qrows(&mut vm, "SELECT price FROM products WHERE id IN (2, 5)")
        .iter().map(|r| if let Value::Integer(v) = r[0] { v } else { panic!() }).collect();
    prices_sq.sort_unstable();
    prices_list.sort_unstable();
    assert_eq!(prices_sq, prices_list);
}

// ── L1: Foreign Key REFERENCES Constraints ────────────────────────────────

#[test]
fn test_l1_fk_insert_valid_ok() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dept (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE emp (id INTEGER PRIMARY KEY, dept_id INTEGER REFERENCES dept(id))").unwrap();
    vm.execute_sql("INSERT INTO dept VALUES (1, 'Eng')").unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (1, 1)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM emp");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l1_fk_insert_invalid_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dept (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE emp (id INTEGER PRIMARY KEY, dept_id INTEGER REFERENCES dept(id))").unwrap();
    vm.execute_sql("INSERT INTO dept VALUES (1, 'HR')").unwrap();
    // dept_id=99 does not exist → must fail
    let err = vm.execute_sql("INSERT INTO emp VALUES (2, 99)");
    assert!(err.is_err(), "Expected FK constraint violation but got Ok");
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.to_ascii_lowercase().contains("foreign key") || msg.to_ascii_lowercase().contains("constraint"),
        "Error must mention constraint violation, got: {msg}"
    );
}

#[test]
fn test_l1_fk_null_value_bypasses_check() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cats (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("CREATE TABLE kittens (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES cats(id))").unwrap();
    // NULL FK bypasses check per SQL standard
    vm.execute_sql("INSERT INTO kittens VALUES (1, NULL)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM kittens");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l1_fk_multiple_valid_inserts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE projects (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("CREATE TABLE tasks (id INTEGER PRIMARY KEY, proj_id INTEGER REFERENCES projects(id))").unwrap();
    vm.execute_sql("INSERT INTO projects VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO projects VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO tasks VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO tasks VALUES (2, 2)").unwrap();
    vm.execute_sql("INSERT INTO tasks VALUES (3, 1)").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM tasks");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_l1_table_without_fk_unaffected() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE free (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO free VALUES (1, 42)").unwrap();
    vm.execute_sql("INSERT INTO free VALUES (2, 99)").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM free");
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ── L2: CHECK Constraints ──────────────────────────────────────────────────

#[test]
fn test_l2_check_simple_pass() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pos (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > 0))").unwrap();
    vm.execute_sql("INSERT INTO pos VALUES (1, 10)").unwrap();
    let rows = qrows(&mut vm, "SELECT val FROM pos");
    assert_eq!(rows[0][0], Value::Integer(10));
}

#[test]
fn test_l2_check_simple_fail() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pos (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > 0))").unwrap();
    let err = vm.execute_sql("INSERT INTO pos VALUES (1, -5)");
    assert!(err.is_err(), "Expected CHECK constraint violation but got Ok");
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.to_ascii_lowercase().contains("check") || msg.to_ascii_lowercase().contains("constraint"),
        "Error must mention CHECK, got: {msg}"
    );
}

#[test]
fn test_l2_check_boundary_equal() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER CHECK (score >= 0 AND score <= 100))").unwrap();
    vm.execute_sql("INSERT INTO scores VALUES (1, 0)").unwrap();
    vm.execute_sql("INSERT INTO scores VALUES (2, 100)").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM scores");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_l2_check_boundary_violation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER CHECK (score >= 0 AND score <= 100))").unwrap();
    let err = vm.execute_sql("INSERT INTO scores VALUES (1, 101)");
    assert!(err.is_err(), "score=101 should violate CHECK (score <= 100)");
}

#[test]
fn test_l2_check_null_passes_through() {
    let mut vm = VM::new_memory();
    // Per SQL standard, NULL in CHECK expression evaluates to UNKNOWN → passes
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, rating INTEGER CHECK (rating > 3))").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, NULL)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM t");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l2_table_level_check() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE range_t (id INTEGER PRIMARY KEY, lo INTEGER, hi INTEGER, CHECK (lo < hi))").unwrap();
    vm.execute_sql("INSERT INTO range_t VALUES (1, 1, 10)").unwrap();
    let err = vm.execute_sql("INSERT INTO range_t VALUES (2, 10, 5)");
    assert!(err.is_err(), "lo=10, hi=5 violates CHECK (lo < hi)");
}

// ── O1: Column Statistics (ANALYZE TABLE) ─────────────────────────────────

#[test]
fn test_o1_analyze_basic_stats() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (3, 30)").unwrap();
    let r = vm.execute_sql("ANALYZE TABLE nums").unwrap();
    let msg = match r {
        ExecResult::Ok { message } => message,
        other => panic!("expected Ok, got {:?}", other),
    };
    assert!(msg.contains("3 rows") || msg.contains("3"), "should report 3 rows: {msg}");

    // Stats should now be accessible in the schema
    let ts = vm.schema.get_table("nums").unwrap();
    let id_stats = ts.columns[0].stats.as_ref().unwrap();
    assert_eq!(id_stats.total_count, 3);
    assert_eq!(id_stats.ndv, 3);
    assert_eq!(id_stats.null_count, 0);
}

#[test]
fn test_o1_analyze_null_count() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, optional INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, NULL)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, NULL)").unwrap();
    vm.execute_sql("ANALYZE TABLE t").unwrap();
    let ts = vm.schema.get_table("t").unwrap();
    let col_stats = ts.columns[1].stats.as_ref().unwrap();
    assert_eq!(col_stats.null_count, 2);
    assert_eq!(col_stats.ndv, 1);
}

#[test]
fn test_o1_analyze_min_max() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER)").unwrap();
    for i in [5, 1, 9, 3, 7i64] {
        vm.execute_sql(&format!("INSERT INTO scores VALUES ({i}, {i})")).unwrap();
    }
    vm.execute_sql("ANALYZE TABLE scores").unwrap();
    let ts = vm.schema.get_table("scores").unwrap();
    let s = ts.columns[1].stats.as_ref().unwrap();
    assert_eq!(s.min, Some(Value::Integer(1)));
    assert_eq!(s.max, Some(Value::Integer(9)));
}

#[test]
fn test_o1_analyze_empty_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE empty (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("ANALYZE TABLE empty").unwrap();
    let ts = vm.schema.get_table("empty").unwrap();
    let id_stats = ts.columns[0].stats.as_ref().unwrap();
    assert_eq!(id_stats.total_count, 0);
    assert_eq!(id_stats.ndv, 0);
    assert!(id_stats.min.is_none());
    assert!(id_stats.max.is_none());
}

// ── O2: Cost-Based Optimizer (CBO) ────────────────────────────────────────

/// Helper: set up a table with an index, insert rows, run ANALYZE, then query
fn cbo_setup(vm: &mut VM, n: usize) {
    vm.execute_sql("CREATE TABLE cbo_t (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("CREATE INDEX idx_v ON cbo_t (v)").unwrap();
    for i in 1..=(n as i64) {
        vm.execute_sql(&format!("INSERT INTO cbo_t VALUES ({i}, {i})")).unwrap();
    }
    vm.execute_sql("ANALYZE TABLE cbo_t").unwrap();
}

/// Eq on PK-like NDV column: selectivity=1/N, index wins → result correct
#[test]
fn test_o2_cbo_eq_result_correct() {
    let mut vm = VM::new_memory();
    cbo_setup(&mut vm, 100);
    let rows = qrows(&mut vm, "SELECT v FROM cbo_t WHERE v = 42");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(42));
}

/// Range query covering 99% rows: CBO should prefer seq scan, result still correct
#[test]
fn test_o2_cbo_low_selectivity_result_correct() {
    let mut vm = VM::new_memory();
    cbo_setup(&mut vm, 100);
    // v > 1 matches 99/100 rows — index not beneficial
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM cbo_t WHERE v > 1");
    assert_eq!(rows[0][0], Value::Integer(99));
}

/// BETWEEN with narrow range: selectivity ~ 10%, index wins
#[test]
fn test_o2_cbo_between_result_correct() {
    let mut vm = VM::new_memory();
    cbo_setup(&mut vm, 100);
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM cbo_t WHERE v BETWEEN 1 AND 10");
    assert_eq!(rows[0][0], Value::Integer(10));
}

/// Without ANALYZE, no stats → default selectivity 0.1 → index used (correctness check)
#[test]
fn test_o2_cbo_no_stats_result_correct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nostats (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("CREATE INDEX idx_ns ON nostats (v)").unwrap();
    for i in 1..=20i64 {
        vm.execute_sql(&format!("INSERT INTO nostats VALUES ({i}, {i})")).unwrap();
    }
    // No ANALYZE — falls back to default selectivity
    let rows = qrows(&mut vm, "SELECT v FROM nostats WHERE v = 7");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(7));
}
