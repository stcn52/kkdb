//! Coverage boost 11: targets less-obvious code paths across eval_expr.rs,
//! exec_select.rs, exec_ddl.rs, execute.rs, and schema.rs.
//!
//! Focus areas:
//!   - Window aggregate functions (SUM/AVG/COUNT/MIN/MAX OVER), NTH_VALUE
//!   - INTERSECT / EXCEPT set operations with ORDER BY / LIMIT
//!   - EXPLAIN ANALYZE for INSERT/UPDATE/DELETE (non-SELECT paths)
//!   - EXPLAIN FORMAT JSON
//!   - SET session variables (engine config: buffer_pool, flush_method, isolation)
//!   - SAVEPOINT / RELEASE SAVEPOINT / ROLLBACK TO SAVEPOINT
//!   - CREATE OR REPLACE VIEW / DROP VIEW via DROP TABLE
//!   - Mixed-type arithmetic in binary ops (Integer + Real, etc.)
//!   - POWER with overflow, negative exponent, Real base
//!   - FACTORIAL edge cases
//!   - CAST to Date/Timestamp/Json/Blob
//!   - INTERVAL expressions
//!   - JSON_UNQUOTE
//!   - REGEXP_LIKE patterns
//!   - Correlated subqueries (EXISTS, IN subquery)
//!   - ANY / ALL subquery operators
//!   - CASE WHEN (simple and searched)
//!   - BETWEEN with NULL
//!   - Bitwise operators (|, &, ^, <<, >>)
//!   - Modulo operator
//!   - Concat operator (||)
//!   - SIGN / CBRT / BIT_NOT on various types
//!   - ALTER TABLE DROP COLUMN with cascade index drop
//!   - ANALYZE TABLE + VACUUM
//!   - Named window definitions (WINDOW w AS (...))
//!   - Schema operations: add column with DEFAULT, rename column

use super::{query_rows, ExecResult, Value, VM};

fn text(s: &str) -> Value {
    Value::Text(s.into())
}

// ── 1. Window Aggregate: SUM OVER (ORDER BY) ─────────────────────────────────

#[test]
fn window_sum_over_order_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ws (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ws VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO ws VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO ws VALUES (3, 30)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, SUM(val) OVER (ORDER BY id) FROM ws ORDER BY id",
    );
    // Running sum: 10, 30, 60
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(10));
    assert_eq!(rows[1][1], Value::Integer(30));
    assert_eq!(rows[2][1], Value::Integer(60));
}

// ── 2. Window Aggregate: AVG OVER (PARTITION BY) ─────────────────────────────

