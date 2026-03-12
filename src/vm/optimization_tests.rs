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
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM t");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_q3_count_star_three_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE things (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO things VALUES (1, 'a')")
        .unwrap();
    vm.execute_sql("INSERT INTO things VALUES (2, 'b')")
        .unwrap();
    vm.execute_sql("INSERT INTO things VALUES (3, 'c')")
        .unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM things");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_q3_count_star_column_name() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    match vm.execute_sql("SELECT COUNT(*) FROM t").unwrap() {
        ExecResult::QueryResult { columns, .. } => assert_eq!(columns[0], "COUNT(*)"),
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

/// WHERE present → fast path bypassed, result still correct
#[test]
fn test_q3_count_star_with_where_fallback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    for i in 1..=10i64 {
        vm.execute_sql(&format!("INSERT INTO nums VALUES ({i}, {i})"))
            .unwrap();
    }
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM nums WHERE v > 5");
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_q3_count_star_after_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
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
    vm.execute_sql("CREATE TABLE orders (id INTEGER PRIMARY KEY, uid INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE active (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO active VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO active VALUES (3)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (2, 2)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (3, 3)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (4, 1)").unwrap();
    let rows = qrows(
        &mut vm,
        "SELECT id FROM orders WHERE uid IN (SELECT id FROM active)",
    );
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| {
            if let Value::Integer(v) = r[0] {
                v
            } else {
                panic!("expected int")
            }
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3, 4]);
}

#[test]
fn test_q6_not_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE excl (id INTEGER PRIMARY KEY)")
        .unwrap();
    for i in 1..=5i64 {
        vm.execute_sql(&format!("INSERT INTO items VALUES ({i})"))
            .unwrap();
    }
    vm.execute_sql("INSERT INTO excl VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO excl VALUES (4)").unwrap();
    let rows = qrows(
        &mut vm,
        "SELECT id FROM items WHERE id NOT IN (SELECT id FROM excl)",
    );
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| {
            if let Value::Integer(v) = r[0] {
                v
            } else {
                panic!("expected int")
            }
        })
        .collect();
    ids.sort_unstable();
    assert_eq!(ids, vec![1, 3, 5]);
}

#[test]
fn test_q6_in_subquery_empty_returns_nothing() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE empty_src (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    let rows = qrows(
        &mut vm,
        "SELECT id FROM t WHERE id IN (SELECT id FROM empty_src)",
    );
    assert!(rows.is_empty(), "IN on empty subquery must return nothing");
}

/// Result of IN (subquery) must equal IN (literal list)
#[test]
fn test_q6_in_subquery_matches_in_list() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE products (id INTEGER PRIMARY KEY, price INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE featured (id INTEGER PRIMARY KEY)")
        .unwrap();
    for i in 1..=6i64 {
        vm.execute_sql(&format!("INSERT INTO products VALUES ({i}, {})", i * 10))
            .unwrap();
    }
    vm.execute_sql("INSERT INTO featured VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO featured VALUES (5)").unwrap();
    let mut prices_sq: Vec<i64> = qrows(
        &mut vm,
        "SELECT price FROM products WHERE id IN (SELECT id FROM featured)",
    )
    .iter()
    .map(|r| {
        if let Value::Integer(v) = r[0] {
            v
        } else {
            panic!()
        }
    })
    .collect();
    let mut prices_list: Vec<i64> = qrows(&mut vm, "SELECT price FROM products WHERE id IN (2, 5)")
        .iter()
        .map(|r| {
            if let Value::Integer(v) = r[0] {
                v
            } else {
                panic!()
            }
        })
        .collect();
    prices_sq.sort_unstable();
    prices_list.sort_unstable();
    assert_eq!(prices_sq, prices_list);
}

// ── L1: Foreign Key REFERENCES Constraints ────────────────────────────────

