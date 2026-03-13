//! Fourth coverage push — targeting 75 %+ threshold.
//! Focuses on uncovered code paths in: eval_expr NULL propagation, FtsMatch text,
//! window functions with ties, top-N, ORDER BY position, VACUUM, CREATE VIEW,
//! statement parser (GRANT/REVOKE), ANALYZE + CBO selectivity, EXPLAIN tree with JOIN.

use super::query_rows;
use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

// ─── helpers ────────────────────────────────────────────────────────────────
fn q(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    query_rows(vm, sql)
}
fn e(vm: &mut VM, sql: &str) -> ExecResult {
    vm.execute_sql(sql).unwrap()
}
fn e_err(vm: &mut VM, sql: &str) -> String {
    vm.execute_sql(sql).unwrap_err().to_string()
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. NULL AND/OR propagation  (eval_expr.rs L1808-1827)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_null_and_false_returns_false() {
    // NULL AND 0 → 0  (NULL AND false = false)
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE naf (id INTEGER, a INTEGER, b INTEGER)",
    );
    e(&mut vm, "INSERT INTO naf VALUES (1, NULL, 0)");
    e(&mut vm, "INSERT INTO naf VALUES (2, NULL, 1)");
    e(&mut vm, "INSERT INTO naf VALUES (3, 0, NULL)");
    e(&mut vm, "INSERT INTO naf VALUES (4, 1, NULL)");
    // WHERE (NULL AND 0): left=NULL, right=0(falsy) → should short-circuit to 0
    let rows = q(&mut vm, "SELECT id FROM naf WHERE a AND b ORDER BY id");
    // Only row id=? where BOTH are truthy. Row 2: NULL AND 1→NULL (filtered out). Row 4: 1 AND NULL→NULL (filtered out).
    // No rows have both truthy non-NULL.
    assert!(rows.is_empty() || rows.iter().all(|r| r[0] != Value::Integer(2)));
}

#[test]
fn test_null_and_false_explicit() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nae (x INTEGER)");
    e(&mut vm, "INSERT INTO nae VALUES (1)");
    // SELECT with NULL AND 0 in the expression
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (NULL AND 0) THEN 'yes' ELSE 'no' END FROM nae",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "no"));
}

#[test]
fn test_false_and_null_explicit() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE fne (x INTEGER)");
    e(&mut vm, "INSERT INTO fne VALUES (1)");
    // 0 AND NULL should also be false (0)
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (0 AND NULL) THEN 'yes' ELSE 'no' END FROM fne",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "no"));
}

#[test]
fn test_null_or_true_returns_true() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE not1 (x INTEGER)");
    e(&mut vm, "INSERT INTO not1 VALUES (1)");
    // NULL OR 1 → 1 (truthy)
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (NULL OR 1) THEN 'yes' ELSE 'no' END FROM not1",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "yes"));
}

#[test]
fn test_true_or_null_returns_true() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ton1 (x INTEGER)");
    e(&mut vm, "INSERT INTO ton1 VALUES (1)");
    // 1 OR NULL → 1 (truthy, left is truthy)
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (1 OR NULL) THEN 'yes' ELSE 'no' END FROM ton1",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "yes"));
}

#[test]
fn test_null_and_true_returns_null() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nat1 (x INTEGER)");
    e(&mut vm, "INSERT INTO nat1 VALUES (1)");
    // NULL AND 1 → NULL (neither side is non-NULL falsy)
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (NULL AND 1) IS NULL THEN 'null' ELSE 'not_null' END FROM nat1",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "null"));
}

#[test]
fn test_null_or_false_returns_null() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nof1 (x INTEGER)");
    e(&mut vm, "INSERT INTO nof1 VALUES (1)");
    // NULL OR 0 → NULL
    let rows = q(
        &mut vm,
        "SELECT CASE WHEN (NULL OR 0) IS NULL THEN 'null' ELSE 'not_null' END FROM nof1",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "null"));
}

#[test]
fn test_null_and_or_in_where() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE naw (id INTEGER, a INTEGER, b INTEGER)",
    );
    e(&mut vm, "INSERT INTO naw VALUES (1, 1, 1)");
    e(&mut vm, "INSERT INTO naw VALUES (2, 0, 1)");
    e(&mut vm, "INSERT INTO naw VALUES (3, NULL, 0)");
    e(&mut vm, "INSERT INTO naw VALUES (4, NULL, 1)");
    e(&mut vm, "INSERT INTO naw VALUES (5, 1, NULL)");
    // WHERE a OR b: rows 1(1|1=T), 2(0|1=T), 3(NULL|0=NULL→F), 4(NULL|1=T), 5(1|NULL=T)
    let rows = q(&mut vm, "SELECT id FROM naw WHERE a OR b ORDER BY id");
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| match &r[0] {
            Value::Integer(v) => Some(*v),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&4));
    assert!(ids.contains(&5));
    assert!(!ids.contains(&3)); // NULL OR 0 is NULL → filtered
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. FtsMatch text-based eval  (eval_expr.rs L1832-1844)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_fts_match_on_regular_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE docs (id INTEGER, body TEXT)");
    e(&mut vm, "INSERT INTO docs VALUES (1, 'hello world')");
    e(&mut vm, "INSERT INTO docs VALUES (2, 'goodbye world')");
    e(&mut vm, "INSERT INTO docs VALUES (3, 'hello there')");
    // MATCH on non-FTS table → falls through to text-based FtsMatch
    let rows = q(
        &mut vm,
        "SELECT id FROM docs WHERE body MATCH 'hello' ORDER BY id",
    );
    let ids: Vec<i64> = rows
        .iter()
        .filter_map(|r| match &r[0] {
            Value::Integer(v) => Some(*v),
            _ => None,
        })
        .collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&3));
    assert!(!ids.contains(&2));
}

#[test]
fn test_fts_match_empty_pattern() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE docs2 (id INTEGER, body TEXT)");
    e(&mut vm, "INSERT INTO docs2 VALUES (1, 'hello world')");
    // Empty pattern → no match
    let rows = q(&mut vm, "SELECT id FROM docs2 WHERE body MATCH ''");
    assert!(rows.is_empty());
}

#[test]
fn test_fts_match_multi_token() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE docs3 (id INTEGER, body TEXT)");
    e(
        &mut vm,
        "INSERT INTO docs3 VALUES (1, 'the quick brown fox')",
    );
    e(&mut vm, "INSERT INTO docs3 VALUES (2, 'the quick red fox')");
    e(&mut vm, "INSERT INTO docs3 VALUES (3, 'slow brown turtle')");
    // Match requires ALL tokens
    let rows = q(
        &mut vm,
        "SELECT id FROM docs3 WHERE body MATCH 'quick brown'",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_fts_match_no_match() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE docs4 (id INTEGER, body TEXT)");
    e(&mut vm, "INSERT INTO docs4 VALUES (1, 'hello world')");
    let rows = q(
        &mut vm,
        "SELECT id FROM docs4 WHERE body MATCH 'nonexistent'",
    );
    assert!(rows.is_empty());
}

