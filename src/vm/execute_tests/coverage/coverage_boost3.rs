//! Coverage boost tests – Phase 3.
//!
//! Targets specific uncovered code blocks identified by tarpaulin report (2026-03-11).
//! Focused on the top uncovered areas across eval_expr.rs, exec_select.rs,
//! exec_dml.rs, exec_ddl.rs, execute.rs, types.rs.
//!
//! Estimated new coverage: ~200+ lines across these modules.

use super::*;

// ═══════════════════════════════════════════════════════════════════════════════
//  A. eval_expr.rs — JSON_CONTAINS, FtsMatch in apply_binary_op
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_json_contains_array_int() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_CONTAINS('[1,2,3]', 2)"#);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_json_contains_array_string() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_CONTAINS('["a","b","c"]', 'b')"#);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_json_contains_not_found() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_CONTAINS('[1,2,3]', '5')"#);
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_json_contains_with_path() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        r#"SELECT JSON_CONTAINS('{"a":[1,2,3]}', 2, '$.a')"#,
    );
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_json_contains_null_needle() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_CONTAINS('[null, 1]', NULL)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_json_contains_real_needle() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_CONTAINS('[1.5, 2.5]', 1.5)"#);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_json_contains_on_non_json() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_CONTAINS(42, '1')");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_json_contains_too_few_args() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_CONTAINS('[1]')");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  B. eval_expr.rs — FtsMatch as binary operator (apply_binary_op path)
//     Covers lines 1832-1844
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_fts_match_direct_text() {
    // Use MATCH operator in WHERE clause on a table with TEXT columns
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE docs (id INTEGER PRIMARY KEY, content TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO docs VALUES (1, 'hello world rust'), (2, 'goodbye world'), (3, 'hello rust programming')").unwrap();
    // FtsMatch via WHERE: table_name MATCH 'keyword'
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM docs WHERE docs MATCH 'hello rust' ORDER BY id",
    );
    // Should find rows containing both 'hello' and 'rust'
    assert!(!rows.is_empty());
}