#[test]
fn test_l1_fk_insert_valid_ok() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dept (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql(
        "CREATE TABLE emp (id INTEGER PRIMARY KEY, dept_id INTEGER REFERENCES dept(id))",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO dept VALUES (1, 'Eng')")
        .unwrap();
    vm.execute_sql("INSERT INTO emp VALUES (1, 1)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM emp");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l1_fk_insert_invalid_fails() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dept (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql(
        "CREATE TABLE emp (id INTEGER PRIMARY KEY, dept_id INTEGER REFERENCES dept(id))",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO dept VALUES (1, 'HR')").unwrap();
    // dept_id=99 does not exist → must fail
    let err = vm.execute_sql("INSERT INTO emp VALUES (2, 99)");
    assert!(err.is_err(), "Expected FK constraint violation but got Ok");
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.to_ascii_lowercase().contains("foreign key")
            || msg.to_ascii_lowercase().contains("constraint"),
        "Error must mention constraint violation, got: {msg}"
    );
}

#[test]
fn test_l1_fk_null_value_bypasses_check() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cats (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql(
        "CREATE TABLE kittens (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES cats(id))",
    )
    .unwrap();
    // NULL FK bypasses check per SQL standard
    vm.execute_sql("INSERT INTO kittens VALUES (1, NULL)")
        .unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM kittens");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l1_fk_multiple_valid_inserts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE projects (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql(
        "CREATE TABLE tasks (id INTEGER PRIMARY KEY, proj_id INTEGER REFERENCES projects(id))",
    )
    .unwrap();
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
    vm.execute_sql("CREATE TABLE free (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO free VALUES (1, 42)").unwrap();
    vm.execute_sql("INSERT INTO free VALUES (2, 99)").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM free");
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ── L2: CHECK Constraints ──────────────────────────────────────────────────

#[test]
fn test_l2_check_simple_pass() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pos (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > 0))")
        .unwrap();
    vm.execute_sql("INSERT INTO pos VALUES (1, 10)").unwrap();
    let rows = qrows(&mut vm, "SELECT val FROM pos");
    assert_eq!(rows[0][0], Value::Integer(10));
}

#[test]
fn test_l2_check_simple_fail() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pos (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > 0))")
        .unwrap();
    let err = vm.execute_sql("INSERT INTO pos VALUES (1, -5)");
    assert!(
        err.is_err(),
        "Expected CHECK constraint violation but got Ok"
    );
    let msg = format!("{}", err.unwrap_err());
    assert!(
        msg.to_ascii_lowercase().contains("check")
            || msg.to_ascii_lowercase().contains("constraint"),
        "Error must mention CHECK, got: {msg}"
    );
}

#[test]
fn test_l2_check_boundary_equal() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER CHECK (score >= 0 AND score <= 100))").unwrap();
    vm.execute_sql("INSERT INTO scores VALUES (1, 0)").unwrap();
    vm.execute_sql("INSERT INTO scores VALUES (2, 100)")
        .unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM scores");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_l2_check_boundary_violation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER CHECK (score >= 0 AND score <= 100))").unwrap();
    let err = vm.execute_sql("INSERT INTO scores VALUES (1, 101)");
    assert!(
        err.is_err(),
        "score=101 should violate CHECK (score <= 100)"
    );
}

#[test]
fn test_l2_check_null_passes_through() {
    let mut vm = VM::new_memory();
    // Per SQL standard, NULL in CHECK expression evaluates to UNKNOWN → passes
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, rating INTEGER CHECK (rating > 3))")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, NULL)").unwrap();
    let rows = qrows(&mut vm, "SELECT id FROM t");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_l2_table_level_check() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE range_t (id INTEGER PRIMARY KEY, lo INTEGER, hi INTEGER, CHECK (lo < hi))",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO range_t VALUES (1, 1, 10)")
        .unwrap();
    let err = vm.execute_sql("INSERT INTO range_t VALUES (2, 10, 5)");
    assert!(err.is_err(), "lo=10, hi=5 violates CHECK (lo < hi)");
}

// ── O1: Column Statistics (ANALYZE TABLE) ─────────────────────────────────