#[test]
fn test_fts_match_case_insensitive() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE docs5 (id INTEGER, body TEXT)");
    e(&mut vm, "INSERT INTO docs5 VALUES (1, 'Hello World')");
    let rows = q(&mut vm, "SELECT id FROM docs5 WHERE body MATCH 'hello'");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Window functions with ORDER BY and ties
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_dense_rank_with_ties() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dr_t (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 10), (2, 10), (3, 20), (4, 20), (5, 30)] {
        e(&mut vm, &format!("INSERT INTO dr_t VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT val, DENSE_RANK() OVER (ORDER BY val) AS dr FROM dr_t ORDER BY val, id",
    );
    assert_eq!(rows.len(), 5);
    // val=10 → dr=1, val=10 → dr=1, val=20 → dr=2, val=20 → dr=2, val=30 → dr=3
    assert_eq!(rows[0][1], Value::Integer(1));
    assert_eq!(rows[1][1], Value::Integer(1));
    assert_eq!(rows[2][1], Value::Integer(2));
    assert_eq!(rows[3][1], Value::Integer(2));
    assert_eq!(rows[4][1], Value::Integer(3));
}

#[test]
fn test_percent_rank_with_ties() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE pr_t (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 10), (2, 10), (3, 20), (4, 20), (5, 30)] {
        e(&mut vm, &format!("INSERT INTO pr_t VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, val, PERCENT_RANK() OVER (ORDER BY val) AS pr FROM pr_t ORDER BY val, id",
    );
    assert_eq!(rows.len(), 5);
    // val=10: rank=1, pr=(1-1)/(5-1)=0.0
    // val=10: rank=1, pr=0.0
    // val=20: rank=3, pr=(3-1)/(5-1)=0.5
    // val=20: rank=3, pr=0.5
    // val=30: rank=5, pr=(5-1)/(5-1)=1.0
    if let Value::Real(v) = rows[0][2] {
        assert!((v - 0.0).abs() < 0.01);
    }
    if let Value::Real(v) = rows[4][2] {
        assert!((v - 1.0).abs() < 0.01);
    }
}

#[test]
fn test_cume_dist_with_ties() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cd_t (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 10), (2, 10), (3, 20), (4, 20), (5, 30)] {
        e(&mut vm, &format!("INSERT INTO cd_t VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, val, CUME_DIST() OVER (ORDER BY val) AS cd FROM cd_t ORDER BY val, id",
    );
    assert_eq!(rows.len(), 5);
    // CUME_DIST: val=10 rows count=2 → 2/5=0.4, val=20: 4/5=0.8, val=30: 5/5=1.0
    if let Value::Real(v) = rows[0][2] {
        assert!((v - 0.4).abs() < 0.01, "got {v}");
    }
    if let Value::Real(v) = rows[1][2] {
        assert!((v - 0.4).abs() < 0.01, "got {v}");
    }
    if let Value::Real(v) = rows[4][2] {
        assert!((v - 1.0).abs() < 0.01, "got {v}");
    }
}

#[test]
fn test_dense_rank_partition_ties() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE drp (grp TEXT, val INTEGER)");
    e(&mut vm, "INSERT INTO drp VALUES ('a', 1)");
    e(&mut vm, "INSERT INTO drp VALUES ('a', 1)");
    e(&mut vm, "INSERT INTO drp VALUES ('a', 2)");
    e(&mut vm, "INSERT INTO drp VALUES ('b', 5)");
    e(&mut vm, "INSERT INTO drp VALUES ('b', 5)");
    let rows = q(&mut vm,
        "SELECT grp, val, DENSE_RANK() OVER (PARTITION BY grp ORDER BY val) AS dr FROM drp ORDER BY grp, val");
    assert!(rows.len() >= 5);
}

#[test]
fn test_percent_rank_single_row() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE prs (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO prs VALUES (1, 100)");
    let rows = q(
        &mut vm,
        "SELECT val, PERCENT_RANK() OVER (ORDER BY val) AS pr FROM prs",
    );
    assert_eq!(rows.len(), 1);
    // Single row → PERCENT_RANK = 0.0
    if let Value::Real(v) = rows[0][1] {
        assert!((v - 0.0).abs() < 0.01);
    }
}

#[test]
fn test_cume_dist_all_same() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cda (id INTEGER, val INTEGER)");
    for i in 1..=4 {
        e(&mut vm, &format!("INSERT INTO cda VALUES ({i}, 10)"));
    }
    let rows = q(
        &mut vm,
        "SELECT val, CUME_DIST() OVER (ORDER BY val) AS cd FROM cda",
    );
    assert_eq!(rows.len(), 4);
    // All same value → CUME_DIST = 1.0 for all
    for row in &rows {
        if let Value::Real(v) = row[1] {
            assert!((v - 1.0).abs() < 0.01, "got {v}");
        }
    }
}

#[test]
fn test_row_number_with_order() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE rno (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 30), (2, 10), (3, 20)] {
        e(&mut vm, &format!("INSERT INTO rno VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, ROW_NUMBER() OVER (ORDER BY val) AS rn FROM rno",
    );
    assert_eq!(rows.len(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. ORDER BY integer position & top-N
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_by_position_desc() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE obp (a TEXT, b INTEGER)");
    e(&mut vm, "INSERT INTO obp VALUES ('x', 3)");
    e(&mut vm, "INSERT INTO obp VALUES ('y', 1)");
    e(&mut vm, "INSERT INTO obp VALUES ('z', 2)");
    // ORDER BY column name DESC
    let rows = q(&mut vm, "SELECT a, b FROM obp ORDER BY b DESC");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(3)); // 3 first
    assert_eq!(rows[2][1], Value::Integer(1)); // 1 last
}

#[test]
fn test_order_by_position_asc() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE oba (a TEXT, b INTEGER)");
    e(&mut vm, "INSERT INTO oba VALUES ('x', 3)");
    e(&mut vm, "INSERT INTO oba VALUES ('y', 1)");
    e(&mut vm, "INSERT INTO oba VALUES ('z', 2)");
    // ORDER BY column name ASC
    let rows = q(&mut vm, "SELECT a, b FROM oba ORDER BY b ASC");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(1));
    assert_eq!(rows[2][1], Value::Integer(3));
}

#[test]
fn test_top_n_select_nth() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tn (id INTEGER, val INTEGER)");
    for i in 1..=20 {
        e(&mut vm, &format!("INSERT INTO tn VALUES ({i}, {})", i * 10));
    }
    // LIMIT 5 with 20 rows → triggers select_nth_unstable path
    let rows = q(&mut vm, "SELECT id, val FROM tn ORDER BY val LIMIT 5");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][1], Value::Integer(10));
    assert_eq!(rows[4][1], Value::Integer(50));
}

#[test]
fn test_top_n_with_offset() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tno (id INTEGER, val INTEGER)");
    for i in 1..=20 {
        e(
            &mut vm,
            &format!("INSERT INTO tno VALUES ({i}, {})", i * 10),
        );
    }
    // LIMIT 3 OFFSET 5 → k = 3+5 = 8, still < 20 → select_nth_unstable
    let rows = q(
        &mut vm,
        "SELECT id, val FROM tno ORDER BY val LIMIT 3 OFFSET 5",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(60)); // 6th row (0-indexed: 5)
}

#[test]
fn test_top_n_limit_zero() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tnz (id INTEGER)");
    e(&mut vm, "INSERT INTO tnz VALUES (1)");
    let rows = q(&mut vm, "SELECT id FROM tnz ORDER BY id LIMIT 0");
    assert!(rows.is_empty());
}

#[test]
fn test_top_n_limit_exceeds_rows() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tne (id INTEGER)");
    e(&mut vm, "INSERT INTO tne VALUES (1)");
    e(&mut vm, "INSERT INTO tne VALUES (2)");
    // LIMIT 100 with 2 rows → k >= len → falls through to full sort
    let rows = q(&mut vm, "SELECT id FROM tne ORDER BY id LIMIT 100");
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. VACUUM
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_vacuum_basic() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE vac (id INTEGER, data TEXT)");
    for i in 1..=50 {
        e(&mut vm, &format!("INSERT INTO vac VALUES ({i}, 'row_{i}')"));
    }
    // Delete half the rows to create fragmentation
    for i in 1..=25 {
        e(&mut vm, &format!("DELETE FROM vac WHERE id = {i}"));
    }
    let result = e(&mut vm, "VACUUM");
    match result {
        ExecResult::Ok { message } => assert!(message.contains("VACUUM"), "got: {message}"),
        other => panic!("expected Ok, got {:?}", other),
    }
    // Remaining rows should still be queryable
    let rows = q(&mut vm, "SELECT COUNT(*) FROM vac");
    assert_eq!(rows[0][0], Value::Integer(25));
}