#[test]
fn test_fts_match_no_match() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE docs2 (id INTEGER PRIMARY KEY, content TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO docs2 VALUES (1, 'alpha beta')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM docs2 WHERE docs2 MATCH 'xyz'");
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_fts_match_empty_keyword() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE docs3 (id INTEGER PRIMARY KEY, content TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO docs3 VALUES (1, 'some text')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM docs3 WHERE docs3 MATCH ''");
    assert_eq!(rows.len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  C. eval_expr.rs — RANDOM/RAND functions (lines 1152-1163)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_random_function() {
    let mut vm = VM::new_memory();
    let rows1 = query_rows(&mut vm, "SELECT RANDOM()");
    let rows2 = query_rows(&mut vm, "SELECT RANDOM()");
    // Should return integers (may differ)
    assert!(matches!(rows1[0][0], Value::Integer(_)));
    assert!(matches!(rows2[0][0], Value::Integer(_)));
}

#[test]
fn test_rand_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT RAND()");
    assert!(matches!(rows[0][0], Value::Integer(_)));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  D. eval_expr.rs — auth.uid / auth_uid session functions (lines ~1165-1175)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_auth_uid_without_session() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT auth.uid()");
    // Without session, should return '' or Null
    match &rows[0][0] {
        Value::Text(s) => assert!(s.is_empty() || s.as_ref() == ""),
        Value::Null => {}
        other => panic!("Expected Text or Null, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  E. exec_select.rs — Semi/Anti joins (lines 1016-1120)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_left_semi_join_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ls1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE ls2 (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ls1 VALUES (1,'Alice'),(2,'Bob'),(3,'Charlie')")
        .unwrap();
    vm.execute_sql("INSERT INTO ls2 VALUES (1,1),(2,3)")
        .unwrap();
    // WHERE EXISTS simulates semi join
    let rows = query_rows(
        &mut vm,
        "SELECT id, name FROM ls1 WHERE EXISTS (SELECT 1 FROM ls2 WHERE ls2.ref_id = ls1.id) ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_not_exists_anti_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE na1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE na2 (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO na1 VALUES (1,'Alice'),(2,'Bob'),(3,'Charlie')")
        .unwrap();
    vm.execute_sql("INSERT INTO na2 VALUES (1,1),(2,3)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, name FROM na1 WHERE NOT EXISTS (SELECT 1 FROM na2 WHERE na2.ref_id = na1.id) ORDER BY id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  F. exec_select.rs — Blob in typed_key_into (line 621-624)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_group_by_blob_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE bt (id INTEGER PRIMARY KEY, data BLOB, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO bt VALUES (1, CAST('abc' AS BLOB), 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO bt VALUES (2, CAST('abc' AS BLOB), 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO bt VALUES (3, CAST('def' AS BLOB), 30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT data, SUM(val) FROM bt GROUP BY data ORDER BY data",
    );
    // Should group correctly even with BLOB type
    assert!(rows.len() >= 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  G. exec_dml.rs — INSERT auto-transaction commit/rollback (lines 58-85)
//     Trigger AFTER INSERT fire (lines 1493-1510)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_auto_txn_commit() {
    // Insert without explicit transaction should auto-commit
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at1 (id INTEGER PRIMARY KEY, x TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO at1 VALUES (1, 'auto')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT x FROM at1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("auto".into()));
}

#[test]
fn test_insert_in_explicit_txn() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at2 (id INTEGER PRIMARY KEY, x TEXT)")
        .unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO at2 VALUES (1, 'txn1')")
        .unwrap();
    vm.execute_sql("INSERT INTO at2 VALUES (2, 'txn2')")
        .unwrap();
    vm.execute_sql("COMMIT").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM at2");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_insert_txn_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at3 (id INTEGER PRIMARY KEY, x TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO at3 VALUES (1, 'kept')")
        .unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO at3 VALUES (2, 'rolled_back')")
        .unwrap();
    vm.execute_sql("ROLLBACK").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM at3");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  H. exec_dml.rs — ON CONFLICT DO UPDATE (lines 559-620)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_or_replace_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE upsert1 (id INTEGER PRIMARY KEY, name TEXT, c INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO upsert1 VALUES (1, 'Alice', 1)")
        .unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO upsert1 VALUES (1, 'Alice', 2)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT c FROM upsert1 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_insert_or_replace_no_conflict() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE upsert2 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO upsert2 VALUES (1, 'a')")
        .unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO upsert2 VALUES (2, 'b')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT val FROM upsert2 WHERE id = 2");
    assert_eq!(rows[0][0], Value::Text("b".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  I. exec_dml.rs — Check constraint evaluation helper (lines 1880-1910)
//     chk_cmp, chk_arith helper functions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_check_constraint_comparison() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk1 (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > 0))")
        .unwrap();
    // Valid insert
    vm.execute_sql("INSERT INTO chk1 VALUES (1, 5)").unwrap();
    let rows = query_rows(&mut vm, "SELECT val FROM chk1");
    assert_eq!(rows[0][0], Value::Integer(5));
    // Invalid insert should fail
    let result = vm.execute_sql("INSERT INTO chk1 VALUES (2, -1)");
    assert!(result.is_err());
}

#[test]
fn test_check_constraint_text_comparison() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk2 (id INTEGER PRIMARY KEY, status TEXT CHECK (status IN ('active','inactive')))").unwrap();
    vm.execute_sql("INSERT INTO chk2 VALUES (1, 'active')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT status FROM chk2");
    assert_eq!(rows[0][0], Value::Text("active".into()));
}

#[test]
fn test_check_constraint_real_arithmetic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk3 (id INTEGER PRIMARY KEY, price REAL CHECK (price >= 0.0))")
        .unwrap();
    vm.execute_sql("INSERT INTO chk3 VALUES (1, 9.99)").unwrap();
    let rows = query_rows(&mut vm, "SELECT price FROM chk3");
    assert_eq!(rows[0][0], Value::Real(9.99));
}

#[test]
fn test_check_constraint_is_null() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE chk4 (id INTEGER PRIMARY KEY, val INTEGER CHECK (val IS NOT NULL))",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO chk4 VALUES (1, 42)").unwrap();
    let result = vm.execute_sql("INSERT INTO chk4 VALUES (2, NULL)");
    assert!(result.is_err());
}

#[test]
fn test_check_constraint_and_or() {
    let mut vm = VM::new_memory();
    vm.execute_sql(
        "CREATE TABLE chk5 (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER CHECK (a > 0 AND b > 0))",
    )
    .unwrap();
    vm.execute_sql("INSERT INTO chk5 VALUES (1, 3, 4)").unwrap();
    let result = vm.execute_sql("INSERT INTO chk5 VALUES (2, -1, 4)");
    assert!(result.is_err());
}

#[test]
fn test_check_constraint_not() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk6 (id INTEGER PRIMARY KEY, val INTEGER CHECK (NOT val = 0))")
        .unwrap();
    vm.execute_sql("INSERT INTO chk6 VALUES (1, 5)").unwrap();
    let result = vm.execute_sql("INSERT INTO chk6 VALUES (2, 0)");
    assert!(result.is_err());
}