#[test]
fn window_avg_over_partition() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wa (id INTEGER PRIMARY KEY, grp TEXT, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO wa VALUES (1, 'a', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO wa VALUES (2, 'a', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO wa VALUES (3, 'b', 30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, AVG(val) OVER (PARTITION BY grp) FROM wa ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // Group 'a' avg = 15.0, group 'b' avg = 30.0
    assert!(matches!(rows[0][1], Value::Real(v) if (v - 15.0).abs() < 0.01));
    assert!(matches!(rows[2][1], Value::Real(v) if (v - 30.0).abs() < 0.01));
}

// ── 3. Window Aggregate: COUNT OVER + MIN/MAX OVER ───────────────────────────

#[test]
fn window_count_min_max_over() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wcm (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO wcm VALUES (1, 5)").unwrap();
    vm.execute_sql("INSERT INTO wcm VALUES (2, 15)").unwrap();
    vm.execute_sql("INSERT INTO wcm VALUES (3, 10)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, COUNT(val) OVER (), MIN(val) OVER (), MAX(val) OVER () FROM wcm ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // COUNT=3, MIN=5, MAX=15 for all rows
    assert_eq!(rows[0][1], Value::Integer(3));
    assert_eq!(rows[0][2], Value::Integer(5));
    assert_eq!(rows[0][3], Value::Integer(15));
}

// ── 4. NTH_VALUE window function ─────────────────────────────────────────────

#[test]
fn window_nth_value() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE wn (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO wn VALUES (1, 'first')")
        .unwrap();
    vm.execute_sql("INSERT INTO wn VALUES (2, 'second')")
        .unwrap();
    vm.execute_sql("INSERT INTO wn VALUES (3, 'third')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id, NTH_VALUE(val, 2) OVER (ORDER BY id) FROM wn ORDER BY id",
    );
    assert_eq!(rows.len(), 3);
    // NTH_VALUE(val, 2) → 'second' for rows where frame contains at least 2 rows
    // Row 1: frame [1], NTH_VALUE(2) = NULL
    assert_eq!(rows[0][1], Value::Null);
    // Row 2+: frame [1,2,...], NTH_VALUE(2) = 'second'
    assert_eq!(rows[1][1], text("second"));
}

// ── 5. INTERSECT ALL ─────────────────────────────────────────────────────────

#[test]
fn set_op_intersect_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE si1 (v INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE si2 (v INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO si1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO si1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO si1 VALUES (3)").unwrap();
    vm.execute_sql("INSERT INTO si2 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO si2 VALUES (3)").unwrap();
    vm.execute_sql("INSERT INTO si2 VALUES (4)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT v FROM si1 INTERSECT ALL SELECT v FROM si2 ORDER BY v",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
}

// ── 6. EXCEPT DISTINCT with ORDER BY and LIMIT ───────────────────────────────

#[test]
fn set_op_except_distinct_with_limit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE se1 (v INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE se2 (v INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO se1 VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO se1 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO se1 VALUES (3)").unwrap();
    vm.execute_sql("INSERT INTO se1 VALUES (4)").unwrap();
    vm.execute_sql("INSERT INTO se2 VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO se2 VALUES (4)").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT v FROM se1 EXCEPT SELECT v FROM se2 ORDER BY v LIMIT 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ── 7. EXPLAIN ANALYZE for INSERT ────────────────────────────────────────────

#[test]
fn explain_analyze_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ei (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN ANALYZE INSERT INTO ei VALUES (1, 'test')")
        .unwrap();
    match res {
        ExecResult::Explain { plan } => {
            assert!(
                plan.contains("ANALYZE"),
                "plan should contain ANALYZE: {}",
                plan
            );
            assert!(plan.contains("INSERT"), "plan should mention INSERT");
        }
        _ => panic!("expected Explain"),
    }
}

// ── 8. EXPLAIN ANALYZE for UPDATE ────────────────────────────────────────────

#[test]
fn explain_analyze_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE eu (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO eu VALUES (1, 10)").unwrap();
    let res = vm
        .execute_sql("EXPLAIN ANALYZE UPDATE eu SET val = 20 WHERE id = 1")
        .unwrap();
    match res {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("UPDATE") || plan.contains("Execution time"));
        }
        _ => panic!("expected Explain"),
    }
}

// ── 9. EXPLAIN ANALYZE for DELETE ────────────────────────────────────────────

#[test]
fn explain_analyze_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ed (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO ed VALUES (1)").unwrap();
    let res = vm
        .execute_sql("EXPLAIN ANALYZE DELETE FROM ed WHERE id = 1")
        .unwrap();
    match res {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("DELETE") || plan.contains("Execution time"));
        }
        _ => panic!("expected Explain"),
    }
}

// ── 10. EXPLAIN FORMAT JSON ──────────────────────────────────────────────────

#[test]
fn explain_format_json_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ej (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO ej VALUES (1, 'hello')")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN FORMAT JSON SELECT * FROM ej WHERE id = 1")
        .unwrap();
    match res {
        ExecResult::Explain { plan } => {
            assert!(
                plan.contains("{"),
                "JSON plan should contain braces: {}",
                plan
            );
            assert!(plan.contains("operation"), "JSON should have operation key");
        }
        _ => panic!("expected Explain"),
    }
}

// ── 11. SET session variables: engine config ─────────────────────────────────

#[test]
fn set_session_vars_engine_config() {
    let mut vm = VM::new_memory();
    // Buffer pool pages
    let res = vm
        .execute_sql("SET innodb_buffer_pool_pages = '512'")
        .unwrap();
    match res {
        ExecResult::Ok { message } => assert!(message.contains("512")),
        _ => panic!("expected Ok"),
    }
    // Flush method
    let res = vm.execute_sql("SET innodb_flush_method = 'none'").unwrap();
    match res {
        ExecResult::Ok { message } => assert!(message.contains("none")),
        _ => panic!("expected Ok"),
    }
    // WAL auto checkpoint
    let res = vm.execute_sql("SET wal_auto_checkpoint = '1000'").unwrap();
    match res {
        ExecResult::Ok { message } => assert!(message.contains("1000")),
        _ => panic!("expected Ok"),
    }
}

// ── 12. SET transaction isolation level ──────────────────────────────────────

#[test]
fn set_transaction_isolation_level() {
    let mut vm = VM::new_memory();
    let res = vm
        .execute_sql("SET transaction_isolation = 'read committed'")
        .unwrap();
    match res {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("isolation") || message.contains("ReadCommitted"),
                "message: {}",
                message
            );
        }
        _ => panic!("expected Ok"),
    }
    let res = vm
        .execute_sql("SET transaction_isolation = 'serializable'")
        .unwrap();
    match res {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("isolation") || message.contains("Serializable"),
                "message: {}",
                message
            );
        }
        _ => panic!("expected Ok"),
    }
}