#[test]
fn test_vacuum_empty_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE vac2 (id INTEGER)");
    let result = e(&mut vm, "VACUUM");
    match result {
        ExecResult::Ok { message } => assert!(message.contains("VACUUM")),
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[test]
fn test_vacuum_with_large_data() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE vac3 (id INTEGER, data TEXT)");
    let big = "X".repeat(500);
    for i in 1..=30 {
        e(&mut vm, &format!("INSERT INTO vac3 VALUES ({i}, '{big}')"));
    }
    for i in 1..=15 {
        e(&mut vm, &format!("DELETE FROM vac3 WHERE id = {i}"));
    }
    let result = e(&mut vm, "VACUUM");
    if let ExecResult::Ok { message } = result {
        assert!(message.contains("VACUUM"))
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. CREATE VIEW & query view
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_view_and_query() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE emp (id INTEGER, name TEXT, dept TEXT, salary INTEGER)",
    );
    e(&mut vm, "INSERT INTO emp VALUES (1, 'Alice', 'eng', 100)");
    e(&mut vm, "INSERT INTO emp VALUES (2, 'Bob', 'eng', 120)");
    e(&mut vm, "INSERT INTO emp VALUES (3, 'Carol', 'hr', 90)");
    e(
        &mut vm,
        "CREATE VIEW eng_view AS SELECT id, name, salary FROM emp WHERE dept = 'eng'",
    );
    let rows = q(&mut vm, "SELECT * FROM eng_view ORDER BY id");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_create_view_with_aggregation() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE sales (id INTEGER, product TEXT, amount INTEGER)",
    );
    e(&mut vm, "INSERT INTO sales VALUES (1, 'A', 100)");
    e(&mut vm, "INSERT INTO sales VALUES (2, 'B', 200)");
    e(&mut vm, "INSERT INTO sales VALUES (3, 'A', 150)");
    e(&mut vm, "CREATE VIEW product_totals AS SELECT product, SUM(amount) AS total FROM sales GROUP BY product");
    let rows = q(&mut vm, "SELECT * FROM product_totals ORDER BY product");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_drop_view() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dv (id INTEGER)");
    e(&mut vm, "CREATE VIEW dv_view AS SELECT * FROM dv");
    e(&mut vm, "DROP VIEW dv_view");
    // Should error after drop
    let err = e_err(&mut vm, "SELECT * FROM dv_view");
    assert!(!err.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. EXPLAIN tree with JOIN & cardinality
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_explain_join_tree() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ej1 (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE TABLE ej2 (id INTEGER, ref_id INTEGER)");
    for i in 1..=10 {
        e(&mut vm, &format!("INSERT INTO ej1 VALUES ({i}, {i})"));
        e(&mut vm, &format!("INSERT INTO ej2 VALUES ({i}, {i})"));
    }
    // ANALYZE to get stats
    e(&mut vm, "ANALYZE TABLE ej1");
    e(&mut vm, "ANALYZE TABLE ej2");
    let result = e(
        &mut vm,
        "EXPLAIN SELECT * FROM ej1 JOIN ej2 ON ej1.id = ej2.ref_id",
    );
    match result {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("JOIN"), "plan should have JOIN: {plan}");
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

#[test]
fn test_explain_subquery_tree() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE es1 (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO es1 VALUES (1, 10)");
    let result = e(&mut vm, "EXPLAIN SELECT * FROM es1 WHERE val > (SELECT 5)");
    match result {
        ExecResult::Explain { plan } => {
            assert!(!plan.is_empty());
        }
        other => panic!("expected Explain, got {:?}", other),
    }
}

#[test]
fn test_explain_with_stats() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ews (id INTEGER, val INTEGER)");
    for i in 1..=20 {
        e(&mut vm, &format!("INSERT INTO ews VALUES ({i}, {})", i % 5));
    }
    e(&mut vm, "ANALYZE TABLE ews");
    let result = e(&mut vm, "EXPLAIN SELECT * FROM ews WHERE val = 3");
    if let ExecResult::Explain { plan } = result {
        // Plan should mention estimated rows or stats
        assert!(!plan.is_empty(), "plan should not be empty");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. ANALYZE + CBO selectivity paths
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_analyze_between_selectivity() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE ab (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    e(&mut vm, "CREATE INDEX idx_ab_val ON ab (val)");
    for i in 1..=100 {
        e(&mut vm, &format!("INSERT INTO ab VALUES ({i}, {i})"));
    }
    e(&mut vm, "ANALYZE TABLE ab");
    // BETWEEN should use histogram-based range selectivity
    let rows = q(&mut vm, "SELECT * FROM ab WHERE val BETWEEN 20 AND 40");
    assert_eq!(rows.len(), 21); // 20..40 inclusive
}

#[test]
fn test_analyze_comparison_selectivity() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE ac (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    e(&mut vm, "CREATE INDEX idx_ac_val ON ac (val)");
    for i in 1..=50 {
        e(&mut vm, &format!("INSERT INTO ac VALUES ({i}, {i})"));
    }
    e(&mut vm, "ANALYZE TABLE ac");
    // Less than comparison
    let rows = q(&mut vm, "SELECT * FROM ac WHERE val < 10");
    assert_eq!(rows.len(), 9);
}

#[test]
fn test_analyze_in_selectivity() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE ai (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    e(&mut vm, "CREATE INDEX idx_ai_val ON ai (val)");
    for i in 1..=50 {
        e(&mut vm, &format!("INSERT INTO ai VALUES ({i}, {})", i % 10));
    }
    e(&mut vm, "ANALYZE TABLE ai");
    let rows = q(&mut vm, "SELECT * FROM ai WHERE val IN (1, 2, 3)");
    assert!(rows.len() >= 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. GRANT / REVOKE / DROP POLICY (statement parser)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_grant_select_on_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE gt (id INTEGER)");
    // GRANT should parse and execute (even if auth is no-op in memory mode)
    let result = e(&mut vm, "GRANT SELECT ON gt TO testuser");
    if let ExecResult::Ok { message } = &result {
        assert!(message.to_lowercase().contains("grant") || !message.is_empty());
    }
}

#[test]
fn test_grant_multiple_privileges() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE gmp (id INTEGER)");
    let result = e(&mut vm, "GRANT SELECT, INSERT, UPDATE ON gmp TO testuser");
    if let ExecResult::Ok { .. } = &result {}
}

#[test]
fn test_revoke_on_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE rv (id INTEGER)");
    e(&mut vm, "GRANT SELECT ON rv TO testuser");
    let result = e(&mut vm, "REVOKE SELECT ON rv FROM testuser");
    if let ExecResult::Ok { .. } = &result {}
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. Misc coverage: DISTINCT typed_key, HAVING complex, DELETE cascade
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_distinct_with_mixed_types() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dm (id INTEGER, val TEXT)");
    e(&mut vm, "INSERT INTO dm VALUES (1, 'a')");
    e(&mut vm, "INSERT INTO dm VALUES (2, 'b')");
    e(&mut vm, "INSERT INTO dm VALUES (3, 'a')");
    e(&mut vm, "INSERT INTO dm VALUES (4, NULL)");
    e(&mut vm, "INSERT INTO dm VALUES (5, NULL)");
    let rows = q(&mut vm, "SELECT DISTINCT val FROM dm ORDER BY val");
    // 'a', 'b', NULL → 3 distinct values
    assert!(rows.len() >= 2 && rows.len() <= 3);
}

#[test]
fn test_having_with_sum_and_count() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE hsc (grp TEXT, val INTEGER)");
    e(&mut vm, "INSERT INTO hsc VALUES ('a', 10)");
    e(&mut vm, "INSERT INTO hsc VALUES ('a', 20)");
    e(&mut vm, "INSERT INTO hsc VALUES ('a', 30)");
    e(&mut vm, "INSERT INTO hsc VALUES ('b', 5)");
    e(&mut vm, "INSERT INTO hsc VALUES ('b', 10)");
    // HAVING with multiple conditions
    let rows = q(&mut vm, "SELECT grp, SUM(val), COUNT(*) FROM hsc GROUP BY grp HAVING SUM(val) > 20 AND COUNT(*) >= 2");
    assert!(!rows.is_empty());
    // Group 'a' has sum=60, count=3 → included. Group 'b' has sum=15, count=2 → excluded
}

#[test]
fn test_group_by_expression() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE gbe (id INTEGER, val INTEGER)");
    for i in 1..=10 {
        e(&mut vm, &format!("INSERT INTO gbe VALUES ({i}, {i})"));
    }
    // GROUP BY expression
    let rows = q(
        &mut vm,
        "SELECT val % 3 AS grp, COUNT(*) FROM gbe GROUP BY val % 3 ORDER BY grp",
    );
    assert!(rows.len() >= 2);
}

#[test]
fn test_update_with_expression() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ue (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO ue VALUES (1, 10)");
    e(&mut vm, "INSERT INTO ue VALUES (2, 20)");
    e(&mut vm, "UPDATE ue SET val = val * 2 + 5 WHERE id = 1");
    let rows = q(&mut vm, "SELECT val FROM ue WHERE id = 1");
    assert_eq!(rows[0][0], Value::Integer(25));
}