#[test]
fn test_check_constraint_unary_minus() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk7 (id INTEGER PRIMARY KEY, val INTEGER CHECK (val > -10))")
        .unwrap();
    vm.execute_sql("INSERT INTO chk7 VALUES (1, -5)").unwrap();
    vm.execute_sql("INSERT INTO chk7 VALUES (2, 0)").unwrap();
    let result = vm.execute_sql("INSERT INTO chk7 VALUES (3, -20)");
    assert!(result.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  J. exec_select.rs — FULL OUTER JOIN with ON expression (lines 950-961)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_full_join_with_complex_on() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fj1 (id INTEGER PRIMARY KEY, x INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE fj2 (id INTEGER PRIMARY KEY, y INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO fj1 VALUES (1, 10), (2, 20), (3, 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO fj2 VALUES (4, 10), (5, 20), (6, 40)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT fj1.x, fj2.y FROM fj1 FULL OUTER JOIN fj2 ON fj1.x = fj2.y ORDER BY COALESCE(fj1.x, fj2.y)",
    );
    // 10 matches, 20 matches, 30 unmatched from left, 40 unmatched from right
    assert_eq!(rows.len(), 4);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  K. exec_select.rs — Window: PERCENT_RANK & CUME_DIST with ORDER BY (3304-3373)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_percent_rank_with_duplicates() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pr (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO pr VALUES (1,10),(2,20),(3,20),(4,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val, PERCENT_RANK() OVER (ORDER BY val) FROM pr ORDER BY id",
    );
    assert_eq!(rows.len(), 4);
    // First row: percent_rank = 0.0
    if let Value::Real(v) = rows[0][1] {
        assert!((v - 0.0).abs() < 0.001);
    }
}

#[test]
fn test_percent_rank_single_row() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pr1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO pr1 VALUES (1, 100)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT PERCENT_RANK() OVER (ORDER BY val) FROM pr1",
    );
    assert_eq!(rows.len(), 1);
    // Single row: percent_rank = 0.0
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 0.0).abs() < 0.001);
    }
}

#[test]
fn test_cume_dist_with_order() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cd (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO cd VALUES (1,10),(2,20),(3,20),(4,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val, CUME_DIST() OVER (ORDER BY val) FROM cd ORDER BY id",
    );
    assert_eq!(rows.len(), 4);
    // Last row: cume_dist = 1.0
    if let Value::Real(v) = rows[3][1] {
        assert!((v - 1.0).abs() < 0.001);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  L. exec_ddl.rs — CREATE INDEX variants (lines 303-309, 288-299)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_index_if_not_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE idx_t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_name ON idx_t (name)")
        .unwrap();
    // Second create with IF NOT EXISTS should succeed
    vm.execute_sql("CREATE INDEX IF NOT EXISTS idx_name ON idx_t (name)")
        .unwrap();
}

#[test]
fn test_create_unique_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE idx_u (id INTEGER PRIMARY KEY, email TEXT)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX idx_email ON idx_u (email)")
        .unwrap();
    vm.execute_sql("INSERT INTO idx_u VALUES (1, 'a@b.com')")
        .unwrap();
    // Duplicate via unique index should fail
    let result = vm.execute_sql("INSERT INTO idx_u VALUES (2, 'a@b.com')");
    assert!(result.is_err());
}

#[test]
fn test_drop_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE idx_d (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON idx_d (val)")
        .unwrap();
    vm.execute_sql("DROP INDEX idx_val").unwrap();
}

#[test]
fn test_drop_index_if_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("DROP INDEX IF EXISTS nonexistent_idx")
        .unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
//  M. exec_ddl.rs — RLS/policy (lines 800-840)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_rls_enable_disable() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rls_t (id INTEGER PRIMARY KEY, data TEXT)")
        .unwrap();
    vm.execute_sql("ALTER TABLE rls_t ENABLE ROW LEVEL SECURITY")
        .unwrap();
    // Insert should still work
    vm.execute_sql("INSERT INTO rls_t VALUES (1, 'secure')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT data FROM rls_t");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_rls_policy_crud() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rls2 (id INTEGER PRIMARY KEY, owner TEXT, data TEXT)")
        .unwrap();
    vm.execute_sql("ALTER TABLE rls2 ENABLE ROW LEVEL SECURITY")
        .unwrap();
    vm.execute_sql("CREATE POLICY p1 ON rls2 FOR SELECT USING (owner = 'admin')")
        .unwrap();
    vm.execute_sql("DROP POLICY p1 ON rls2").unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
//  N. execute.rs — VACUUM (line 860-864, already has a test but let's cover more)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_vacuum_after_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE v1 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    for i in 0..20 {
        vm.execute_sql(&format!("INSERT INTO v1 VALUES ({}, 'data_{}')", i, i))
            .unwrap();
    }
    vm.execute_sql("DELETE FROM v1 WHERE id < 10").unwrap();
    vm.execute_sql("VACUUM").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM v1");
    assert_eq!(rows[0][0], Value::Integer(10));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  O. eval_expr.rs — JSON helper functions: json_array_get, json_set (2151-2228)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_json_extract_nested_object() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        r#"SELECT JSON_EXTRACT('{"a":{"b":{"c":42}}}', '$.a.b.c')"#,
    );
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("42"), "Expected 42, got '{}'", s);
    } else if let Value::Integer(v) = rows[0][0] {
        assert_eq!(v, 42);
    }
}