#[test]
fn test_o1_analyze_basic_stats() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (3, 30)").unwrap();
    let r = vm.execute_sql("ANALYZE TABLE nums").unwrap();
    let msg = match r {
        ExecResult::Ok { message } => message,
        other => panic!("expected Ok, got {:?}", other),
    };
    assert!(
        msg.contains("3 rows") || msg.contains("3"),
        "should report 3 rows: {msg}"
    );

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
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, optional INTEGER)")
        .unwrap();
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
    vm.execute_sql("CREATE TABLE scores (id INTEGER PRIMARY KEY, score INTEGER)")
        .unwrap();
    for i in [5, 1, 9, 3, 7i64] {
        vm.execute_sql(&format!("INSERT INTO scores VALUES ({i}, {i})"))
            .unwrap();
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
    vm.execute_sql("CREATE TABLE empty (id INTEGER PRIMARY KEY)")
        .unwrap();
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
    vm.execute_sql("CREATE TABLE cbo_t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_v ON cbo_t (v)").unwrap();
    for i in 1..=(n as i64) {
        vm.execute_sql(&format!("INSERT INTO cbo_t VALUES ({i}, {i})"))
            .unwrap();
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
    let rows = qrows(
        &mut vm,
        "SELECT COUNT(*) FROM cbo_t WHERE v BETWEEN 1 AND 10",
    );
    assert_eq!(rows[0][0], Value::Integer(10));
}

/// Without ANALYZE, no stats → default selectivity 0.1 → index used (correctness check)
#[test]
fn test_o2_cbo_no_stats_result_correct() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nostats (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_ns ON nostats (v)")
        .unwrap();
    for i in 1..=20i64 {
        vm.execute_sql(&format!("INSERT INTO nostats VALUES ({i}, {i})"))
            .unwrap();
    }
    // No ANALYZE — falls back to default selectivity
    let rows = qrows(&mut vm, "SELECT v FROM nostats WHERE v = 7");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(7));
}

// --------------------------------------------------------------------------------
// L3: Trigger Tests
// --------------------------------------------------------------------------------

#[test]
fn test_l3_after_insert_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE audit (log TEXT)").unwrap();
    vm.execute_sql("CREATE TRIGGER t1 AFTER INSERT ON main FOR EACH ROW INSERT INTO audit (log) VALUES ('inserted');").unwrap();
    vm.execute_sql("INSERT INTO main (id, v) VALUES (1, 'a'), (2, 'b')")
        .unwrap();

    let res = qrows(&mut vm, "SELECT COUNT(*) FROM audit");
    assert_eq!(res[0][0], Value::Integer(2));
}

#[test]
fn test_l3_before_insert_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE prep (hit INTEGER)").unwrap();
    vm.execute_sql(
        "CREATE TRIGGER t2 BEFORE INSERT ON main FOR EACH ROW INSERT INTO prep (hit) VALUES (1);",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO main (id) VALUES (10)").unwrap();
    let res = qrows(&mut vm, "SELECT hit FROM prep");
    assert_eq!(res.len(), 1);
    assert_eq!(res[0][0], Value::Integer(1));
}

#[test]
fn test_l3_after_delete_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE log (action TEXT)").unwrap();
    vm.execute_sql("INSERT INTO main (id) VALUES (1)").unwrap();
    vm.execute_sql("CREATE TRIGGER t3 AFTER DELETE ON main FOR EACH ROW INSERT INTO log (action) VALUES ('deleted');").unwrap();

    vm.execute_sql("DELETE FROM main WHERE id = 1").unwrap();
    let res = qrows(&mut vm, "SELECT action FROM log");
    assert_eq!(res.len(), 1);
    assert_eq!(&res[0][0], &Value::Text("deleted".into()));
}

#[test]
fn test_l3_after_update_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE log (action TEXT)").unwrap();
    vm.execute_sql("INSERT INTO main (id, val) VALUES (1, 100)")
        .unwrap();
    vm.execute_sql("CREATE TRIGGER t4 AFTER UPDATE ON main FOR EACH ROW INSERT INTO log (action) VALUES ('updated');").unwrap();

    vm.execute_sql("UPDATE main SET val = 200 WHERE id = 1")
        .unwrap();
    let res = qrows(&mut vm, "SELECT action FROM log");
    assert_eq!(res.len(), 1);
    assert_eq!(&res[0][0], &Value::Text("updated".into()));
}