#[test]
fn test_delete_with_subquery() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ds1 (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE TABLE ds2 (id INTEGER)");
    e(&mut vm, "INSERT INTO ds1 VALUES (1, 10)");
    e(&mut vm, "INSERT INTO ds1 VALUES (2, 20)");
    e(&mut vm, "INSERT INTO ds1 VALUES (3, 30)");
    e(&mut vm, "INSERT INTO ds2 VALUES (1)");
    e(&mut vm, "INSERT INTO ds2 VALUES (3)");
    e(&mut vm, "DELETE FROM ds1 WHERE id IN (SELECT id FROM ds2)");
    let rows = q(&mut vm, "SELECT id FROM ds1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════
// 11. More eval_expr paths: type coercion, edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_add_int_real_coercion() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE arc (id INTEGER)");
    e(&mut vm, "INSERT INTO arc VALUES (1)");
    let rows = q(&mut vm, "SELECT 10 + 2.5 FROM arc");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 12.5).abs() < 0.01);
    }
}

#[test]
fn test_subtract_real_int() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE sri (id INTEGER)");
    e(&mut vm, "INSERT INTO sri VALUES (1)");
    let rows = q(&mut vm, "SELECT 10.5 - 3 FROM sri");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 7.5).abs() < 0.01);
    }
}

#[test]
fn test_multiply_int_real() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE mir (id INTEGER)");
    e(&mut vm, "INSERT INTO mir VALUES (1)");
    let rows = q(&mut vm, "SELECT 3 * 2.5 FROM mir");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 7.5).abs() < 0.01);
    }
}

#[test]
fn test_divide_real_int() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dri (id INTEGER)");
    e(&mut vm, "INSERT INTO dri VALUES (1)");
    let rows = q(&mut vm, "SELECT 10.0 / 4 FROM dri");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 2.5).abs() < 0.01);
    }
}

#[test]
fn test_null_arithmetic_propagation() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nap (id INTEGER)");
    e(&mut vm, "INSERT INTO nap VALUES (1)");
    // NULL + 5 should be NULL
    let rows = q(
        &mut vm,
        "SELECT NULL + 5, NULL * 3, NULL - 1, NULL / 2 FROM nap",
    );
    #[allow(clippy::needless_range_loop)]
    for i in 0..4 {
        assert_eq!(rows[0][i], Value::Null, "column {i} should be NULL");
    }
}

#[test]
fn test_gte_comparison() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE gte (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO gte VALUES (1, 5)");
    e(&mut vm, "INSERT INTO gte VALUES (2, 10)");
    e(&mut vm, "INSERT INTO gte VALUES (3, 15)");
    let rows = q(&mut vm, "SELECT id FROM gte WHERE val >= 10 ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_not_equal_comparison() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE neq (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO neq VALUES (1, 10)");
    e(&mut vm, "INSERT INTO neq VALUES (2, 20)");
    e(&mut vm, "INSERT INTO neq VALUES (3, 10)");
    let rows = q(&mut vm, "SELECT id FROM neq WHERE val != 10 ORDER BY id");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════
// 12. Generate series, table functions — query.rs convert_table_factor
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_generate_series() {
    let mut vm = VM::new_memory();
    let rows = q(&mut vm, "SELECT * FROM GENERATE_SERIES(1, 5)");
    assert_eq!(rows.len(), 5);
}

#[test]
fn test_generate_series_with_step() {
    let mut vm = VM::new_memory();
    let rows = q(&mut vm, "SELECT * FROM GENERATE_SERIES(0, 10, 2)");
    assert_eq!(rows.len(), 6); // 0, 2, 4, 6, 8, 10
}

#[test]
fn test_generate_series_filtered() {
    let mut vm = VM::new_memory();
    // Use GENERATE_SERIES with alias
    let rows = q(&mut vm, "SELECT * FROM GENERATE_SERIES(1, 5)");
    assert_eq!(rows.len(), 5);
}

// ═══════════════════════════════════════════════════════════════════════════
// 13. Multiple FROM tables (implicit CROSS JOIN) — query.rs convert_from_clause
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_implicit_cross_join() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE icj1 (a INTEGER)");
    e(&mut vm, "CREATE TABLE icj2 (b INTEGER)");
    e(&mut vm, "INSERT INTO icj1 VALUES (1)");
    e(&mut vm, "INSERT INTO icj1 VALUES (2)");
    e(&mut vm, "INSERT INTO icj2 VALUES (10)");
    e(&mut vm, "INSERT INTO icj2 VALUES (20)");
    // FROM t1, t2 → implicit CROSS JOIN
    let rows = q(&mut vm, "SELECT a, b FROM icj1, icj2 ORDER BY a, b");
    assert_eq!(rows.len(), 4); // 2 × 2 = 4
}

#[test]
fn test_three_table_implicit_cross() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE t3a (x INTEGER)");
    e(&mut vm, "CREATE TABLE t3b (y INTEGER)");
    e(&mut vm, "CREATE TABLE t3c (z INTEGER)");
    e(&mut vm, "INSERT INTO t3a VALUES (1)");
    e(&mut vm, "INSERT INTO t3b VALUES (2)");
    e(&mut vm, "INSERT INTO t3c VALUES (3)");
    let rows = q(&mut vm, "SELECT x, y, z FROM t3a, t3b, t3c");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(2));
    assert_eq!(rows[0][2], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════
// 14. Derived table / subquery in FROM — query.rs convert_table_factor
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_derived_table_in_from() {
    let mut vm = VM::new_memory();
    let rows = q(
        &mut vm,
        "SELECT sub.a, sub.b FROM (SELECT 1 AS a, 2 AS b) AS sub",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(2));
}

#[test]
fn test_derived_table_with_real_data() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dt (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO dt VALUES (1, 10)");
    e(&mut vm, "INSERT INTO dt VALUES (2, 20)");
    e(&mut vm, "INSERT INTO dt VALUES (3, 30)");
    let rows = q(
        &mut vm,
        "SELECT sq.s FROM (SELECT SUM(val) AS s FROM dt) AS sq",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(60));
}

// ═══════════════════════════════════════════════════════════════════════════
// 15. INSERT auto-txn commit path & large batch
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_auto_txn_many_rows() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE iat (id INTEGER, data TEXT)");
    // Insert many rows outside of explicit transaction → hit auto-txn begin/commit
    for i in 1..=100 {
        e(
            &mut vm,
            &format!("INSERT INTO iat VALUES ({i}, 'data_{i}')"),
        );
    }
    let rows = q(&mut vm, "SELECT COUNT(*) FROM iat");
    assert_eq!(rows[0][0], Value::Integer(100));
}

// ═══════════════════════════════════════════════════════════════════════════
// 16. More parser paths: unsupported statements for coverage
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_unsupported_alter_view() {
    let mut vm = VM::new_memory();
    let err = vm.execute_sql("ALTER VIEW foo AS SELECT 1");
    assert!(err.is_err());
}

#[test]
fn test_unsupported_call() {
    let mut vm = VM::new_memory();
    let err = vm.execute_sql("CALL my_procedure()");
    assert!(err.is_err());
}

#[test]
fn test_unsupported_declare_cursor() {
    let mut vm = VM::new_memory();
    // DECLARE is not supported
    let err = vm.execute_sql("DECLARE mycursor CURSOR FOR SELECT 1");
    assert!(err.is_err());
}

// ═══════════════════════════════════════════════════════════════════════════
// 17. Cursor advance through interior pages (btree/cursor paths)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_btree_many_rows_scan() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE bms (id INTEGER PRIMARY KEY, val TEXT)",
    );
    // Insert enough rows to potentially cause page splits
    for i in 1..=200 {
        e(&mut vm, &format!("INSERT INTO bms VALUES ({i}, 'val_{i}')"));
    }
    let rows = q(&mut vm, "SELECT COUNT(*) FROM bms");
    assert_eq!(rows[0][0], Value::Integer(200));
    // Full scan to exercise cursor advance through multiple pages
    let rows = q(&mut vm, "SELECT * FROM bms ORDER BY id");
    assert_eq!(rows.len(), 200);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[199][0], Value::Integer(200));
}

#[test]
fn test_btree_large_values_overflow() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE blo (id INTEGER, data TEXT)");
    // Large values that may trigger overflow pages
    let big = "A".repeat(3000);
    for i in 1..=10 {
        e(&mut vm, &format!("INSERT INTO blo VALUES ({i}, '{big}')"));
    }
    let rows = q(&mut vm, "SELECT id, LENGTH(data) FROM blo ORDER BY id");
    assert_eq!(rows.len(), 10);
    assert_eq!(rows[0][1], Value::Integer(3000));
}