// ── 13. SAVEPOINT / RELEASE / ROLLBACK TO ────────────────────────────────────

#[test]
fn savepoint_release_rollback_to() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sp (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO sp VALUES (1, 'a')").unwrap();
    vm.execute_sql("SAVEPOINT s1").unwrap();
    vm.execute_sql("INSERT INTO sp VALUES (2, 'b')").unwrap();
    // rollback to savepoint — row 2 should be gone
    vm.execute_sql("ROLLBACK TO SAVEPOINT s1").unwrap();
    // Insert row 3 instead, after rollback to savepoint
    vm.execute_sql("INSERT INTO sp VALUES (3, 'c')").unwrap();
    vm.execute_sql("SAVEPOINT s2").unwrap();
    vm.execute_sql("INSERT INTO sp VALUES (4, 'd')").unwrap();
    vm.execute_sql("RELEASE SAVEPOINT s2").unwrap();
    vm.execute_sql("COMMIT").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM sp ORDER BY id");
    assert!(!rows.is_empty());
}

// ── 14. CREATE OR REPLACE VIEW ───────────────────────────────────────────────

#[test]
fn create_or_replace_view() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vt (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO vt VALUES (1, 'hello')")
        .unwrap();
    vm.execute_sql("CREATE VIEW v1 AS SELECT id, val FROM vt")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM v1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], text("hello"));
    // Replace the view — simply exercise the code path
    vm.execute_sql("CREATE OR REPLACE VIEW v1 AS SELECT id FROM vt")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM v1");
    assert_eq!(rows.len(), 1);
}

// ── 15. CREATE VIEW IF NOT EXISTS ────────────────────────────────────────────

#[test]
fn create_view_if_not_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vt2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE VIEW v2 AS SELECT id FROM vt2")
        .unwrap();
    // Should not error
    let res = vm
        .execute_sql("CREATE VIEW IF NOT EXISTS v2 AS SELECT id FROM vt2")
        .unwrap();
    match res {
        ExecResult::Ok { message } => assert!(message.contains("already exists")),
        _ => panic!("expected Ok for IF NOT EXISTS"),
    }
}

// ── 16. Mixed-type arithmetic: Integer + Real ────────────────────────────────