#[test]
fn test_l3_drop_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE log (hit INTEGER)").unwrap();
    vm.execute_sql(
        "CREATE TRIGGER t5 AFTER INSERT ON main FOR EACH ROW INSERT INTO log (hit) VALUES (1);",
    )
    .unwrap();

    vm.execute_sql("DROP TRIGGER t5").unwrap();
    vm.execute_sql("INSERT INTO main (id) VALUES (1)").unwrap();

    let res = qrows(&mut vm, "SELECT COUNT(*) FROM log");
    assert_eq!(res[0][0], Value::Integer(0));
}

#[test]
fn test_l3_multiple_triggers_same_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main (id INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE log (hit INTEGER)").unwrap();

    vm.execute_sql(
        "CREATE TRIGGER t6a AFTER INSERT ON main FOR EACH ROW INSERT INTO log (hit) VALUES (1);",
    )
    .unwrap();
    vm.execute_sql(
        "CREATE TRIGGER t6b AFTER INSERT ON main FOR EACH ROW INSERT INTO log (hit) VALUES (2);",
    )
    .unwrap();

    vm.execute_sql("INSERT INTO main (id) VALUES (99)").unwrap();

    let res = qrows(&mut vm, "SELECT hit FROM log ORDER BY hit");
    assert_eq!(res.len(), 2);
    assert_eq!(res[0][0], Value::Integer(1));
    assert_eq!(res[1][0], Value::Integer(2));
}

// ── O2-E: CBO Histogram Integration Tests ────────────────────────────────────