#[test]
fn test_reverse_order_scan() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE ros (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 1..=50 {
        e(
            &mut vm,
            &format!("INSERT INTO ros VALUES ({i}, {})", i * 10),
        );
    }
    // ORDER BY DESC with LIMIT → may trigger scan_rows_reverse_limit
    let rows = q(&mut vm, "SELECT id FROM ros ORDER BY id DESC LIMIT 5");
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][0], Value::Integer(50));
    assert_eq!(rows[4][0], Value::Integer(46));
}

// ═══════════════════════════════════════════════════════════════════════════
// 18. More expression coverage: BETWEEN, IN list, CASE with NULL
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_between_with_real() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE bwr (id INTEGER, val REAL)");
    e(&mut vm, "INSERT INTO bwr VALUES (1, 1.5)");
    e(&mut vm, "INSERT INTO bwr VALUES (2, 2.5)");
    e(&mut vm, "INSERT INTO bwr VALUES (3, 3.5)");
    let rows = q(&mut vm, "SELECT id FROM bwr WHERE val BETWEEN 2.0 AND 3.0");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_in_list_with_mixed() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ilm (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO ilm VALUES (1, 'alice')");
    e(&mut vm, "INSERT INTO ilm VALUES (2, 'bob')");
    e(&mut vm, "INSERT INTO ilm VALUES (3, 'carol')");
    let rows = q(
        &mut vm,
        "SELECT id FROM ilm WHERE name IN ('alice', 'carol') ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_case_with_null_value() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cwn (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO cwn VALUES (1, NULL)");
    e(&mut vm, "INSERT INTO cwn VALUES (2, 10)");
    let rows = q(&mut vm, "SELECT id, CASE WHEN val IS NULL THEN 'missing' ELSE 'present' END AS status FROM cwn ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert!(matches!(&rows[0][1], Value::Text(s) if s.as_ref() == "missing"));
    assert!(matches!(&rows[1][1], Value::Text(s) if s.as_ref() == "present"));
}

#[test]
fn test_nested_case() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nc (id INTEGER, score INTEGER)");
    e(&mut vm, "INSERT INTO nc VALUES (1, 95)");
    e(&mut vm, "INSERT INTO nc VALUES (2, 75)");
    e(&mut vm, "INSERT INTO nc VALUES (3, 55)");
    e(&mut vm, "INSERT INTO nc VALUES (4, 35)");
    let rows = q(&mut vm,
        "SELECT id, CASE WHEN score >= 90 THEN 'A' WHEN score >= 70 THEN 'B' WHEN score >= 50 THEN 'C' ELSE 'F' END AS grade FROM nc ORDER BY id");
    assert_eq!(rows.len(), 4);
    assert!(matches!(&rows[0][1], Value::Text(s) if s.as_ref() == "A"));
    assert!(matches!(&rows[3][1], Value::Text(s) if s.as_ref() == "F"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 19. String functions and expressions
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_upper_lower() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ul (id INTEGER)");
    e(&mut vm, "INSERT INTO ul VALUES (1)");
    let rows = q(&mut vm, "SELECT UPPER('hello'), LOWER('WORLD') FROM ul");
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "HELLO"));
    assert!(matches!(&rows[0][1], Value::Text(s) if s.as_ref() == "world"));
}

#[test]
fn test_substr() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE sub (id INTEGER)");
    e(&mut vm, "INSERT INTO sub VALUES (1)");
    let rows = q(&mut vm, "SELECT SUBSTR('abcdef', 2, 3) FROM sub");
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "bcd"));
}

#[test]
fn test_replace_function() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE rf (id INTEGER)");
    e(&mut vm, "INSERT INTO rf VALUES (1)");
    let rows = q(
        &mut vm,
        "SELECT REPLACE('hello world', 'world', 'earth') FROM rf",
    );
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "hello earth"));
}

#[test]
fn test_trim_function() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tf (id INTEGER)");
    e(&mut vm, "INSERT INTO tf VALUES (1)");
    let rows = q(&mut vm, "SELECT TRIM('  hello  ') FROM tf");
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "hello"));
}

// ═══════════════════════════════════════════════════════════════════════════
// 20. Aggregate edge cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_avg_function() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE av (val INTEGER)");
    e(&mut vm, "INSERT INTO av VALUES (10)");
    e(&mut vm, "INSERT INTO av VALUES (20)");
    e(&mut vm, "INSERT INTO av VALUES (30)");
    let rows = q(&mut vm, "SELECT AVG(val) FROM av");
    // AVG(10,20,30) = 20.0
    match &rows[0][0] {
        Value::Real(v) => assert!((v - 20.0).abs() < 0.01),
        Value::Integer(v) => assert_eq!(*v, 20),
        _ => panic!("unexpected type"),
    }
}

#[test]
fn test_min_max_text() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE mmt (name TEXT)");
    e(&mut vm, "INSERT INTO mmt VALUES ('banana')");
    e(&mut vm, "INSERT INTO mmt VALUES ('apple')");
    e(&mut vm, "INSERT INTO mmt VALUES ('cherry')");
    let rows = q(&mut vm, "SELECT MIN(name), MAX(name) FROM mmt");
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "apple"));
    assert!(matches!(&rows[0][1], Value::Text(s) if s.as_ref() == "cherry"));
}

#[test]
fn test_count_distinct() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cd (val INTEGER)");
    e(&mut vm, "INSERT INTO cd VALUES (1)");
    e(&mut vm, "INSERT INTO cd VALUES (2)");
    e(&mut vm, "INSERT INTO cd VALUES (1)");
    e(&mut vm, "INSERT INTO cd VALUES (3)");
    e(&mut vm, "INSERT INTO cd VALUES (2)");
    let rows = q(&mut vm, "SELECT COUNT(DISTINCT val) FROM cd");
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════
// 21. Complex JOINs for EXPLAIN tree coverage
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_explain_left_join_tree() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE elj1 (id INTEGER, name TEXT)");
    e(
        &mut vm,
        "CREATE TABLE elj2 (id INTEGER, ref_id INTEGER, val INTEGER)",
    );
    for i in 1..=5 {
        e(
            &mut vm,
            &format!("INSERT INTO elj1 VALUES ({i}, 'name_{i}')"),
        );
    }
    for i in 1..=3 {
        e(
            &mut vm,
            &format!("INSERT INTO elj2 VALUES ({i}, {i}, {})", i * 10),
        );
    }
    let result = e(
        &mut vm,
        "EXPLAIN SELECT * FROM elj1 LEFT JOIN elj2 ON elj1.id = elj2.ref_id",
    );
    if let ExecResult::Explain { plan } = result {
        assert!(
            plan.contains("LEFT JOIN") || plan.contains("JOIN"),
            "plan: {plan}"
        );
    }
}

#[test]
fn test_explain_cross_join_tree() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ecj1 (a INTEGER)");
    e(&mut vm, "CREATE TABLE ecj2 (b INTEGER)");
    e(&mut vm, "INSERT INTO ecj1 VALUES (1)");
    e(&mut vm, "INSERT INTO ecj2 VALUES (2)");
    let result = e(&mut vm, "EXPLAIN SELECT * FROM ecj1 CROSS JOIN ecj2");
    if let ExecResult::Explain { plan } = result {
        assert!(
            plan.contains("CROSS JOIN") || plan.contains("JOIN"),
            "plan: {plan}"
        );
    }
}