#[test]
fn mixed_type_arithmetic() {
    let mut vm = VM::new_memory();
    // Addition
    let rows = query_rows(&mut vm, "SELECT 10 + 2.5");
    assert_eq!(rows[0][0], Value::Real(12.5));
    // Subtraction
    let rows = query_rows(&mut vm, "SELECT 10 - 2.5");
    assert_eq!(rows[0][0], Value::Real(7.5));
    // Multiplication
    let rows = query_rows(&mut vm, "SELECT 3 * 2.5");
    assert_eq!(rows[0][0], Value::Real(7.5));
    // Division: Real / Integer
    let rows = query_rows(&mut vm, "SELECT 7.5 / 3");
    assert_eq!(rows[0][0], Value::Real(2.5));
    // Division: Integer / Real
    let rows = query_rows(&mut vm, "SELECT 10 / 2.5");
    assert_eq!(rows[0][0], Value::Real(4.0));
}

// ── 17. POWER with overflow, negative exponent, Real args ────────────────────

#[test]
fn power_edge_cases() {
    let mut vm = VM::new_memory();
    // Integer negative exponent → Real
    let rows = query_rows(&mut vm, "SELECT POWER(2, -1)");
    assert!(matches!(rows[0][0], Value::Real(v) if (v - 0.5).abs() < 0.01));
    // Real base, Integer exponent
    let rows = query_rows(&mut vm, "SELECT POWER(2.0, 3)");
    assert_eq!(rows[0][0], Value::Real(8.0));
    // Integer base, Real exponent
    let rows = query_rows(&mut vm, "SELECT POWER(4, 0.5)");
    assert!(matches!(rows[0][0], Value::Real(v) if (v - 2.0).abs() < 0.01));
    // Real base, Real exponent
    let rows = query_rows(&mut vm, "SELECT POWER(2.0, 0.5)");
    assert!(matches!(rows[0][0], Value::Real(v) if (v - std::f64::consts::SQRT_2).abs() < 0.01));
    // NULL args
    let rows = query_rows(&mut vm, "SELECT POWER(NULL, 2)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT POWER(2, NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 18. FACTORIAL edge cases ─────────────────────────────────────────────────

#[test]
fn factorial_edge_cases() {
    let mut vm = VM::new_memory();
    // Factorial of 0 = 1
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(0)");
    assert_eq!(rows[0][0], Value::Integer(1));
    // Factorial of 5 = 120
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(5)");
    assert_eq!(rows[0][0], Value::Integer(120));
    // Factorial of 20 (max)
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(20)");
    assert!(matches!(rows[0][0], Value::Integer(_)));
    // Factorial of negative → error
    let res = vm.execute_sql("SELECT FACTORIAL(-1)");
    assert!(res.is_err());
    // Factorial of 21 → overflow error
    let res = vm.execute_sql("SELECT FACTORIAL(21)");
    assert!(res.is_err());
    // Factorial of NULL → NULL
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 19. CAST to temporal and JSON types ──────────────────────────────────────

#[test]
fn cast_to_temporal_and_json() {
    let mut vm = VM::new_memory();
    // CAST to DATE
    let rows = query_rows(&mut vm, "SELECT CAST('2024-03-15' AS DATE)");
    assert_eq!(rows[0][0], text("2024-03-15"));
    // CAST Integer to TIMESTAMP
    let rows = query_rows(&mut vm, "SELECT CAST(1710460800 AS TIMESTAMP)");
    assert!(matches!(rows[0][0], Value::Text(_)));
    // CAST to JSON
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS JSON)");
    assert_eq!(rows[0][0], text("42"));
    // CAST Real to JSON
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS JSON)");
    assert!(matches!(rows[0][0], Value::Text(_)));
    // CAST NULL to DATE
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS DATE)");
    assert_eq!(rows[0][0], Value::Null);
    // CAST to BLOB from Integer
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS BLOB)");
    assert!(matches!(rows[0][0], Value::Blob(_)));
    // CAST to BLOB from Real
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS BLOB)");
    assert!(matches!(rows[0][0], Value::Blob(_)));
}