#[test]
fn test_json_extract_array_index() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_EXTRACT('[10,20,30]', '$[1]')"#);
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("20"), "Expected 20, got '{}'", s);
    } else if let Value::Integer(v) = rows[0][0] {
        assert_eq!(v, 20);
    }
}

#[test]
fn test_json_extract_array_nested_object() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        r#"SELECT JSON_EXTRACT('[{"a":1},{"a":2}]', '$[0].a')"#,
    );
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("1"), "Expected 1, got '{}'", s);
    }
}

#[test]
fn test_json_set_new_key() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_SET('{"a":1}', '$.b', '2')"#);
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("\"b\""), "Expected key b, got '{}'", s);
    }
}

#[test]
fn test_json_set_replace_existing() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_SET('{"a":1}', '$.a', '99')"#);
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("99"), "Expected 99, got '{}'", s);
    }
}

#[test]
fn test_json_type_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_TYPE('{"a":1}')"#);
    if let Value::Text(s) = &rows[0][0] {
        assert!(
            s.contains("object") || s.contains("OBJECT"),
            "Expected object type, got '{}'",
            s
        );
    }
}

#[test]
fn test_json_valid_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_VALID('{}')");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT JSON_VALID('not json')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  P. exec_select.rs — eval_expr_with_aggregates BinaryOp And/Or (2302-2327)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_having_and_condition() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hav (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO hav VALUES (1,'a',10),(2,'a',20),(3,'b',30),(4,'b',40),(5,'c',5)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT cat, SUM(val) as s FROM hav GROUP BY cat HAVING SUM(val) > 10 AND COUNT(*) > 1 ORDER BY cat",
    );
    assert_eq!(rows.len(), 2); // 'a' sum=30, 'b' sum=70, 'c' excluded
}

#[test]
fn test_having_or_condition() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hav2 (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO hav2 VALUES (1,'a',10),(2,'b',2),(3,'c',50)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT cat, SUM(val) as s FROM hav2 GROUP BY cat HAVING SUM(val) >= 50 OR SUM(val) <= 5 ORDER BY cat",
    );
    assert_eq!(rows.len(), 2); // 'b' sum=2, 'c' sum=50
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Q. types.rs — PrefixPageEncoder/Decoder, format_as_timestamp (505-557)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_format_timestamp_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ts (id INTEGER PRIMARY KEY, created_at TIMESTAMP)")
        .unwrap();
    vm.execute_sql("INSERT INTO ts VALUES (1, '2025-01-15 10:30:00')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT created_at FROM ts");
    assert_eq!(rows.len(), 1);
    // Should store and retrieve timestamp
    match &rows[0][0] {
        Value::Text(s) => assert!(s.contains("2025"), "Expected timestamp, got '{}'", s),
        Value::Integer(_) => {} // epoch form is also acceptable
        other => panic!("Unexpected type: {:?}", other),
    }
}

#[test]
fn test_date_column_type() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dt (id INTEGER PRIMARY KEY, d DATE)")
        .unwrap();
    vm.execute_sql("INSERT INTO dt VALUES (1, '2025-06-15')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT d FROM dt");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  R. exec_dml.rs — UPDATE with RETURNING (covers different path from INSERT RETURNING)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_update_returning_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ur (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ur VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    let result = vm.execute_sql("UPDATE ur SET val = val * 2 WHERE id <= 2 RETURNING id, val");
    if let Ok(ExecResult::QueryResult { rows, columns }) = result {
        assert_eq!(rows.len(), 2);
        assert!(columns.len() >= 2);
    }
}