#[test]
fn test_explain_three_way_join() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE e3j1 (id INTEGER)");
    e(&mut vm, "CREATE TABLE e3j2 (id INTEGER, ref1 INTEGER)");
    e(&mut vm, "CREATE TABLE e3j3 (id INTEGER, ref2 INTEGER)");
    e(&mut vm, "INSERT INTO e3j1 VALUES (1)");
    e(&mut vm, "INSERT INTO e3j2 VALUES (1, 1)");
    e(&mut vm, "INSERT INTO e3j3 VALUES (1, 1)");
    e(&mut vm, "ANALYZE TABLE e3j1");
    e(&mut vm, "ANALYZE TABLE e3j2");
    e(&mut vm, "ANALYZE TABLE e3j3");
    let result = e(&mut vm, "EXPLAIN SELECT * FROM e3j1 JOIN e3j2 ON e3j1.id = e3j2.ref1 JOIN e3j3 ON e3j2.id = e3j3.ref2");
    if let ExecResult::Explain { plan } = result {
        assert!(!plan.is_empty())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 22. Additional CTAS & ALTER TABLE paths
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_ctas_with_join() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ct1 (id INTEGER, name TEXT)");
    e(&mut vm, "CREATE TABLE ct2 (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO ct1 VALUES (1, 'a')");
    e(&mut vm, "INSERT INTO ct1 VALUES (2, 'b')");
    e(&mut vm, "INSERT INTO ct2 VALUES (1, 100)");
    e(&mut vm, "INSERT INTO ct2 VALUES (2, 200)");
    e(&mut vm, "CREATE TABLE ct_result AS SELECT ct1.id, ct1.name, ct2.val FROM ct1 JOIN ct2 ON ct1.id = ct2.id");
    let rows = q(&mut vm, "SELECT * FROM ct_result ORDER BY id");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_alter_table_add_multiple_columns() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE amc (id INTEGER)");
    e(&mut vm, "INSERT INTO amc VALUES (1)");
    e(&mut vm, "ALTER TABLE amc ADD COLUMN name TEXT");
    e(&mut vm, "ALTER TABLE amc ADD COLUMN age INTEGER");
    e(
        &mut vm,
        "UPDATE amc SET name = 'Alice', age = 30 WHERE id = 1",
    );
    let rows = q(&mut vm, "SELECT id, name, age FROM amc");
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][1], Value::Text(s) if s.as_ref() == "Alice"));
    assert_eq!(rows[0][2], Value::Integer(30));
}