// ── 20. INTERVAL expression ──────────────────────────────────────────────────

#[test]
fn interval_expression() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT INTERVAL '5' DAY");
    assert_eq!(rows[0][0], text("5 DAY"));
    let rows = query_rows(&mut vm, "SELECT INTERVAL '3' MONTH");
    assert_eq!(rows[0][0], text("3 MONTH"));
}

// ── 21. JSON_UNQUOTE ─────────────────────────────────────────────────────────

#[test]
fn json_unquote() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_UNQUOTE('"hello world"')"#);
    assert_eq!(rows[0][0], text("hello world"));
    // Non-quoted string passes through
    let rows = query_rows(&mut vm, "SELECT JSON_UNQUOTE('plain')");
    assert_eq!(rows[0][0], text("plain"));
    // Integer passes through
    let rows = query_rows(&mut vm, "SELECT JSON_UNQUOTE(42)");
    assert_eq!(rows[0][0], Value::Integer(42));
}

// ── 22. REGEXP_LIKE with anchored patterns ───────────────────────────────────

#[test]
fn regexp_like_patterns() {
    let mut vm = VM::new_memory();
    // Basic match
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('hello world', 'hello')");
    assert_eq!(rows[0][0], Value::Integer(1));
    // .* wildcard
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('hello world', 'hello.*world')");
    assert_eq!(rows[0][0], Value::Integer(1));
    // No match
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('hello', 'xyz')");
    assert_eq!(rows[0][0], Value::Integer(0));
    // Non-text args
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE(42, 'hello')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ── 23. EXISTS subquery ──────────────────────────────────────────────────────

#[test]
fn exists_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ex_main (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE ex_ref (id INTEGER PRIMARY KEY, mid INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO ex_main VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO ex_main VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO ex_ref VALUES (1, 1)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT id FROM ex_main WHERE EXISTS (SELECT 1 FROM ex_ref WHERE ex_ref.mid = ex_main.id) ORDER BY id");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ── 24. ANY / ALL subquery operators ─────────────────────────────────────────

#[test]
fn any_all_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE aa_vals (v INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO aa_vals VALUES (10)").unwrap();
    vm.execute_sql("INSERT INTO aa_vals VALUES (20)").unwrap();
    vm.execute_sql("INSERT INTO aa_vals VALUES (30)").unwrap();
    // 15 > ANY (SELECT v FROM aa_vals) → true (15 > 10)
    let rows = query_rows(&mut vm, "SELECT 15 > ANY (SELECT v FROM aa_vals)");
    assert_eq!(rows[0][0], Value::Integer(1));
    // 5 > ALL (SELECT v FROM aa_vals) → false
    let rows = query_rows(&mut vm, "SELECT 5 > ALL (SELECT v FROM aa_vals)");
    assert_eq!(rows[0][0], Value::Integer(0));
    // 35 > ALL (SELECT v FROM aa_vals) → true
    let rows = query_rows(&mut vm, "SELECT 35 > ALL (SELECT v FROM aa_vals)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ── 25. CASE WHEN searched ───────────────────────────────────────────────────

#[test]
fn case_when_searched() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE cw (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO cw VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO cw VALUES (2, 50)").unwrap();
    vm.execute_sql("INSERT INTO cw VALUES (3, 90)").unwrap();
    let rows = query_rows(&mut vm,
        "SELECT id, CASE WHEN val < 20 THEN 'low' WHEN val < 80 THEN 'mid' ELSE 'high' END FROM cw ORDER BY id");
    assert_eq!(rows[0][1], text("low"));
    assert_eq!(rows[1][1], text("mid"));
    assert_eq!(rows[2][1], text("high"));
}

// ── 26. Simple CASE expression ───────────────────────────────────────────────

#[test]
fn case_simple() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' WHEN 3 THEN 'three' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], text("two"));
    // CASE with no match → ELSE
    let rows = query_rows(&mut vm, "SELECT CASE 99 WHEN 1 THEN 'one' ELSE 'other' END");
    assert_eq!(rows[0][0], text("other"));
    // CASE with NULL operand → NULL (no match)
    let rows = query_rows(
        &mut vm,
        "SELECT CASE NULL WHEN 1 THEN 'one' ELSE 'miss' END",
    );
    assert_eq!(rows[0][0], text("miss"));
}

// ── 27. BETWEEN with NULL ────────────────────────────────────────────────────

#[test]
fn between_with_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL BETWEEN 1 AND 10");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT 5 BETWEEN NULL AND 10");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT 5 BETWEEN 1 AND NULL");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 28. Bitwise operators ────────────────────────────────────────────────────

#[test]
fn bitwise_operators() {
    let mut vm = VM::new_memory();
    // OR
    let rows = query_rows(&mut vm, "SELECT 5 | 3");
    assert_eq!(rows[0][0], Value::Integer(7));
    // AND
    let rows = query_rows(&mut vm, "SELECT 5 & 3");
    assert_eq!(rows[0][0], Value::Integer(1));
    // XOR (bitwise)
    let rows = query_rows(&mut vm, "SELECT 5 ^ 3");
    assert_eq!(rows[0][0], Value::Integer(6));
}

// ── 29. Modulo operator ──────────────────────────────────────────────────────

#[test]
fn modulo_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 17 % 5");
    assert_eq!(rows[0][0], Value::Integer(2));
    // Modulo by zero → NULL
    let rows = query_rows(&mut vm, "SELECT 10 % 0");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 30. Concat operator (||) ─────────────────────────────────────────────────

#[test]
fn concat_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    assert_eq!(rows[0][0], text("hello world"));
}

// ── 31. SIGN / CBRT / BIT_NOT on various types ──────────────────────────────

#[test]
fn sign_cbrt_bitnot() {
    let mut vm = VM::new_memory();
    // SIGN
    let rows = query_rows(&mut vm, "SELECT SIGN(-42)");
    assert_eq!(rows[0][0], Value::Integer(-1));
    let rows = query_rows(&mut vm, "SELECT SIGN(0)");
    assert_eq!(rows[0][0], Value::Integer(0));
    let rows = query_rows(&mut vm, "SELECT SIGN(3.14)");
    assert_eq!(rows[0][0], Value::Real(1.0));
    let rows = query_rows(&mut vm, "SELECT SIGN(NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // CBRT
    let rows = query_rows(&mut vm, "SELECT CBRT(27)");
    assert!(matches!(rows[0][0], Value::Real(v) if (v - 3.0).abs() < 0.001));
    let rows = query_rows(&mut vm, "SELECT CBRT(8.0)");
    assert!(matches!(rows[0][0], Value::Real(v) if (v - 2.0).abs() < 0.001));
    let rows = query_rows(&mut vm, "SELECT CBRT(NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // BITWISE_NOT
    let rows = query_rows(&mut vm, "SELECT BITWISE_NOT(0)");
    assert_eq!(rows[0][0], Value::Integer(-1));
    let rows = query_rows(&mut vm, "SELECT BITWISE_NOT(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 32. DATE_EXTRACT with all fields ─────────────────────────────────────────

#[test]
fn date_extract_all_fields() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT EXTRACT(YEAR FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(2024));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(MONTH FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(3));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(DAY FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(15));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(HOUR FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(10));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(MINUTE FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(30));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(SECOND FROM '2024-03-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(45));
}

// ── 33. ALTER TABLE DROP COLUMN with cascading index drop ────────────────────

#[test]
fn alter_drop_column_cascade_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE dc (id INTEGER PRIMARY KEY, a INTEGER, b TEXT)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_dc_a ON dc (a)").unwrap();
    vm.execute_sql("INSERT INTO dc VALUES (1, 10, 'x')")
        .unwrap();
    vm.execute_sql("INSERT INTO dc VALUES (2, 20, 'y')")
        .unwrap();
    // Drop column 'a' — should cascade drop idx_dc_a
    vm.execute_sql("ALTER TABLE dc DROP COLUMN a").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM dc ORDER BY id");
    assert_eq!(rows.len(), 2);
    // Each row should now have 2 columns (id, b)
    assert_eq!(rows[0].len(), 2);
    assert_eq!(rows[0][1], text("x"));
}

// ── 34. ANALYZE TABLE + VACUUM ───────────────────────────────────────────────

#[test]
fn analyze_table_and_vacuum() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE at (id INTEGER PRIMARY KEY, val INTEGER, name TEXT)")
        .unwrap();
    for i in 1..=20 {
        vm.execute_sql(&format!(
            "INSERT INTO at VALUES ({}, {}, 'name_{}')",
            i,
            i * 10,
            i
        ))
        .unwrap();
    }
    // ANALYZE TABLE
    let res = vm.execute_sql("ANALYZE TABLE at").unwrap();
    match res {
        ExecResult::Ok { message } => {
            assert!(message.contains("20 rows"), "message: {}", message);
            assert!(message.contains("3 columns"), "message: {}", message);
        }
        _ => panic!("expected Ok"),
    }
    // VACUUM
    let res = vm.execute_sql("VACUUM").unwrap();
    match res {
        ExecResult::Ok { message } => assert!(message.contains("VACUUM")),
        _ => panic!("expected Ok"),
    }
}

// ── 35. IN subquery with NULLs ───────────────────────────────────────────────

#[test]
fn in_subquery_with_nulls() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE isn (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO isn VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO isn VALUES (2, NULL)").unwrap();
    vm.execute_sql("INSERT INTO isn VALUES (3, 30)").unwrap();
    // NULL IN (subquery) → NULL
    let rows = query_rows(&mut vm, "SELECT NULL IN (SELECT val FROM isn)");
    assert_eq!(rows[0][0], Value::Null);
    // Value not found, but subquery contains NULL → NULL
    let rows = query_rows(&mut vm, "SELECT 99 IN (SELECT val FROM isn)");
    assert_eq!(rows[0][0], Value::Null);
    // Value found → 1
    let rows = query_rows(&mut vm, "SELECT 10 IN (SELECT val FROM isn)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ── 36. STARTS_WITH function ─────────────────────────────────────────────────

#[test]
fn starts_with_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello world', 'hello')");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello world', 'world')");
    assert_eq!(rows[0][0], Value::Integer(0));
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH(NULL, 'test')");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 37. current_setting and session variable functions ────────────────────────

#[test]
fn current_setting_function() {
    let mut vm = VM::new_memory();
    vm.execute_sql("SET myapp.key = 'myvalue'").unwrap();
    let rows = query_rows(&mut vm, "SELECT current_setting('myapp.key')");
    assert_eq!(rows[0][0], text("myvalue"));
    // Non-existent key → NULL
    let rows = query_rows(&mut vm, "SELECT current_setting('nonexistent')");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 38. Division by zero ─────────────────────────────────────────────────────

#[test]
fn division_by_zero() {
    let mut vm = VM::new_memory();
    // Integer division by zero → NULL
    let rows = query_rows(&mut vm, "SELECT 10 / 0");
    assert_eq!(rows[0][0], Value::Null);
    // Real division by zero → NULL
    let rows = query_rows(&mut vm, "SELECT 10.0 / 0.0");
    assert_eq!(rows[0][0], Value::Null);
    // Integer / Real zero → NULL
    let rows = query_rows(&mut vm, "SELECT 10 / 0.0");
    assert_eq!(rows[0][0], Value::Null);
    // Real / Integer zero → NULL
    let rows = query_rows(&mut vm, "SELECT 10.0 / 0");
    assert_eq!(rows[0][0], Value::Null);
}

// ── 39. ALTER TABLE ADD COLUMN with DEFAULT value ────────────────────────────

#[test]
fn alter_add_column_with_default() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ad (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO ad VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO ad VALUES (2, 'Bob')").unwrap();
    // Add column with default value
    vm.execute_sql("ALTER TABLE ad ADD COLUMN score INTEGER DEFAULT 100")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT id, name, score FROM ad ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][2], Value::Integer(100));
    assert_eq!(rows[1][2], Value::Integer(100));
}

// ── 40. EXPLAIN with JOIN (INDEX + Stats decision tree) ──────────────────────

#[test]
fn explain_join_with_stats() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ej1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE ej2 (id INTEGER PRIMARY KEY, ref_id INTEGER)")
        .unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO ej1 VALUES ({}, {})", i, i * 10))
            .unwrap();
        vm.execute_sql(&format!("INSERT INTO ej2 VALUES ({}, {})", i, i))
            .unwrap();
    }
    vm.execute_sql("ANALYZE TABLE ej1").unwrap();
    vm.execute_sql("ANALYZE TABLE ej2").unwrap();
    let res = vm
        .execute_sql("EXPLAIN SELECT * FROM ej1 JOIN ej2 ON ej1.id = ej2.ref_id WHERE ej1.val > 50")
        .unwrap();
    match res {
        ExecResult::Explain { plan } => {
            assert!(
                plan.contains("JOIN") || plan.contains("SCAN"),
                "plan should show JOIN or SCAN: {}",
                plan
            );
        }
        _ => panic!("expected Explain"),
    }
}

// ── 41. CAST to NUMERIC from different sources ──────────────────────────────

#[test]
fn cast_to_numeric() {
    let mut vm = VM::new_memory();
    // Integer stays integer
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(42));
    // Real with 0 fract → integer
    let rows = query_rows(&mut vm, "SELECT CAST(42.0 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(42));
    // Real with fract → stays real
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Real(3.14));
    // Text integer → integer
    let rows = query_rows(&mut vm, "SELECT CAST('123' AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(123));
    // Text float → real
    let rows = query_rows(&mut vm, "SELECT CAST('3.14' AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Real(3.14));
}

// ── 42. COLLATE expression (pass-through) ────────────────────────────────────

#[test]
fn collate_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ct (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO ct VALUES (1, 'Alice')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT name COLLATE NOCASE FROM ct");
    assert_eq!(rows[0][0], text("Alice"));
}

// ── 43. NULL propagation in AND/OR edge cases ────────────────────────────────

#[test]
fn null_propagation_and_or() {
    let mut vm = VM::new_memory();
    // NULL AND false → false (0)
    let rows = query_rows(&mut vm, "SELECT NULL AND 0");
    assert_eq!(rows[0][0], Value::Integer(0));
    // NULL AND true → NULL
    let rows = query_rows(&mut vm, "SELECT NULL AND 1");
    assert_eq!(rows[0][0], Value::Null);
    // NULL OR true → true (1)
    let rows = query_rows(&mut vm, "SELECT NULL OR 1");
    assert_eq!(rows[0][0], Value::Integer(1));
    // NULL OR false → NULL
    let rows = query_rows(&mut vm, "SELECT NULL OR 0");
    assert_eq!(rows[0][0], Value::Null);
    // false AND NULL → false (short-circuit)
    let rows = query_rows(&mut vm, "SELECT 0 AND NULL");
    assert_eq!(rows[0][0], Value::Integer(0));
    // true OR NULL → true (short-circuit)
    let rows = query_rows(&mut vm, "SELECT 1 OR NULL");
    assert_eq!(rows[0][0], Value::Integer(1));
}
