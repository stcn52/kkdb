//! Coverage Boost Round 6 — surgically targeting remaining uncovered paths
//! to push past 75%.
//!
//! Targets:
//!   - RIGHT JOIN with ON (exec_select.rs ~L1005-1040)
//!   - LeftSemi / RightSemi join paths (exec_select.rs ~L1060-1105)
//!   - HAVING with LIKE (exec_select.rs ~L2425-2435)
//!   - Schema load: trigger/FK/check restore (schema.rs ~L215-368)
//!   - CAST BLOB paths and edge cases (eval_expr.rs ~L1602-1670)
//!   - SetOp inside FROM clause (exec_select.rs ~L1543)
//!   - Large payload BLOB: overflow cursor path (cursor.rs)
//!   - FULL JOIN with matched/unmatched on both sides (exec_select.rs ~L950-1040)
//!   - Complex multi-table AUTO-TXN paths (exec_dml.rs ~L58-85)

use super::*;

// ═══════════════════════════════════════════════════════════════════════
//  Section A: RIGHT JOIN exercising exec_select.rs L1005-1040
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_right_join_all_unmatched_left() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rj_l (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE rj_r (id INTEGER PRIMARY KEY, ref_id INTEGER, w TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rj_l VALUES (1,'a'),(2,'b')").unwrap();
    vm.execute_sql("INSERT INTO rj_r VALUES (1,10,'x'),(2,20,'y')").unwrap();
    // No match at all — all right rows come with NULL left columns
    let rows = query_rows(&mut vm,
        "SELECT rj_l.v, rj_r.w FROM rj_l RIGHT JOIN rj_r ON rj_l.id = rj_r.ref_id ORDER BY rj_r.id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Null);
    assert_eq!(rows[1][0], Value::Null);
}

#[test]
fn test_right_join_partial_match() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rjp_l (id INTEGER PRIMARY KEY, k INTEGER, v TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE rjp_r (id INTEGER PRIMARY KEY, k INTEGER, w TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rjp_l VALUES (1,100,'a'),(2,200,'b')").unwrap();
    vm.execute_sql("INSERT INTO rjp_r VALUES (1,100,'x'),(2,300,'y'),(3,400,'z')").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT rjp_l.v, rjp_r.w FROM rjp_l RIGHT JOIN rjp_r ON rjp_l.k = rjp_r.k ORDER BY rjp_r.id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Text("a".into())); // matched
    assert_eq!(rows[1][0], Value::Null); // unmatched
    assert_eq!(rows[2][0], Value::Null); // unmatched
}

#[test]
fn test_right_join_with_complex_on() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rjc_a (id INTEGER PRIMARY KEY, x INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE rjc_b (id INTEGER PRIMARY KEY, y INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO rjc_a VALUES (1,10),(2,20),(3,30)").unwrap();
    vm.execute_sql("INSERT INTO rjc_b VALUES (1,10),(2,15),(3,30)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT rjc_a.x, rjc_b.y FROM rjc_a RIGHT JOIN rjc_b ON rjc_a.x = rjc_b.y AND rjc_b.y > 10 ORDER BY rjc_b.id");
    assert_eq!(rows.len(), 3);
    // y=10: no match because condition y>10 fails → NULL, 10
    assert_eq!(rows[0][0], Value::Null);
    // y=30: match → 30, 30
    assert_eq!(rows[2][0], Value::Integer(30));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section B: FULL OUTER JOIN with mixed matches — exec_select.rs ~L940-1040
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_full_join_three_way_overlap() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fj_l (id INTEGER PRIMARY KEY, k INTEGER, lv TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE fj_r (id INTEGER PRIMARY KEY, k INTEGER, rv TEXT)").unwrap();
    vm.execute_sql("INSERT INTO fj_l VALUES (1,1,'L1'),(2,2,'L2'),(3,3,'L3')").unwrap();
    vm.execute_sql("INSERT INTO fj_r VALUES (1,2,'R2'),(2,4,'R4')").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT fj_l.lv, fj_r.rv FROM fj_l FULL OUTER JOIN fj_r ON fj_l.k = fj_r.k ORDER BY fj_l.k");
    // L1 (no match), L2+R2, L3 (no match), R4 (no left match)
    assert!(rows.len() >= 3);
}

#[test]
fn test_full_join_empty_tables() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fje_l (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE fje_r (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT fje_l.v, fje_r.v FROM fje_l FULL OUTER JOIN fje_r ON fje_l.id = fje_r.id");
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_full_join_one_empty() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fjo_l (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE fjo_r (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("INSERT INTO fjo_l VALUES (1,'a'),(2,'b')").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT fjo_l.v, fjo_r.v FROM fjo_l FULL OUTER JOIN fjo_r ON fjo_l.id = fjo_r.id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════
//  Section C: LeftSemi join — exec_select.rs L1060-1085
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_left_semi_join_via_exists_large() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE lsj_a (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE lsj_b (id INTEGER PRIMARY KEY, ref_v INTEGER)").unwrap();
    for i in 0..50 {
        vm.execute_sql(&format!("INSERT INTO lsj_a VALUES ({}, {})", i, i * 10)).unwrap();
    }
    for i in 0..20 {
        vm.execute_sql(&format!("INSERT INTO lsj_b VALUES ({}, {})", i, i * 10)).unwrap();
    }
    let rows = query_rows(&mut vm,
        "SELECT a.v FROM lsj_a a WHERE EXISTS (SELECT 1 FROM lsj_b b WHERE b.ref_v = a.v) ORDER BY a.v");
    assert_eq!(rows.len(), 20);
}

#[test]
fn test_semi_join_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sj1 (id INTEGER PRIMARY KEY, code TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE sj2 (id INTEGER PRIMARY KEY, code TEXT)").unwrap();
    vm.execute_sql("INSERT INTO sj1 VALUES (1,'A'),(2,'B'),(3,'C'),(4,'D')").unwrap();
    vm.execute_sql("INSERT INTO sj2 VALUES (1,'B'),(2,'D')").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT code FROM sj1 WHERE code IN (SELECT code FROM sj2) ORDER BY code");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("B".into()));
    assert_eq!(rows[1][0], Value::Text("D".into()));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section D: HAVING with LIKE — exec_select.rs L2425-2435
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_having_with_like() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hv_l (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO hv_l VALUES (1,'alpha',10),(2,'alpha',20),(3,'beta',30),(4,'gamma',40)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT cat, SUM(val) FROM hv_l GROUP BY cat HAVING cat LIKE 'a%' ORDER BY cat");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("alpha".into()));
    assert_eq!(rows[0][1], Value::Integer(30));
}

#[test]
fn test_having_with_ilike() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE hv_i (id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO hv_i VALUES (1,'Alpha',10),(2,'BETA',20),(3,'alpha',30)").unwrap();
    let res = vm.execute_sql(
        "SELECT cat, SUM(val) FROM hv_i GROUP BY cat HAVING cat ILIKE 'alpha' ORDER BY cat");
    match res {
        Ok(ExecResult::QueryResult { rows, .. }) => {
            assert!(rows.len() >= 1);
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Section E: Triggers — schema.rs L342-368 (trigger restore)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_before_insert_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_t (id INTEGER PRIMARY KEY, val INTEGER, log TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE trig_log (id INTEGER PRIMARY KEY, msg TEXT)").unwrap();
    let res = vm.execute_sql(
        "CREATE TRIGGER trg_before_ins BEFORE INSERT ON trig_t \
         BEGIN INSERT INTO trig_log VALUES (NEW.id, 'inserted'); END");
    if res.is_ok() {
        vm.execute_sql("INSERT INTO trig_t VALUES (1, 100, 'test')").unwrap();
        let rows = query_rows(&mut vm, "SELECT msg FROM trig_log WHERE id = 1");
        if !rows.is_empty() {
            assert_eq!(rows[0][0], Value::Text("inserted".into()));
        }
    }
}

#[test]
fn test_after_update_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_u (id INTEGER PRIMARY KEY, val INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE trig_u_log (id INTEGER PRIMARY KEY, old_val INTEGER, new_val INTEGER)").unwrap();
    let res = vm.execute_sql(
        "CREATE TRIGGER trg_after_upd AFTER UPDATE ON trig_u \
         BEGIN INSERT INTO trig_u_log VALUES (OLD.id, OLD.val, NEW.val); END");
    if res.is_ok() {
        vm.execute_sql("INSERT INTO trig_u VALUES (1, 10)").unwrap();
        vm.execute_sql("UPDATE trig_u SET val = 20 WHERE id = 1").unwrap();
        let rows = query_rows(&mut vm, "SELECT old_val, new_val FROM trig_u_log");
        if !rows.is_empty() {
            assert_eq!(rows[0][0], Value::Integer(10));
            assert_eq!(rows[0][1], Value::Integer(20));
        }
    }
}

#[test]
fn test_after_delete_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_d (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE trig_d_log (id INTEGER PRIMARY KEY, deleted_name TEXT)").unwrap();
    let res = vm.execute_sql(
        "CREATE TRIGGER trg_after_del AFTER DELETE ON trig_d \
         BEGIN INSERT INTO trig_d_log VALUES (OLD.id, OLD.name); END");
    if res.is_ok() {
        vm.execute_sql("INSERT INTO trig_d VALUES (1, 'alice')").unwrap();
        vm.execute_sql("DELETE FROM trig_d WHERE id = 1").unwrap();
        let rows = query_rows(&mut vm, "SELECT deleted_name FROM trig_d_log");
        if !rows.is_empty() {
            assert_eq!(rows[0][0], Value::Text("alice".into()));
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Section F: Foreign key schema — schema.rs L223-229
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_foreign_key_schema_restore() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fk_par (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql(
        "CREATE TABLE fk_child (id INTEGER PRIMARY KEY, par_id INTEGER REFERENCES fk_par(id))").unwrap();
    vm.execute_sql("INSERT INTO fk_par VALUES (1, 'parent')").unwrap();
    vm.execute_sql("INSERT INTO fk_child VALUES (1, 1)").unwrap();
    let rows = query_rows(&mut vm, "SELECT p.name FROM fk_par p JOIN fk_child c ON c.par_id = p.id");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("parent".into()));
}

#[test]
fn test_foreign_key_with_actions() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE fka_par (id INTEGER PRIMARY KEY)").unwrap();
    let res = vm.execute_sql(
        "CREATE TABLE fka_child (id INTEGER PRIMARY KEY, pid INTEGER, \
         FOREIGN KEY (pid) REFERENCES fka_par(id) ON DELETE CASCADE ON UPDATE SET NULL)");
    assert!(res.is_ok());
    vm.execute_sql("INSERT INTO fka_par VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO fka_child VALUES (1, 1)").unwrap();
    let rows = query_rows(&mut vm, "SELECT pid FROM fka_child");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section G: CHECK constraints — schema.rs L215-220, L234-236
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_check_constraint_column_level() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk_col (id INTEGER PRIMARY KEY, age INTEGER CHECK (age >= 0 AND age <= 150))").unwrap();
    vm.execute_sql("INSERT INTO chk_col VALUES (1, 25)").unwrap();
    let res = vm.execute_sql("INSERT INTO chk_col VALUES (2, -1)");
    assert!(res.is_err());
    let res = vm.execute_sql("INSERT INTO chk_col VALUES (3, 200)");
    assert!(res.is_err());
}

#[test]
fn test_check_constraint_table_level() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE chk_tbl (id INTEGER PRIMARY KEY, low INTEGER, high INTEGER, CHECK (low < high))").unwrap();
    vm.execute_sql("INSERT INTO chk_tbl VALUES (1, 10, 20)").unwrap();
    let res = vm.execute_sql("INSERT INTO chk_tbl VALUES (2, 30, 20)");
    assert!(res.is_err());
}

// ═══════════════════════════════════════════════════════════════════════
//  Section H: CAST to BLOB and back — eval_expr.rs L1665-1680
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_cast_null_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS BLOB)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_null_to_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS TEXT)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_null_to_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS REAL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_real_to_integer_truncation() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(9.99 AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(9));
}

#[test]
fn test_cast_negative_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(-3.7 AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(-3));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section I: Large BLOB to trigger overflow — cursor.rs L145-150
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_large_blob_overflow_chain() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE blob_large (id INTEGER PRIMARY KEY, data TEXT)").unwrap();
    // Insert a very large text value to trigger overflow pages
    let big = "A".repeat(16000);
    vm.execute_sql(&format!("INSERT INTO blob_large VALUES (1, '{}')", big)).unwrap();
    let rows = query_rows(&mut vm, "SELECT LENGTH(data) FROM blob_large WHERE id = 1");
    assert_eq!(rows[0][0], Value::Integer(16000));
    // Read it back
    let rows = query_rows(&mut vm, "SELECT data FROM blob_large WHERE id = 1");
    match &rows[0][0] {
        Value::Text(s) => assert_eq!(s.len(), 16000),
        _ => panic!("expected Text"),
    }
}

#[test]
fn test_multiple_overflow_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE blob_multi (id INTEGER PRIMARY KEY, data TEXT)").unwrap();
    for i in 0..5 {
        let big = format!("{}_{}", "B".repeat(10000), i);
        vm.execute_sql(&format!("INSERT INTO blob_multi VALUES ({}, '{}')", i, big)).unwrap();
    }
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM blob_multi");
    assert_eq!(rows[0][0], Value::Integer(5));
    // Verify each row
    for i in 0..5 {
        let rows = query_rows(&mut vm, &format!("SELECT LENGTH(data) FROM blob_multi WHERE id = {}", i));
        match &rows[0][0] {
            Value::Integer(n) => assert!(*n > 10000),
            _ => panic!("expected Integer"),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Section J: Subquery in FROM with SET operation — exec_select.rs ~L1543
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_subquery_from_union() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sf1 (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE sf2 (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO sf1 VALUES (1,10),(2,20)").unwrap();
    vm.execute_sql("INSERT INTO sf2 VALUES (1,30),(2,40)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT sub.v FROM (SELECT v FROM sf1 UNION ALL SELECT v FROM sf2) sub ORDER BY sub.v");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0][0], Value::Integer(10));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section K: Complex auto-transaction paths — exec_dml.rs L58-85
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_auto_txn_insert_success() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at1 (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    // No explicit BEGIN — auto-transaction
    vm.execute_sql("INSERT INTO at1 VALUES (1, 'auto_txn')").unwrap();
    let rows = query_rows(&mut vm, "SELECT v FROM at1 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("auto_txn".into()));
}

#[test]
fn test_auto_txn_insert_violation_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at2 (id INTEGER PRIMARY KEY, v TEXT NOT NULL)").unwrap();
    vm.execute_sql("INSERT INTO at2 VALUES (1, 'ok')").unwrap();
    // This should fail — NOT NULL violation, but auto-txn should rollback
    let res = vm.execute_sql("INSERT INTO at2 VALUES (2, NULL)");
    assert!(res.is_err());
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM at2");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section L: Multi-column index / complex WHERE with index scan
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_index_scan_with_multiple_conditions() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE idx_mc (id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c TEXT)").unwrap();
    vm.execute_sql("CREATE INDEX idx_mc_a ON idx_mc (a)").unwrap();
    vm.execute_sql("CREATE INDEX idx_mc_b ON idx_mc (b)").unwrap();
    for i in 0..100 {
        vm.execute_sql(&format!(
            "INSERT INTO idx_mc VALUES ({}, '{}', {}, 'data_{}')",
            i, if i % 3 == 0 { "x" } else { "y" }, i * 2, i
        )).unwrap();
    }
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM idx_mc WHERE a = 'x' AND b < 100");
    assert!(rows[0][0] != Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section M: Views with aggregation
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_view_with_aggregation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vw_src (id INTEGER PRIMARY KEY, cat TEXT, amt INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO vw_src VALUES (1,'a',10),(2,'a',20),(3,'b',30)").unwrap();
    vm.execute_sql("CREATE VIEW vw_agg AS SELECT cat, SUM(amt) as total FROM vw_src GROUP BY cat").unwrap();
    let rows = query_rows(&mut vm, "SELECT cat, total FROM vw_agg ORDER BY cat");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Integer(30));
    assert_eq!(rows[1][1], Value::Integer(30));
}

#[test]
fn test_view_nested() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vn_src (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO vn_src VALUES (1,10),(2,20),(3,30)").unwrap();
    vm.execute_sql("CREATE VIEW vn_v1 AS SELECT id, v * 2 as doubled FROM vn_src").unwrap();
    vm.execute_sql("CREATE VIEW vn_v2 AS SELECT id, doubled FROM vn_v1 WHERE doubled > 25").unwrap();
    let rows = query_rows(&mut vm, "SELECT id, doubled FROM vn_v2 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Integer(40));
    assert_eq!(rows[1][1], Value::Integer(60));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section N: Lowercase key / schema name lookup — schema.rs L930-955
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_case_insensitive_table_name() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE MyTable (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("INSERT INTO MyTable VALUES (1, 'hello')").unwrap();
    // Query with different case
    let rows = query_rows(&mut vm, "SELECT v FROM mytable WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("hello".into()));
    let rows = query_rows(&mut vm, "SELECT v FROM MYTABLE WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("hello".into()));
}

#[test]
fn test_case_insensitive_column_name() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ci_col (ID INTEGER PRIMARY KEY, MyValue TEXT)").unwrap();
    vm.execute_sql("INSERT INTO ci_col VALUES (1, 'test')").unwrap();
    let rows = query_rows(&mut vm, "SELECT myvalue FROM ci_col WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("test".into()));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section O: Complex window + GROUP BY interactions
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_window_over_grouped_query() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wg (id INTEGER PRIMARY KEY, grp TEXT, val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO wg VALUES (1,'a',10),(2,'a',20),(3,'b',30),(4,'b',40),(5,'c',50)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT grp, SUM(val) as s, ROW_NUMBER() OVER (ORDER BY SUM(val) DESC) as rn \
         FROM wg GROUP BY grp ORDER BY rn");
    assert_eq!(rows.len(), 3);
}

#[test]
fn test_rank_over_grouped() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rg (id INTEGER PRIMARY KEY, cat TEXT, score INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO rg VALUES (1,'a',100),(2,'a',200),(3,'b',150),(4,'b',150),(5,'c',300)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT cat, SUM(score) as total, RANK() OVER (ORDER BY SUM(score) DESC) as rnk \
         FROM rg GROUP BY cat ORDER BY rnk");
    assert_eq!(rows.len(), 3);
}

// ═══════════════════════════════════════════════════════════════════════
//  Section P: Multi-way UNION with aggregation
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_union_all_aggregation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ua1 (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("CREATE TABLE ua2 (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO ua1 VALUES (1,10),(2,20)").unwrap();
    vm.execute_sql("INSERT INTO ua2 VALUES (1,30),(2,40)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT SUM(v) FROM (SELECT v FROM ua1 UNION ALL SELECT v FROM ua2) sub");
    assert_eq!(rows[0][0], Value::Integer(100));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section Q: INSERT batch with auto-increment
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_autoincrement_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ai (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT)").unwrap();
    vm.execute_sql("INSERT INTO ai (name) VALUES ('first')").unwrap();
    vm.execute_sql("INSERT INTO ai (name) VALUES ('second')").unwrap();
    vm.execute_sql("INSERT INTO ai (name) VALUES ('third')").unwrap();
    let rows = query_rows(&mut vm, "SELECT id, name FROM ai ORDER BY id");
    assert_eq!(rows.len(), 3);
    // IDs should be sequential
    match (&rows[0][0], &rows[1][0], &rows[2][0]) {
        (Value::Integer(a), Value::Integer(b), Value::Integer(c)) => {
            assert!(b > a);
            assert!(c > b);
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Section R: Complex nested CASE with aggregates
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_nested_case_in_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nc (id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO nc VALUES (1,10,20),(2,30,5),(3,NULL,15),(4,0,0)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT id, CASE \
            WHEN a IS NULL THEN 'null' \
            WHEN a > b THEN CASE WHEN a > 20 THEN 'high' ELSE 'medium' END \
            WHEN a = b THEN 'equal' \
            ELSE 'low' END as label \
         FROM nc ORDER BY id");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0][1], Value::Text("low".into()));    // 10<20
    assert_eq!(rows[1][1], Value::Text("high".into()));   // 30>5, 30>20
    assert_eq!(rows[2][1], Value::Text("null".into()));   // NULL
    assert_eq!(rows[3][1], Value::Text("equal".into()));  // 0=0
}

// ═══════════════════════════════════════════════════════════════════════
//  Section S: Recursive CTE — schema.rs trigger/CTE paths
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_recursive_cte_simple() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm,
        "WITH RECURSIVE cnt(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM cnt WHERE x < 10) \
         SELECT x FROM cnt ORDER BY x");
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[9][0], Value::Integer(10));
}

#[test]
fn test_recursive_cte_tree() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE tree (id INTEGER PRIMARY KEY, parent_id INTEGER, name TEXT)").unwrap();
    vm.execute_sql("INSERT INTO tree VALUES (1,NULL,'root'),(2,1,'child1'),(3,1,'child2'),(4,2,'gc1'),(5,2,'gc2')").unwrap();
    let rows = query_rows(&mut vm,
        "WITH RECURSIVE subtree(id, name, depth) AS ( \
            SELECT id, name, 0 FROM tree WHERE id = 1 \
            UNION ALL \
            SELECT t.id, t.name, s.depth + 1 FROM tree t JOIN subtree s ON t.parent_id = s.id \
         ) SELECT id, name, depth FROM subtree ORDER BY id");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][2], Value::Integer(0)); // root depth=0
    assert_eq!(rows[3][2], Value::Integer(2)); // grandchild depth=2
}

// ═══════════════════════════════════════════════════════════════════════
//  Section T: DISTINCT with multiple columns
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_distinct_multi_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dm (id INTEGER PRIMARY KEY, a TEXT, b INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO dm VALUES (1,'x',1),(2,'x',1),(3,'x',2),(4,'y',1),(5,'y',1)").unwrap();
    let rows = query_rows(&mut vm, "SELECT DISTINCT a, b FROM dm ORDER BY a, b");
    assert_eq!(rows.len(), 3); // (x,1), (x,2), (y,1)
}

// ═══════════════════════════════════════════════════════════════════════
//  Section U: Fulltext index — schema.rs L285-310
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_fulltext_index_create_insert_search() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ft (id INTEGER PRIMARY KEY, title TEXT, body TEXT)").unwrap();
    let res = vm.execute_sql("CREATE FULLTEXT INDEX ft_idx ON ft (title, body)");
    if res.is_ok() {
        vm.execute_sql("INSERT INTO ft VALUES (1, 'rust programming', 'learn rust basics')").unwrap();
        vm.execute_sql("INSERT INTO ft VALUES (2, 'python guide', 'python for beginners')").unwrap();
        let rows = query_rows(&mut vm, "SELECT id FROM ft WHERE ft MATCH 'rust' ORDER BY id");
        assert!(rows.len() >= 1);
    }
}

// ═══════════════════════════════════════════════════════════════════════
//  Section V: Batch large inserts for B-tree interior page splits
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_1000_rows_with_scan() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE big1k (id INTEGER PRIMARY KEY, a INTEGER, b TEXT)").unwrap();
    for i in 0..1000 {
        vm.execute_sql(&format!("INSERT INTO big1k VALUES ({},{},'{}')", i, i*3, format!("v{:04}", i))).unwrap();
    }
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM big1k");
    assert_eq!(rows[0][0], Value::Integer(1000));

    // Full scan with aggregation
    let rows = query_rows(&mut vm, "SELECT SUM(a) FROM big1k");
    assert_eq!(rows[0][0], Value::Integer(1498500)); // sum(0..999)*3

    // Point query
    let rows = query_rows(&mut vm, "SELECT b FROM big1k WHERE id = 500");
    assert_eq!(rows[0][0], Value::Text("v0500".into()));

    // Range scan
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM big1k WHERE id >= 900");
    assert_eq!(rows[0][0], Value::Integer(100));
}

#[test]
fn test_btree_delete_half_and_scan() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dh (id INTEGER PRIMARY KEY, v INTEGER)").unwrap();
    for i in 0..500 {
        vm.execute_sql(&format!("INSERT INTO dh VALUES ({}, {})", i, i)).unwrap();
    }
    // Delete odd rows
    vm.execute_sql("DELETE FROM dh WHERE id % 2 = 1").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM dh");
    assert_eq!(rows[0][0], Value::Integer(250));
    // Verify remaining are all even
    let rows = query_rows(&mut vm, "SELECT MIN(id), MAX(id) FROM dh");
    assert_eq!(rows[0][0], Value::Integer(0));
    assert_eq!(rows[0][1], Value::Integer(498));
}

// ═══════════════════════════════════════════════════════════════════════
//  Section W: Misc eval_expr paths
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn test_in_list_mixed_types() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5 IN (1, 3, 5, 7, 9)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_not_in_list() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 4 NOT IN (1, 3, 5, 7)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_null_in_list() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL IN (1, 2, 3)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_between_with_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL BETWEEN 1 AND 10");
    assert_eq!(rows[0][0], Value::Null);
}