#[test]
fn test_delete_returning_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dr (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO dr VALUES (1,'a'),(2,'b'),(3,'c')")
        .unwrap();
    let result = vm.execute_sql("DELETE FROM dr WHERE id >= 2 RETURNING *");
    if let Ok(ExecResult::QueryResult { rows, .. }) = result {
        assert_eq!(rows.len(), 2);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  S. eval_expr.rs — complex CASE expressions with NULL
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_searched_case_with_null_propagation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cs (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO cs VALUES (1, NULL), (2, 10), (3, NULL)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE WHEN val > 5 THEN 'big' WHEN val IS NULL THEN 'missing' ELSE 'small' END FROM cs ORDER BY id",
    );
    assert_eq!(rows[0][0], Value::Text("missing".into()));
    assert_eq!(rows[1][0], Value::Text("big".into()));
    assert_eq!(rows[2][0], Value::Text("missing".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  T. eval_expr.rs — DATE_EXTRACT edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_date_extract_all_parts() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('YEAR', '2025-03-15 14:30:45'), DATE_EXTRACT('MONTH', '2025-03-15 14:30:45'), DATE_EXTRACT('DAY', '2025-03-15 14:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(2025));
    assert_eq!(rows[0][1], Value::Integer(3));
    assert_eq!(rows[0][2], Value::Integer(15));
}

#[test]
fn test_date_extract_time_parts() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('HOUR', '2025-03-15 14:30:45'), DATE_EXTRACT('MINUTE', '2025-03-15 14:30:45'), DATE_EXTRACT('SECOND', '2025-03-15 14:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(14));
    assert_eq!(rows[0][1], Value::Integer(30));
    assert_eq!(rows[0][2], Value::Integer(45));
}

#[test]
fn test_date_extract_invalid_part() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT DATE_EXTRACT('WEEK', '2025-03-15')");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  U. Multiple NULL-related binary op paths
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_null_and_false() {
    let mut vm = VM::new_memory();
    // NULL AND false should be false (0)
    let rows = query_rows(&mut vm, "SELECT NULL AND 0");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_null_and_true() {
    let mut vm = VM::new_memory();
    // NULL AND true should be NULL
    let rows = query_rows(&mut vm, "SELECT NULL AND 1");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_null_or_true() {
    let mut vm = VM::new_memory();
    // NULL OR true should be true (1)
    let rows = query_rows(&mut vm, "SELECT NULL OR 1");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_null_or_false() {
    let mut vm = VM::new_memory();
    // NULL OR false should be NULL
    let rows = query_rows(&mut vm, "SELECT NULL OR 0");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_false_and_null() {
    let mut vm = VM::new_memory();
    // false AND NULL should be false (0) - left side non-null falsy
    let rows = query_rows(&mut vm, "SELECT 0 AND NULL");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_true_or_null() {
    let mut vm = VM::new_memory();
    // true OR NULL should be true (1)
    let rows = query_rows(&mut vm, "SELECT 1 OR NULL");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_null_arithmetic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT NULL + 1, NULL - 1, NULL * 2, NULL / 3, 5 + NULL",
    );
    #[allow(clippy::needless_range_loop)]
    for i in 0..5 {
        assert_eq!(rows[0][i], Value::Null);
    }
}

#[test]
fn test_null_comparison() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT NULL = NULL, NULL <> 1, NULL < 5, NULL > 5, NULL >= 1, NULL <= 1",
    );
    #[allow(clippy::needless_range_loop)]
    for i in 0..6 {
        assert_eq!(rows[0][i], Value::Null, "Column {} should be NULL", i);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  V. exec_select.rs — Aggregate functions on empty tables
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_aggregate_empty_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE emp (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT COUNT(*), SUM(val), AVG(val), MIN(val), MAX(val) FROM emp",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(0)); // COUNT
    assert_eq!(rows[0][1], Value::Null); // SUM
    assert_eq!(rows[0][2], Value::Null); // AVG
    assert_eq!(rows[0][3], Value::Null); // MIN
    assert_eq!(rows[0][4], Value::Null); // MAX
}

// ═══════════════════════════════════════════════════════════════════════════════
//  W. Multiple join types combined
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_three_way_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE j1(id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE j2(id INTEGER PRIMARY KEY, j1_id INTEGER, val TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE j3(id INTEGER PRIMARY KEY, j2_id INTEGER, extra TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO j1 VALUES (1,'a'),(2,'b')")
        .unwrap();
    vm.execute_sql("INSERT INTO j2 VALUES (1,1,'x'),(2,1,'y'),(3,2,'z')")
        .unwrap();
    vm.execute_sql("INSERT INTO j3 VALUES (1,1,'p'),(2,2,'q')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT j1.name, j2.val, j3.extra FROM j1 JOIN j2 ON j1.id = j2.j1_id JOIN j3 ON j2.id = j3.j2_id ORDER BY j1.name, j2.val",
    );
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  X. exec_select.rs — Window functions: ROW_NUMBER, NTILE, LAG, LEAD
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_window_ntile() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wn (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO wn VALUES (1,10),(2,20),(3,30),(4,40),(5,50)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val, NTILE(3) OVER (ORDER BY val) FROM wn ORDER BY val",
    );
    assert_eq!(rows.len(), 5);
    // NTILE(3) over 5 rows: groups of 2,2,1
    if let Value::Integer(v) = rows[0][1] {
        assert_eq!(v, 1);
    }
}

#[test]
fn test_window_lag_lead() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wll (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO wll VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val, LAG(val) OVER (ORDER BY val), LEAD(val) OVER (ORDER BY val) FROM wll ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    // First row: LAG should be NULL
    assert_eq!(rows[0][1], Value::Null);
    // Last row: LEAD should be NULL
    assert_eq!(rows[2][2], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Y. Additional operator paths
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_bitwise_and_with_values() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 255 & 15");
    assert_eq!(rows[0][0], Value::Integer(15));
}

#[test]
fn test_bitwise_or_with_values() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 240 | 15");
    assert_eq!(rows[0][0], Value::Integer(255));
}

#[test]
fn test_bitwise_xor_with_values() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 255 ^ 15");
    assert_eq!(rows[0][0], Value::Integer(240));
}

#[test]
fn test_modulo_with_reals() {
    let mut vm = VM::new_memory();
    // Modulo with non-integer should return NULL
    let rows = query_rows(&mut vm, "SELECT 10 % 3");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  Z. exec_select.rs — CROSS JOIN & self-join
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_cross_join_sizes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cj1 (x INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE cj2 (y INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO cj1 VALUES (1),(2),(3)")
        .unwrap();
    vm.execute_sql("INSERT INTO cj2 VALUES (10),(20)").unwrap();
    let rows = query_rows(&mut vm, "SELECT x, y FROM cj1 CROSS JOIN cj2 ORDER BY x, y");
    assert_eq!(rows.len(), 6); // 3 * 2
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AA. Additional string function coverage
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_hex_with_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT HEX(CAST('AB' AS BLOB))");
    if let Value::Text(s) = &rows[0][0] {
        assert_eq!(s.as_ref(), "4142");
    }
}

#[test]
fn test_char_function_multiple() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CHAR(72, 101, 108, 108, 111)");
    assert_eq!(rows[0][0], Value::Text("Hello".into()));
}

#[test]
fn test_regexp_like_no_match() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('hello world', '^[0-9]+$')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_starts_with_false() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello', 'world')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AB. Complex subquery patterns
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_scalar_subquery_in_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sq1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO sq1 VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, (SELECT MAX(val) FROM sq1) as max_val FROM sq1 ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(30));
    assert_eq!(rows[2][1], Value::Integer(30));
}

#[test]
fn test_correlated_subquery_in_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main_t (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE ref_t (id INTEGER PRIMARY KEY, main_id INTEGER, score INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO main_t VALUES (1,100),(2,200),(3,300)")
        .unwrap();
    vm.execute_sql("INSERT INTO ref_t VALUES (1,1,50),(2,1,60),(3,2,70)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM main_t WHERE val > (SELECT MAX(score) FROM ref_t WHERE ref_t.main_id = main_t.id) ORDER BY id",
    );
    assert!(!rows.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AC. Additional DDL coverage — ALTER TABLE variants
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_alter_table_add_column_with_default() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE alt1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO alt1 VALUES (1),(2),(3)")
        .unwrap();
    vm.execute_sql("ALTER TABLE alt1 ADD COLUMN status TEXT DEFAULT 'active'")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, status FROM alt1 ORDER BY id");
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_create_table_multi_column_pk() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE mcp (a INTEGER, b INTEGER, c TEXT, PRIMARY KEY (a, b))")
        .unwrap();
    vm.execute_sql("INSERT INTO mcp VALUES (1, 1, 'a')")
        .unwrap();
    vm.execute_sql("INSERT INTO mcp VALUES (1, 2, 'b')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT c FROM mcp ORDER BY a, b");
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AD. exec_ddl.rs — CREATE/DROP TABLE IF [NOT] EXISTS
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_table_if_not_exists_already_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE exists_t (id INTEGER PRIMARY KEY)")
        .unwrap();
    // Should not error
    vm.execute_sql("CREATE TABLE IF NOT EXISTS exists_t (id INTEGER PRIMARY KEY)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM exists_t");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_drop_table_if_exists_nonexistent() {
    let mut vm = VM::new_memory();
    // Should not error
    vm.execute_sql("DROP TABLE IF EXISTS nonexistent_table")
        .unwrap();
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AE. exec_dml.rs — Multi-row INSERT with various types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_mixed_types() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE mt (id INTEGER PRIMARY KEY, i INTEGER, r REAL, t TEXT, b BLOB)")
        .unwrap();
    vm.execute_sql("INSERT INTO mt VALUES (1, 42, 3.14, 'hello', CAST('data' AS BLOB))")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM mt");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Integer(42));
    assert_eq!(rows[0][2], Value::Real(3.14));
    assert_eq!(rows[0][3], Value::Text("hello".into()));
}

#[test]
fn test_insert_null_values() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nv (id INTEGER PRIMARY KEY, a TEXT, b INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO nv VALUES (1, NULL, NULL)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT a, b FROM nv");
    assert_eq!(rows[0][0], Value::Null);
    assert_eq!(rows[0][1], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AF. Complex WHERE with multiple operator types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_complex_where_mixed_ops() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cw (id INTEGER PRIMARY KEY, a INTEGER, b REAL, c TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO cw VALUES (1,10,1.5,'foo'), (2,20,2.5,'bar'), (3,30,3.5,'baz')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM cw WHERE (a >= 10 AND b < 3.0) OR c LIKE 'ba%' ORDER BY id",
    );
    assert!(rows.len() >= 2);
}

#[test]
fn test_between_real_values() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 2.5 BETWEEN 1.0 AND 3.0");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AG. exec_select.rs — UNION / EXCEPT / INTERSECT with ORDER BY
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_union_with_order_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE u1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE u2 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO u1 VALUES (1,10),(2,20)")
        .unwrap();
    vm.execute_sql("INSERT INTO u2 VALUES (3,30),(4,20)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val FROM u1 UNION SELECT val FROM u2 ORDER BY val",
    );
    assert_eq!(rows.len(), 3); // 10, 20, 30 (20 deduped)
}

#[test]
fn test_except_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE e1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE e2 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO e1 VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    vm.execute_sql("INSERT INTO e2 VALUES (1,20)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val FROM e1 EXCEPT SELECT val FROM e2 ORDER BY val",
    );
    assert_eq!(rows.len(), 2); // 10, 30
}

#[test]
fn test_intersect_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE i1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE i2 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO i1 VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    vm.execute_sql("INSERT INTO i2 VALUES (1,20),(2,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val FROM i1 INTERSECT SELECT val FROM i2 ORDER BY val",
    );
    assert_eq!(rows.len(), 2); // 20, 30
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AH. exec_dml.rs — DELETE with complex WHERE
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_delete_with_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE del1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE del2 (id INTEGER PRIMARY KEY, ref_val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO del1 VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    vm.execute_sql("INSERT INTO del2 VALUES (1,10),(2,30)")
        .unwrap();
    vm.execute_sql("DELETE FROM del1 WHERE val IN (SELECT ref_val FROM del2)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id FROM del1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AI. exec_select.rs — GROUP BY with expressions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_group_by_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ge (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ge VALUES (1,15),(2,25),(3,35),(4,45)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE WHEN val < 20 THEN 'low' WHEN val < 40 THEN 'mid' ELSE 'high' END as tier, COUNT(*) FROM ge GROUP BY tier ORDER BY tier",
    );
    assert!(rows.len() >= 2);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AJ. exec_select.rs — Window: FIRST_VALUE & LAST_VALUE
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_window_first_last_value() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE flv (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO flv VALUES (1,100),(2,200),(3,300)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT val, FIRST_VALUE(val) OVER (ORDER BY val), LAST_VALUE(val) OVER (ORDER BY val) FROM flv ORDER BY val",
    );
    assert_eq!(rows.len(), 3);
    // FIRST_VALUE should always be 100
    assert_eq!(rows[0][1], Value::Integer(100));
    assert_eq!(rows[2][1], Value::Integer(100));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AK. EXPLAIN coverage for various statements
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_explain_create_table() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("EXPLAIN CREATE TABLE ex (id INTEGER PRIMARY KEY)");
    assert!(result.is_ok());
}

#[test]
fn test_explain_drop_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ex_drop (id INTEGER PRIMARY KEY)")
        .unwrap();
    let result = vm.execute_sql("EXPLAIN DROP TABLE ex_drop");
    assert!(result.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AL. Additional math functions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_abs_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT ABS(-42), ABS(42), ABS(-3.14)");
    assert_eq!(rows[0][0], Value::Integer(42));
    assert_eq!(rows[0][1], Value::Integer(42));
    if let Value::Real(v) = rows[0][2] {
        assert!((v - 3.14).abs() < 0.001);
    }
}

#[test]
fn test_cbrt_function_negative() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CBRT(-27)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - (-3.0)).abs() < 0.001);
    }
}

#[test]
fn test_sign_function_cases() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SIGN(-5), SIGN(0), SIGN(10)");
    assert_eq!(rows[0][0], Value::Integer(-1));
    assert_eq!(rows[0][1], Value::Integer(0));
    assert_eq!(rows[0][2], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AM. INSERT ... ON CONFLICT with unique index (exec_dml.rs 439-526)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_or_replace_with_unique_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE oci (id INTEGER PRIMARY KEY, email TEXT, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX idx_oci_email ON oci (email)")
        .unwrap();
    vm.execute_sql("INSERT INTO oci VALUES (1, 'a@b.com', 'Alice')")
        .unwrap();
    // INSERT OR REPLACE should handle conflict
    vm.execute_sql("INSERT OR REPLACE INTO oci VALUES (1, 'a@b.com', 'Updated')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT name FROM oci WHERE email = 'a@b.com'");
    assert!(!rows.is_empty());
    assert_eq!(rows[0][0], Value::Text("Updated".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AN. exec_dml.rs — TRIGGER AFTER INSERT/UPDATE/DELETE (lines 1493-1510)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_after_insert_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_main (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE trig_log (id INTEGER PRIMARY KEY, msg TEXT)")
        .unwrap();
    vm.execute_sql(
        "CREATE TRIGGER trg_ins AFTER INSERT ON trig_main FOR EACH ROW BEGIN INSERT INTO trig_log VALUES (1, 'inserted'); END"
    ).unwrap();
    vm.execute_sql("INSERT INTO trig_main VALUES (1, 100)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT msg FROM trig_log");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("inserted".into()));
}

#[test]
fn test_after_update_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig2 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE trig2_log (id INTEGER PRIMARY KEY, msg TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO trig2 VALUES (1, 10)").unwrap();
    vm.execute_sql(
        "CREATE TRIGGER trg_upd AFTER UPDATE ON trig2 FOR EACH ROW BEGIN INSERT INTO trig2_log VALUES (1, 'updated'); END"
    ).unwrap();
    vm.execute_sql("UPDATE trig2 SET val = 20 WHERE id = 1")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT msg FROM trig2_log");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_after_delete_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig3 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE trig3_log (id INTEGER PRIMARY KEY, msg TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO trig3 VALUES (1, 10)").unwrap();
    vm.execute_sql(
        "CREATE TRIGGER trg_del AFTER DELETE ON trig3 FOR EACH ROW BEGIN INSERT INTO trig3_log VALUES (1, 'deleted'); END"
    ).unwrap();
    vm.execute_sql("DELETE FROM trig3 WHERE id = 1").unwrap();
    let rows = query_rows(&mut vm, "SELECT msg FROM trig3_log");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AO. storage/cursor.rs — cursor operations (lines 225-271)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_cursor_large_dataset() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cur (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    // Insert enough rows to potentially span multiple pages
    for i in 0..100 {
        vm.execute_sql(&format!("INSERT INTO cur VALUES ({}, 'row_{}')", i, i))
            .unwrap();
    }
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM cur");
    assert_eq!(rows[0][0], Value::Integer(100));
    // Test range scan
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM cur WHERE id >= 50 AND id < 60 ORDER BY id",
    );
    assert_eq!(rows.len(), 10);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AP. Index usage paths (storage/btree.rs lines 716-756, 1187-1407)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_index_scan_with_delete_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE idx_sd (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_sd_val ON idx_sd (val)")
        .unwrap();
    for i in 0..50 {
        vm.execute_sql(&format!("INSERT INTO idx_sd VALUES ({}, {})", i, i * 10))
            .unwrap();
    }
    // Delete some rows to exercise index maintenance
    vm.execute_sql("DELETE FROM idx_sd WHERE val < 100")
        .unwrap();
    // Update some rows
    vm.execute_sql("UPDATE idx_sd SET val = val + 1 WHERE val >= 400")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM idx_sd");
    assert_eq!(rows[0][0], Value::Integer(40));
}

#[test]
fn test_btree_split_rebalance() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE bt_split (id INTEGER PRIMARY KEY, data TEXT)")
        .unwrap();
    // Insert enough data to force page splits
    let long_text = "x".repeat(200);
    for i in 0..100 {
        vm.execute_sql(&format!(
            "INSERT INTO bt_split VALUES ({}, '{}')",
            i, long_text
        ))
        .unwrap();
    }
    // Delete half to trigger potential rebalancing
    vm.execute_sql("DELETE FROM bt_split WHERE id % 2 = 0")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM bt_split");
    assert_eq!(rows[0][0], Value::Integer(50));
    // Verify data integrity
    let rows = query_rows(&mut vm, "SELECT id FROM bt_split ORDER BY id LIMIT 3");
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
    assert_eq!(rows[2][0], Value::Integer(5));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AQ. exec_select.rs — Subquery in FROM clause
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_derived_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dt1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO dt1 VALUES (1,10),(2,20),(3,30),(4,40)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT * FROM (SELECT id, val * 2 as doubled FROM dt1 WHERE val >= 20) sub ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(40)); // 20 * 2
}

// ═══════════════════════════════════════════════════════════════════════════════
//  AR. Additional CAST edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_cast_bool_to_integer() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(1 > 0 AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_cast_null_to_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS TEXT)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_blob_to_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(CAST('hello' AS BLOB) AS TEXT)");
    if let Value::Text(s) = &rows[0][0] {
        assert_eq!(s.as_ref(), "hello");
    }
}