#[test]
fn test_cbo_histogram_built_by_analyze() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE histo (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    for i in 1..=100i64 {
        vm.execute_sql(&format!("INSERT INTO histo VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE histo").unwrap();
    let ts = vm.schema.get_table("histo").unwrap();
    let stats = ts.columns[1].stats.as_ref().unwrap();
    assert!(
        stats.histogram.is_some(),
        "histogram should be built by ANALYZE"
    );
    let hist = stats.histogram.as_ref().unwrap();
    assert!(!hist.is_empty(), "histogram should have buckets");
    // Last bucket cumulative should equal total non-null rows
    assert_eq!(hist.last().unwrap().cumulative_count, 100);
}

#[test]
fn test_cbo_histogram_selectivity_narrow_range() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hsel (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_hsel ON hsel (v)").unwrap();
    for i in 1..=1000i64 {
        vm.execute_sql(&format!("INSERT INTO hsel VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE hsel").unwrap();
    // Narrow range: 1% selectivity → should use index
    let rows = qrows(
        &mut vm,
        "SELECT COUNT(*) FROM hsel WHERE v BETWEEN 1 AND 10",
    );
    assert_eq!(rows[0][0], Value::Integer(10));
}

#[test]
fn test_cbo_histogram_selectivity_wide_range() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hwide (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_hw ON hwide (v)").unwrap();
    for i in 1..=100i64 {
        vm.execute_sql(&format!("INSERT INTO hwide VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE hwide").unwrap();
    // Wide range: 99% selectivity → CBO should prefer seq scan, result still correct
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM hwide WHERE v > 1");
    assert_eq!(rows[0][0], Value::Integer(99));
}

// ── O2-E: CBO Join Reorder Tests ─────────────────────────────────────────────

#[test]
fn test_cbo_join_reorder_small_first() {
    let mut vm = VM::new_memory();
    // Create large table
    vm.execute_sql("CREATE TABLE big (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    for i in 1..=500i64 {
        vm.execute_sql(&format!("INSERT INTO big VALUES ({i}, {i})"))
            .unwrap();
    }
    // Create small table
    vm.execute_sql("CREATE TABLE small (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    for i in 1..=5i64 {
        vm.execute_sql(&format!("INSERT INTO small VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE big").unwrap();
    vm.execute_sql("ANALYZE TABLE small").unwrap();
    // Join: result should be correct regardless of reorder
    let rows = qrows(
        &mut vm,
        "SELECT COUNT(*) FROM big JOIN small ON big.id = small.ref_id",
    );
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_cbo_join_reorder_three_tables() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE j1 (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE j2 (id INTEGER PRIMARY KEY, j1_id INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE j3 (id INTEGER PRIMARY KEY, j2_id INTEGER)")
        .unwrap();
    for i in 1..=100i64 {
        vm.execute_sql(&format!("INSERT INTO j1 VALUES ({i}, {i})"))
            .unwrap();
    }
    for i in 1..=10i64 {
        vm.execute_sql(&format!("INSERT INTO j2 VALUES ({i}, {i})"))
            .unwrap();
    }
    for i in 1..=3i64 {
        vm.execute_sql(&format!("INSERT INTO j3 VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE j1").unwrap();
    vm.execute_sql("ANALYZE TABLE j2").unwrap();
    vm.execute_sql("ANALYZE TABLE j3").unwrap();
    let rows = qrows(
        &mut vm,
        "SELECT j1.v, j3.j2_id FROM j1 JOIN j2 ON j1.id = j2.j1_id JOIN j3 ON j2.id = j3.j2_id",
    );
    assert_eq!(rows.len(), 3);
}

// ── B+ Tree Doubly-Linked List Integration Tests ─────────────────────────────

#[test]
fn test_btree_reverse_scan_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rev (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO rev VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO rev VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO rev VALUES (3, 'c')").unwrap();
    // Forward scan via SQL
    let fwd = qrows(&mut vm, "SELECT v FROM rev ORDER BY id ASC");
    assert_eq!(fwd.len(), 3);
    assert_eq!(fwd[0][0], Value::Text("a".into()));
    assert_eq!(fwd[2][0], Value::Text("c".into()));
    // Reverse scan via SQL
    let rev = qrows(&mut vm, "SELECT v FROM rev ORDER BY id DESC");
    assert_eq!(rev.len(), 3);
    assert_eq!(rev[0][0], Value::Text("c".into()));
    assert_eq!(rev[2][0], Value::Text("a".into()));
}

#[test]
fn test_btree_prev_leaf_maintained_after_splits() {
    use crate::storage::btree::BTree;
    // Insert enough rows to force multiple leaf splits, then verify
    // the doubly-linked list is consistent via reverse scan.
    let mut pager = crate::storage::pager::Pager::open_memory();
    let mut root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Insert 200 rows with big-ish payloads to force many splits
    for i in 1..=200i64 {
        let row = vec![
            Value::Integer(i),
            Value::Text(format!("val_{:04}", i).into()),
        ];
        let mut btree = BTree::new(&mut pager);
        root = btree.insert(root, i, &row).unwrap();
    }
    // Forward scan
    let mut btree = BTree::new(&mut pager);
    let fwd = btree.scan_all(root).unwrap();
    assert_eq!(fwd.len(), 200);
    // Reverse scan
    let rev = btree.scan_all_reverse(root).unwrap();
    assert_eq!(rev.len(), 200);
    // Check consistency: reverse of forward == reverse order
    for (i, (fwd_rid, _)) in fwd.iter().enumerate() {
        let (rev_rid, _) = &rev[fwd.len() - 1 - i];
        assert_eq!(fwd_rid, rev_rid, "mismatch at position {i}");
    }
}

// ── WAL Integration Tests ────────────────────────────────────────────────────

#[test]
fn test_wal_enable_and_basic_ops() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wal_t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO wal_t VALUES (1, 'hello')")
        .unwrap();
    let rows = qrows(&mut vm, "SELECT v FROM wal_t WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("hello".into()));
}

#[test]
fn test_wal_transaction_commit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wt (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO wt VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO wt VALUES (2, 20)").unwrap();
    vm.execute_sql("COMMIT").unwrap();
    let rows = qrows(&mut vm, "SELECT SUM(v) FROM wt");
    assert_eq!(rows[0][0], Value::Integer(30));
}

#[test]
fn test_wal_transaction_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wr (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO wr VALUES (1, 10)").unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO wr VALUES (2, 20)").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();
    let rows = qrows(&mut vm, "SELECT COUNT(*) FROM wr");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ── EXPLAIN ANALYZE Tests ────────────────────────────────────────────────────

#[test]
fn test_explain_analyze_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ea (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    for i in 1..=10i64 {
        vm.execute_sql(&format!("INSERT INTO ea VALUES ({i}, {i})"))
            .unwrap();
    }
    let result = vm.execute_sql("EXPLAIN ANALYZE SELECT * FROM ea").unwrap();
    match result {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("ANALYZE"), "should contain ANALYZE header");
            assert!(plan.contains("SCAN ea"), "should show scan of ea");
            assert!(
                plan.contains("Actual rows: 10"),
                "should report actual row count"
            );
            assert!(plan.contains("Execution time"), "should report timing");
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

#[test]
fn test_explain_analyze_with_stats() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ea2 (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_ea2 ON ea2 (v)").unwrap();
    for i in 1..=100i64 {
        vm.execute_sql(&format!("INSERT INTO ea2 VALUES ({i}, {i})"))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE ea2").unwrap();
    let result = vm
        .execute_sql("EXPLAIN ANALYZE SELECT * FROM ea2 WHERE v = 50")
        .unwrap();
    match result {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("SCAN ea2"), "should show scan of ea2");
            assert!(
                plan.contains("estimated rows: 100"),
                "should show estimated rows"
            );
            assert!(
                plan.contains("histogram available"),
                "should note histogram"
            );
            assert!(plan.contains("Actual rows"), "should report actual rows");
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

#[test]
fn test_explain_basic_shows_join_info() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ej1 (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE ej2 (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    let result = vm
        .execute_sql("EXPLAIN SELECT * FROM ej1 JOIN ej2 ON ej1.id = ej2.ref_id")
        .unwrap();
    match result {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("INNER JOIN"), "should show INNER JOIN");
            assert!(plan.contains("SCAN ej1"), "should show scan of ej1");
            assert!(plan.contains("SCAN ej2"), "should show scan of ej2");
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

// ── InnoDB Engine Mode ──────────────────────────────────────────────────────

#[test]
fn test_innodb_engine_config_defaults() {
    use crate::storage::pager::EngineConfig;
    let cfg = EngineConfig::default();
    assert_eq!(cfg.buffer_pool_pages, 256);
    assert_eq!(cfg.wal_auto_checkpoint, 1000);
    assert!(cfg.wal_enabled);
    assert!(!cfg.use_lz4);
}

#[test]
fn test_set_innodb_buffer_pool_pages() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let result = vm
        .execute_sql("SET innodb_buffer_pool_pages = '128'")
        .unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("128"),
                "message should contain the value: {}",
                message
            );
        }
        other => panic!("expected Ok, got {:?}", other),
    }
    assert_eq!(vm.pager.engine_config.buffer_pool_pages, 128);
}

#[test]
fn test_set_innodb_wal_auto_checkpoint() {
    let mut vm = VM::new_memory();
    let result = vm
        .execute_sql("SET innodb_wal_auto_checkpoint = '500'")
        .unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("500"));
        }
        other => panic!("expected Ok, got {:?}", other),
    }
    assert_eq!(vm.pager.engine_config.wal_auto_checkpoint, 500);
}

#[test]
fn test_set_innodb_flush_method() {
    let mut vm = VM::new_memory();
    let result = vm
        .execute_sql("SET innodb_flush_method = 'fdatasync'")
        .unwrap();
    match result {
        ExecResult::Ok { .. } => {}
        other => panic!("expected Ok, got {:?}", other),
    }
    assert_eq!(
        vm.pager.engine_config.flush_method,
        crate::storage::pager::FlushMethod::FdataSync
    );
}

#[test]
fn test_show_engine_status() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eng1 (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO eng1 VALUES (1, 'hello')")
        .unwrap();
    let result = vm.execute_sql("SHOW ENGINE STATUS").unwrap();
    match result {
        ExecResult::Explain { plan } => {
            assert!(
                plan.contains("InnoDB Engine Status"),
                "should contain title: {}",
                plan
            );
            assert!(
                plan.contains("Buffer pool pages"),
                "should contain buffer pool info: {}",
                plan
            );
            assert!(
                plan.contains("WAL enabled"),
                "should contain WAL info: {}",
                plan
            );
            assert!(
                plan.contains("Current LSN"),
                "should contain LSN info: {}",
                plan
            );
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

#[test]
fn test_innodb_lsn_advances_with_wal_writes() {
    let mut vm = VM::new_memory();
    // Enable WAL on in-memory pager (uses memory WAL)
    let uuid = [0u8; 16];
    vm.pager.wal = Some(crate::storage::wal::Wal::open_memory(&uuid));
    assert_eq!(vm.pager.current_lsn(), 0);

    vm.execute_sql("CREATE TABLE lsn_t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO lsn_t VALUES (1, 'a')").unwrap();
    // LSN should have advanced after WAL writes
    let lsn_after = vm.pager.current_lsn();
    assert!(
        lsn_after > 0,
        "LSN should advance after WAL writes: {}",
        lsn_after
    );
}

#[test]
fn test_set_invalid_flush_method_errors() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SET innodb_flush_method = 'invalid'");
    assert!(result.is_err(), "invalid flush method should error");
}

// ── CBO Join Algorithm Cost Model ───────────────────────────────────────────

#[test]
fn test_join_cost_model_small_tables_use_nested_loop() {
    use crate::vm::exec_select::choose_join_algorithm;
    use crate::vm::exec_select::JoinAlgorithm;
    // Both tables ≤ 64 rows: nested loop preferred
    assert_eq!(
        choose_join_algorithm(10, 20, true, false, false),
        JoinAlgorithm::NestedLoop
    );
    assert_eq!(
        choose_join_algorithm(64, 64, true, false, false),
        JoinAlgorithm::NestedLoop
    );
}

#[test]
fn test_join_cost_model_large_equi_uses_hash() {
    use crate::vm::exec_select::choose_join_algorithm;
    use crate::vm::exec_select::JoinAlgorithm;
    // Large tables with equi-join: hash join preferred
    assert_eq!(
        choose_join_algorithm(10000, 5000, true, false, false),
        JoinAlgorithm::HashJoin
    );
}

#[test]
fn test_join_cost_model_non_equi_uses_nested_loop() {
    use crate::vm::exec_select::choose_join_algorithm;
    use crate::vm::exec_select::JoinAlgorithm;
    // Non-equi join always uses nested loop
    assert_eq!(
        choose_join_algorithm(10000, 5000, false, false, false),
        JoinAlgorithm::NestedLoop
    );
}

#[test]
fn test_join_cost_model_both_sorted_uses_sort_merge() {
    use crate::vm::exec_select::choose_join_algorithm;
    use crate::vm::exec_select::JoinAlgorithm;
    // Both sides sorted on join key: sort-merge wins
    assert_eq!(
        choose_join_algorithm(10000, 5000, true, true, true),
        JoinAlgorithm::SortMergeJoin
    );
}

#[test]
fn test_explain_shows_join_algorithm_and_cardinality() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE jc1 (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE jc2 (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    // Insert enough rows so stats are meaningful
    for i in 0..100 {
        vm.execute_sql(&format!("INSERT INTO jc1 VALUES ({}, {})", i, i * 2))
            .unwrap();
        vm.execute_sql(&format!("INSERT INTO jc2 VALUES ({}, {})", i, i % 50))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE jc1").unwrap();
    vm.execute_sql("ANALYZE TABLE jc2").unwrap();
    let result = vm
        .execute_sql("EXPLAIN SELECT * FROM jc1 JOIN jc2 ON jc1.id = jc2.ref_id")
        .unwrap();
    match result {
        ExecResult::Explain { plan } => {
            // Should show join algorithm (Hash Join for 100-row equi-join)
            assert!(
                plan.contains("Hash Join")
                    || plan.contains("Sort-Merge Join")
                    || plan.contains("Nested Loop"),
                "should show join algorithm: {}",
                plan
            );
            // Should show cardinality estimates
            assert!(
                plan.contains("left≈") && plan.contains("right≈"),
                "should show cardinality estimates: {}",
                plan
            );
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}