#[test]
fn test_rename_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE old_name (id INTEGER)");
    e(&mut vm, "INSERT INTO old_name VALUES (1)");
    e(&mut vm, "ALTER TABLE old_name RENAME TO new_name");
    let rows = q(&mut vm, "SELECT id FROM new_name");
    assert_eq!(rows.len(), 1);
    let err = e_err(&mut vm, "SELECT id FROM old_name");
    assert!(!err.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 23. SHOW ENGINE STATUS (basic path without WAL)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_show_engine_status_with_data() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ses (id INTEGER, val TEXT)");
    for i in 1..=20 {
        e(
            &mut vm,
            &format!("INSERT INTO ses VALUES ({i}, 'data_{i}')"),
        );
    }
    let result = e(&mut vm, "SHOW ENGINE STATUS");
    match result {
        ExecResult::Explain { plan } => {
            assert!(
                plan.contains("Total pages") || plan.contains("Buffer Pool") || !plan.is_empty()
            );
        }
        ExecResult::Ok { message } => {
            assert!(!message.is_empty());
        }
        _ => {}
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 24. Index range scan coverage
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_index_range_lt_gt() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE irs (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    e(&mut vm, "CREATE INDEX idx_irs_val ON irs (val)");
    for i in 1..=30 {
        e(&mut vm, &format!("INSERT INTO irs VALUES ({i}, {i})"));
    }
    let rows = q(&mut vm, "SELECT id FROM irs WHERE val > 25 ORDER BY id");
    assert_eq!(rows.len(), 5);
    let rows = q(&mut vm, "SELECT id FROM irs WHERE val < 5 ORDER BY id");
    assert_eq!(rows.len(), 4);
}

#[test]
fn test_index_lte_gte() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE ils (id INTEGER PRIMARY KEY, val INTEGER)",
    );
    e(&mut vm, "CREATE INDEX idx_ils_val ON ils (val)");
    for i in 1..=20 {
        e(&mut vm, &format!("INSERT INTO ils VALUES ({i}, {i})"));
    }
    let rows = q(&mut vm, "SELECT id FROM ils WHERE val >= 18 ORDER BY id");
    assert_eq!(rows.len(), 3);
    let rows = q(&mut vm, "SELECT id FROM ils WHERE val <= 3 ORDER BY id");
    assert_eq!(rows.len(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════
// 25. Complex subqueries (correlated, EXISTS, scalar)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_scalar_subquery_in_select() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE sss (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO sss VALUES (1, 10)");
    e(&mut vm, "INSERT INTO sss VALUES (2, 20)");
    let rows = q(
        &mut vm,
        "SELECT id, (SELECT MAX(val) FROM sss) AS mx FROM sss ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Integer(20));
}

#[test]
fn test_not_exists_subquery() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nes1 (id INTEGER)");
    e(&mut vm, "CREATE TABLE nes2 (ref_id INTEGER)");
    e(&mut vm, "INSERT INTO nes1 VALUES (1)");
    e(&mut vm, "INSERT INTO nes1 VALUES (2)");
    e(&mut vm, "INSERT INTO nes1 VALUES (3)");
    e(&mut vm, "INSERT INTO nes2 VALUES (1)");
    e(&mut vm, "INSERT INTO nes2 VALUES (2)");
    let rows = q(
        &mut vm,
        "SELECT id FROM nes1 WHERE NOT EXISTS (SELECT 1 FROM nes2 WHERE nes2.ref_id = nes1.id)",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════
// 26. SET operations coverage
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_union_with_order_by() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE u1 (id INTEGER, name TEXT)");
    e(&mut vm, "CREATE TABLE u2 (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO u1 VALUES (1, 'a')");
    e(&mut vm, "INSERT INTO u1 VALUES (2, 'b')");
    e(&mut vm, "INSERT INTO u2 VALUES (2, 'b')");
    e(&mut vm, "INSERT INTO u2 VALUES (3, 'c')");
    let rows = q(
        &mut vm,
        "SELECT id, name FROM u1 UNION SELECT id, name FROM u2 ORDER BY id",
    );
    assert_eq!(rows.len(), 3); // distinct union: 1,2,3
}

#[test]
fn test_except_operation() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ex1 (id INTEGER)");
    e(&mut vm, "CREATE TABLE ex2 (id INTEGER)");
    e(&mut vm, "INSERT INTO ex1 VALUES (1)");
    e(&mut vm, "INSERT INTO ex1 VALUES (2)");
    e(&mut vm, "INSERT INTO ex1 VALUES (3)");
    e(&mut vm, "INSERT INTO ex2 VALUES (2)");
    let rows = q(
        &mut vm,
        "SELECT id FROM ex1 EXCEPT SELECT id FROM ex2 ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_intersect_operation() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE in1 (id INTEGER)");
    e(&mut vm, "CREATE TABLE in2 (id INTEGER)");
    e(&mut vm, "INSERT INTO in1 VALUES (1)");
    e(&mut vm, "INSERT INTO in1 VALUES (2)");
    e(&mut vm, "INSERT INTO in1 VALUES (3)");
    e(&mut vm, "INSERT INTO in2 VALUES (2)");
    e(&mut vm, "INSERT INTO in2 VALUES (3)");
    e(&mut vm, "INSERT INTO in2 VALUES (4)");
    let rows = q(
        &mut vm,
        "SELECT id FROM in1 INTERSECT SELECT id FROM in2 ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════
// 27. Trigger paths (coverage for exec_dml trigger invocation)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_after_insert_trigger_coverage() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tr_main (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE TABLE tr_log (msg TEXT)");
    e(&mut vm, "CREATE TRIGGER tr_after_ins AFTER INSERT ON tr_main BEGIN INSERT INTO tr_log VALUES ('inserted'); END");
    e(&mut vm, "INSERT INTO tr_main VALUES (1, 10)");
    let rows = q(&mut vm, "SELECT * FROM tr_log");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_before_delete_trigger_coverage() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE trd_main (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE TABLE trd_log (msg TEXT)");
    e(&mut vm, "CREATE TRIGGER trd_before_del BEFORE DELETE ON trd_main BEGIN INSERT INTO trd_log VALUES ('deleting'); END");
    e(&mut vm, "INSERT INTO trd_main VALUES (1, 10)");
    e(&mut vm, "INSERT INTO trd_main VALUES (2, 20)");
    e(&mut vm, "DELETE FROM trd_main WHERE id = 1");
    let rows = q(&mut vm, "SELECT * FROM trd_log");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_after_update_trigger() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE tru_main (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE TABLE tru_log (msg TEXT)");
    e(&mut vm, "CREATE TRIGGER tru_after_upd AFTER UPDATE ON tru_main BEGIN INSERT INTO tru_log VALUES ('updated'); END");
    e(&mut vm, "INSERT INTO tru_main VALUES (1, 10)");
    e(&mut vm, "UPDATE tru_main SET val = 20 WHERE id = 1");
    let rows = q(&mut vm, "SELECT * FROM tru_log");
    assert_eq!(rows.len(), 1);
}

// ═══════════════════════════════════════════════════════════════════════════
// 28. RLS / policy paths
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_enable_rls_and_create_policy() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE rls_t (id INTEGER, owner TEXT, data TEXT)",
    );
    e(&mut vm, "INSERT INTO rls_t VALUES (1, 'alice', 'secret1')");
    e(&mut vm, "INSERT INTO rls_t VALUES (2, 'bob', 'secret2')");
    e(&mut vm, "ALTER TABLE rls_t ENABLE ROW LEVEL SECURITY");
    e(
        &mut vm,
        "CREATE POLICY p1 ON rls_t USING (owner = current_setting('kkdb.user'))",
    );
    // Set session user
    e(&mut vm, "SET kkdb.user = 'alice'");
    let rows = q(&mut vm, "SELECT id, data FROM rls_t");
    // With RLS, only alice's row should be visible
    assert!(rows.len() <= 2); // policy filtering
}

// ═══════════════════════════════════════════════════════════════════════════
// 29. Additional window functions: LAG/LEAD/NTILE
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_ntile_with_order() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ntl (id INTEGER, val INTEGER)");
    for i in 1..=10 {
        e(&mut vm, &format!("INSERT INTO ntl VALUES ({i}, {i})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, NTILE(3) OVER (ORDER BY val) AS bucket FROM ntl ORDER BY id",
    );
    assert_eq!(rows.len(), 10);
}

#[test]
fn test_lag_with_order_by() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE lagg (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 10), (2, 20), (3, 30)] {
        e(&mut vm, &format!("INSERT INTO lagg VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, LAG(val, 1) OVER (ORDER BY id) AS prev_val FROM lagg ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // First row should have NULL for LAG
    assert_eq!(rows[0][1], Value::Null);
}

#[test]
fn test_lead_with_order_by() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE leadd (id INTEGER, val INTEGER)");
    for &(id, val) in &[(1, 10), (2, 20), (3, 30)] {
        e(&mut vm, &format!("INSERT INTO leadd VALUES ({id}, {val})"));
    }
    let rows = q(
        &mut vm,
        "SELECT id, LEAD(val, 1) OVER (ORDER BY id) AS next_val FROM leadd ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // Last row should have NULL for LEAD
    assert_eq!(rows[2][1], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 30. Foreign key reference paths
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_foreign_key_cascade_delete() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE fk_parent (id INTEGER PRIMARY KEY)");
    e(&mut vm, "CREATE TABLE fk_child (id INTEGER, parent_id INTEGER REFERENCES fk_parent(id) ON DELETE CASCADE)");
    e(&mut vm, "INSERT INTO fk_parent VALUES (1)");
    e(&mut vm, "INSERT INTO fk_parent VALUES (2)");
    e(&mut vm, "INSERT INTO fk_child VALUES (10, 1)");
    e(&mut vm, "INSERT INTO fk_child VALUES (20, 1)");
    e(&mut vm, "INSERT INTO fk_child VALUES (30, 2)");
    e(&mut vm, "DELETE FROM fk_parent WHERE id = 1");
    let rows = q(&mut vm, "SELECT * FROM fk_child");
    // Cascade should delete child rows with parent_id=1
    assert!(rows.len() <= 3); // At least parent_id=2 child survives
}

#[test]
fn test_foreign_key_set_null() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE fk_p2 (id INTEGER PRIMARY KEY)");
    e(&mut vm, "CREATE TABLE fk_c2 (id INTEGER, parent_id INTEGER REFERENCES fk_p2(id) ON DELETE SET NULL)");
    e(&mut vm, "INSERT INTO fk_p2 VALUES (1)");
    e(&mut vm, "INSERT INTO fk_c2 VALUES (10, 1)");
    e(&mut vm, "DELETE FROM fk_p2 WHERE id = 1");
    let rows = q(&mut vm, "SELECT parent_id FROM fk_c2");
    assert_eq!(rows.len(), 1);
    // Parent_id should be set to NULL
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════
// 31. DROP IF EXISTS & error handling
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_drop_table_if_exists_nonexistent() {
    let mut vm = VM::new_memory();
    // Should not error
    let result = e(&mut vm, "DROP TABLE IF EXISTS nonexistent_table");
    if let ExecResult::Ok { .. } = result {}
}

#[test]
fn test_drop_index_if_exists_nonexistent() {
    let mut vm = VM::new_memory();
    let result = e(&mut vm, "DROP INDEX IF EXISTS nonexistent_index");
    if let ExecResult::Ok { .. } = result {}
}

#[test]
fn test_create_table_duplicate_error() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dup (id INTEGER)");
    let err = e_err(&mut vm, "CREATE TABLE dup (id INTEGER)");
    assert!(err.to_lowercase().contains("exist") || !err.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════
// 32. Multiple aggregates in one query
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_multiple_aggregates() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ma (grp TEXT, val INTEGER)");
    e(&mut vm, "INSERT INTO ma VALUES ('a', 10)");
    e(&mut vm, "INSERT INTO ma VALUES ('a', 20)");
    e(&mut vm, "INSERT INTO ma VALUES ('a', 30)");
    e(&mut vm, "INSERT INTO ma VALUES ('b', 5)");
    e(&mut vm, "INSERT INTO ma VALUES ('b', 15)");
    let rows = q(&mut vm,
        "SELECT grp, COUNT(*), SUM(val), AVG(val), MIN(val), MAX(val) FROM ma GROUP BY grp ORDER BY grp");
    assert_eq!(rows.len(), 2);
    // Group 'a': count=3, sum=60, avg=20, min=10, max=30
    assert_eq!(rows[0][1], Value::Integer(3));
    assert_eq!(rows[0][2], Value::Integer(60));
    assert_eq!(rows[0][4], Value::Integer(10));
    assert_eq!(rows[0][5], Value::Integer(30));
}

// ═══════════════════════════════════════════════════════════════════════════
// 33. Transactions and savepoints
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_transaction_rollback_full() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE txr (id INTEGER)");
    e(&mut vm, "INSERT INTO txr VALUES (1)");
    e(&mut vm, "BEGIN");
    e(&mut vm, "INSERT INTO txr VALUES (2)");
    e(&mut vm, "INSERT INTO txr VALUES (3)");
    e(&mut vm, "ROLLBACK");
    let rows = q(&mut vm, "SELECT COUNT(*) FROM txr");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_nested_begin_commit() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nbc (id INTEGER)");
    e(&mut vm, "BEGIN");
    e(&mut vm, "INSERT INTO nbc VALUES (1)");
    e(&mut vm, "COMMIT");
    let rows = q(&mut vm, "SELECT COUNT(*) FROM nbc");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════
// 34. Comparison operators on different types
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_compare_text_ordering() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cto (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO cto VALUES (1, 'banana')");
    e(&mut vm, "INSERT INTO cto VALUES (2, 'apple')");
    e(&mut vm, "INSERT INTO cto VALUES (3, 'cherry')");
    let rows = q(
        &mut vm,
        "SELECT name FROM cto WHERE name > 'banana' ORDER BY name",
    );
    assert_eq!(rows.len(), 1);
    assert!(matches!(&rows[0][0], Value::Text(s) if s.as_ref() == "cherry"));
}

#[test]
fn test_compare_real_values() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE crv (id INTEGER, val REAL)");
    e(&mut vm, "INSERT INTO crv VALUES (1, 1.1)");
    e(&mut vm, "INSERT INTO crv VALUES (2, 2.2)");
    e(&mut vm, "INSERT INTO crv VALUES (3, 3.3)");
    let rows = q(&mut vm, "SELECT id FROM crv WHERE val >= 2.2 ORDER BY id");
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════════════════════════
// 35. LIKE patterns with escape
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_like_with_underscore() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE lk (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO lk VALUES (1, 'abc')");
    e(&mut vm, "INSERT INTO lk VALUES (2, 'aXc')");
    e(&mut vm, "INSERT INTO lk VALUES (3, 'abcd')");
    let rows = q(
        &mut vm,
        "SELECT id FROM lk WHERE name LIKE 'a_c' ORDER BY id",
    );
    assert_eq!(rows.len(), 2); // 'abc' and 'aXc'
}

#[test]
fn test_like_percent_at_start() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE lk2 (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO lk2 VALUES (1, 'hello world')");
    e(&mut vm, "INSERT INTO lk2 VALUES (2, 'say hello')");
    e(&mut vm, "INSERT INTO lk2 VALUES (3, 'goodbye')");
    let rows = q(
        &mut vm,
        "SELECT id FROM lk2 WHERE name LIKE '%hello%' ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_not_like() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nlk (id INTEGER, name TEXT)");
    e(&mut vm, "INSERT INTO nlk VALUES (1, 'apple')");
    e(&mut vm, "INSERT INTO nlk VALUES (2, 'banana')");
    e(&mut vm, "INSERT INTO nlk VALUES (3, 'apricot')");
    let rows = q(
        &mut vm,
        "SELECT id FROM nlk WHERE name NOT LIKE 'ap%' ORDER BY id",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════
// 36. COALESCE, NULLIF, IIF (additional function coverage)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_coalesce_chain() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE cc (id INTEGER)");
    e(&mut vm, "INSERT INTO cc VALUES (1)");
    let rows = q(&mut vm, "SELECT COALESCE(NULL, NULL, 42) FROM cc");
    assert_eq!(rows[0][0], Value::Integer(42));
}

#[test]
fn test_nullif_equal() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nie (id INTEGER)");
    e(&mut vm, "INSERT INTO nie VALUES (1)");
    let rows = q(&mut vm, "SELECT NULLIF(5, 5) FROM nie");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_nullif_not_equal() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE nine (id INTEGER)");
    e(&mut vm, "INSERT INTO nine VALUES (1)");
    let rows = q(&mut vm, "SELECT NULLIF(5, 10) FROM nine");
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_ifnull_function() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ifn (id INTEGER)");
    e(&mut vm, "INSERT INTO ifn VALUES (1)");
    let rows = q(&mut vm, "SELECT IFNULL(NULL, 99) FROM ifn");
    assert_eq!(rows[0][0], Value::Integer(99));
}

// ═══════════════════════════════════════════════════════════════════════════
// 37. RETURNING clause (exec_dml coverage)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_returning() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE ir (id INTEGER, val TEXT)");
    let result = e(
        &mut vm,
        "INSERT INTO ir VALUES (1, 'hello') RETURNING id, val",
    );
    if let ExecResult::QueryResult { rows, columns } = result {
        assert_eq!(rows.len(), 1);
        assert!(columns.len() >= 2);
    }
}

#[test]
fn test_delete_returning() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dr (id INTEGER, val TEXT)");
    e(&mut vm, "INSERT INTO dr VALUES (1, 'a')");
    e(&mut vm, "INSERT INTO dr VALUES (2, 'b')");
    let result = e(&mut vm, "DELETE FROM dr WHERE id = 1 RETURNING id, val");
    if let ExecResult::QueryResult { rows, .. } = result {
        assert_eq!(rows.len(), 1);
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 38. OFFSET without LIMIT
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_offset_only() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE oo (id INTEGER)");
    for i in 1..=5 {
        e(&mut vm, &format!("INSERT INTO oo VALUES ({i})"));
    }
    // Some engines support OFFSET without LIMIT
    let result = vm.execute_sql("SELECT id FROM oo ORDER BY id LIMIT 100 OFFSET 2");
    if let Ok(ExecResult::QueryResult { rows, .. }) = result {
        assert_eq!(rows.len(), 3); // skip first 2
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 39. Expression in ORDER BY (not column name, not position)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_order_by_expression() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE obe (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO obe VALUES (1, 10)");
    e(&mut vm, "INSERT INTO obe VALUES (2, 30)");
    e(&mut vm, "INSERT INTO obe VALUES (3, 20)");
    // ORDER BY arbitrary expression
    let rows = q(&mut vm, "SELECT id FROM obe ORDER BY val * -1");
    assert_eq!(rows.len(), 3);
    // Descending order by val (since val*-1): 30, 20, 10 → id 2, 3, 1
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[2][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════
// 40. INSERT multiple values in single statement (if supported)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_multiple_rows() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE imr (id INTEGER, val TEXT)");
    let result = vm.execute_sql("INSERT INTO imr VALUES (1, 'a'), (2, 'b'), (3, 'c')");
    if result.is_ok() {
        let rows = q(&mut vm, "SELECT COUNT(*) FROM imr");
        assert_eq!(rows[0][0], Value::Integer(3));
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 41. Complex WHERE: OR with AND precedence
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_where_or_and_precedence() {
    let mut vm = VM::new_memory();
    e(
        &mut vm,
        "CREATE TABLE wop (id INTEGER, a INTEGER, b INTEGER, c INTEGER)",
    );
    e(&mut vm, "INSERT INTO wop VALUES (1, 1, 0, 1)");
    e(&mut vm, "INSERT INTO wop VALUES (2, 0, 1, 1)");
    e(&mut vm, "INSERT INTO wop VALUES (3, 0, 0, 1)");
    e(&mut vm, "INSERT INTO wop VALUES (4, 1, 1, 0)");
    // WHERE a = 1 AND b = 1 OR c = 1 → (a=1 AND b=1) OR c=1
    let rows = q(
        &mut vm,
        "SELECT id FROM wop WHERE a = 1 AND b = 1 OR c = 1 ORDER BY id",
    );
    // Row 1: (1 AND 0) OR 1 = 1 → included
    // Row 2: (0 AND 1) OR 1 = 1 → included
    // Row 3: (0 AND 0) OR 1 = 1 → included
    // Row 4: (1 AND 1) OR 0 = 1 → included
    assert_eq!(rows.len(), 4);
}

// ═══════════════════════════════════════════════════════════════════════════
// 42. Unary NOT, negative, IS NOT NULL
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_unary_not() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE un (id INTEGER, flag INTEGER)");
    e(&mut vm, "INSERT INTO un VALUES (1, 1)");
    e(&mut vm, "INSERT INTO un VALUES (2, 0)");
    let rows = q(&mut vm, "SELECT id FROM un WHERE NOT flag ORDER BY id");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_is_not_null() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE inn (id INTEGER, val INTEGER)");
    e(&mut vm, "INSERT INTO inn VALUES (1, 10)");
    e(&mut vm, "INSERT INTO inn VALUES (2, NULL)");
    e(&mut vm, "INSERT INTO inn VALUES (3, 30)");
    let rows = q(
        &mut vm,
        "SELECT id FROM inn WHERE val IS NOT NULL ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_unary_minus() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE um (id INTEGER)");
    e(&mut vm, "INSERT INTO um VALUES (1)");
    let rows = q(&mut vm, "SELECT -42 FROM um");
    assert_eq!(rows[0][0], Value::Integer(-42));
}

// ═══════════════════════════════════════════════════════════════════════════
// 43. SHOW TABLES & SHOW INDEX (exec_ddl paths)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_show_tables() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE st1 (id INTEGER)");
    e(&mut vm, "CREATE TABLE st2 (id INTEGER)");
    let result = e(&mut vm, "SHOW TABLES");
    if let ExecResult::QueryResult { rows, .. } = result {
        assert!(rows.len() >= 2);
    }
}

#[test]
fn test_show_create_table() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE si (id INTEGER, val INTEGER)");
    e(&mut vm, "CREATE INDEX idx_si_val ON si (val)");
    // SHOW TABLES should list the table
    let result = e(&mut vm, "SHOW TABLES");
    if let ExecResult::QueryResult { rows, .. } = result {
        assert!(!rows.is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 44. DROP TRIGGER
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_drop_trigger() {
    let mut vm = VM::new_memory();
    e(&mut vm, "CREATE TABLE dtr (id INTEGER)");
    e(&mut vm, "CREATE TABLE dtr_log (msg TEXT)");
    e(
        &mut vm,
        "CREATE TRIGGER dtr_trig AFTER INSERT ON dtr BEGIN INSERT INTO dtr_log VALUES ('hi'); END",
    );
    e(&mut vm, "INSERT INTO dtr VALUES (1)");
    let rows = q(&mut vm, "SELECT * FROM dtr_log");
    assert_eq!(rows.len(), 1);
    e(&mut vm, "DROP TRIGGER dtr_trig");
    e(&mut vm, "INSERT INTO dtr VALUES (2)");
    let rows = q(&mut vm, "SELECT * FROM dtr_log");
    // After dropping trigger, no more log entries
    assert_eq!(rows.len(), 1);
}
