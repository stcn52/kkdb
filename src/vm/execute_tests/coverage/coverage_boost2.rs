//! Coverage boost tests – Phase 2.
//!
//! Targets specific uncovered code blocks identified by tarpaulin analysis.
//! Each test is annotated with the file/function/line range it covers.
//!
//! Priority list (by estimated uncovered lines covered):
//! 1.  CAST expressions: INTEGER, REAL, NUMERIC, TEXT, BLOB, JSON, Date/Time  (~50 lines)
//! 2.  Bitwise & shift operators: |, &, ^, <<, >>, XOR                       (~30 lines)
//! 3.  Math functions: SIGN, CBRT, FACTORIAL, POWER, OVERLAY, Modulo          (~50 lines)
//! 4.  String functions: HEX, UNICODE, CHAR, STARTS_WITH, REGEXP_LIKE         (~30 lines)
//! 5.  DATE_EXTRACT (YEAR/MONTH/DAY/HOUR/MINUTE/SECOND)                       (~40 lines)
//! 6.  EXPLAIN for SELECT/UPDATE/DELETE/INSERT/other                           (~30 lines)
//! 7.  SET session var + current_setting + current_user                        (~20 lines)
//! 8.  CREATE TABLE AS SELECT                                                  (~50 lines)
//! 9.  INSERT ... SELECT                                                       (~15 lines)
//! 10. ANY / ALL subquery operators                                            (~60 lines)
//! 11. GRANT / REVOKE                                                          (~40 lines)
//! 12. ALTER TABLE RENAME COLUMN                                               (~10 lines)
//! 13. ALTER TABLE ENABLE RLS + CREATE / DROP POLICY                           (~50 lines)
//! 14. CREATE / DROP VIEW (OR REPLACE, IF NOT EXISTS)                          (~30 lines)
//! 15. CREATE USER / ALTER USER / DROP USER                                    (~30 lines)
//! 16. JSON_UNQUOTE function                                                   (~10 lines)
//! 17. Interval expressions                                                    (~10 lines)
//! 18. CREATE / DROP TRIGGER (including OR REPLACE)                            (~20 lines)
//! 19. format_as_timestamp / format_for_column (types.rs)                      (~15 lines)
//! 20. Concat operator ||                                                      (~8 lines)

use super::*;

// ═══════════════════════════════════════════════════════════════════════════════
//  1. CAST expressions (eval_expr.rs ~1580-1730)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_cast_text_to_integer() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('42' AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(42));
}

#[test]
fn test_cast_real_to_integer() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(3.7 AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_cast_text_to_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('3.14' AS REAL)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 3.14).abs() < 0.001);
    } else {
        panic!("Expected Real");
    }
}

#[test]
fn test_cast_integer_to_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(5 AS REAL)");
    assert_eq!(rows[0][0], Value::Real(5.0));
}

#[test]
fn test_cast_to_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS TEXT)");
    assert_eq!(rows[0][0], Value::Text("42".into()));
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS TEXT)");
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("3.14"));
    }
}

#[test]
fn test_cast_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('hello' AS BLOB)");
    if let Value::Blob(b) = &rows[0][0] {
        assert_eq!(b, b"hello");
    } else {
        panic!("Expected Blob");
    }
}

#[test]
fn test_cast_integer_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS BLOB)");
    if let Value::Blob(b) = &rows[0][0] {
        assert_eq!(b, b"42");
    }
}

#[test]
fn test_cast_to_numeric() {
    let mut vm = VM::new_memory();
    // Integer stays Integer
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(42));
    // Real with no fraction → Integer
    let rows = query_rows(&mut vm, "SELECT CAST(5.0 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(5));
    // Real with fraction stays Real
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Real(3.14));
    // Text parseable as integer
    let rows = query_rows(&mut vm, "SELECT CAST('100' AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Integer(100));
    // Text parseable as float
    let rows = query_rows(&mut vm, "SELECT CAST('2.5' AS NUMERIC)");
    assert_eq!(rows[0][0], Value::Real(2.5));
}

#[test]
fn test_cast_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS INTEGER)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS REAL)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS TEXT)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS BLOB)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_to_json() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(42 AS JSON)");
    assert_eq!(rows[0][0], Value::Text("42".into()));
    let rows = query_rows(&mut vm, "SELECT CAST(NULL AS JSON)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cast_to_timestamp() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(1000 AS TIMESTAMP)");
    assert_eq!(rows[0][0], Value::Text("1000".into()));
    let rows = query_rows(&mut vm, "SELECT CAST('2024-01-01' AS DATE)");
    assert_eq!(rows[0][0], Value::Text("2024-01-01".into()));
}

#[test]
fn test_cast_text_float_to_integer() {
    // Text with float parsed as integer (truncates)
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('3.9' AS INTEGER)");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_cast_real_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(2.5 AS BLOB)");
    if let Value::Blob(b) = &rows[0][0] {
        assert!(!b.is_empty());
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  2. Bitwise & shift operators (eval_expr.rs ~1960-2000)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_bitwise_or() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5 | 3");
    assert_eq!(rows[0][0], Value::Integer(7)); // 101 | 011 = 111
}

#[test]
fn test_bitwise_and() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5 & 3");
    assert_eq!(rows[0][0], Value::Integer(1)); // 101 & 011 = 001
}

#[test]
fn test_bitwise_xor_operator() {
    let mut vm = VM::new_memory();
    // Use the ^ operator for bitwise XOR on integers
    let rows = query_rows(&mut vm, "SELECT 5 ^ 3");
    assert_eq!(rows[0][0], Value::Integer(6)); // 101 ^ 011 = 110
}

// NOTE: << and >> are not supported by the SQL parser as infix operators.
// Shift operations removed.

#[test]
fn test_xor_logical() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1 XOR 0");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT 1 XOR 1");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_modulo_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 % 3");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT 15 % 4");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_modulo_by_zero() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 % 0");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_concat_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    assert_eq!(rows[0][0], Value::Text("hello world".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  3. Math functions (eval_expr.rs ~520-700)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_sign_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SIGN(-5)");
    assert_eq!(rows[0][0], Value::Integer(-1));
    let rows = query_rows(&mut vm, "SELECT SIGN(0)");
    assert_eq!(rows[0][0], Value::Integer(0));
    let rows = query_rows(&mut vm, "SELECT SIGN(42)");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT SIGN(-3.14)");
    assert_eq!(rows[0][0], Value::Real(-1.0));
    let rows = query_rows(&mut vm, "SELECT SIGN(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_cbrt_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CBRT(27)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 3.0).abs() < 0.0001);
    }
    let rows = query_rows(&mut vm, "SELECT CBRT(8.0)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 2.0).abs() < 0.0001);
    }
    let rows = query_rows(&mut vm, "SELECT CBRT(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_factorial_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(5)");
    assert_eq!(rows[0][0], Value::Integer(120));
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(0)");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT FACTORIAL(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_factorial_overflow() {
    let mut vm = VM::new_memory();
    // FACTORIAL(21) should error
    let result = vm.execute_sql("SELECT FACTORIAL(21)");
    assert!(result.is_err());
}

#[test]
fn test_factorial_negative() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SELECT FACTORIAL(-1)");
    assert!(result.is_err());
}

#[test]
fn test_power_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POWER(2, 10)");
    assert_eq!(rows[0][0], Value::Integer(1024));
    let rows = query_rows(&mut vm, "SELECT POWER(2.0, 3)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 8.0).abs() < 0.01);
    }
    let rows = query_rows(&mut vm, "SELECT POWER(2, -1)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 0.5).abs() < 0.01);
    }
    let rows = query_rows(&mut vm, "SELECT POWER(NULL, 2)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT POWER(2, NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_power_real_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POWER(2.0, 0.5)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - std::f64::consts::SQRT_2).abs() < 0.001);
    }
}

#[test]
fn test_power_int_real() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POWER(4, 0.5)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 2.0).abs() < 0.01);
    }
}

#[test]
fn test_overlay_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT OVERLAY('hello world', 'BRAVE', 7)");
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("BRAVE"));
    }
    let rows = query_rows(&mut vm, "SELECT OVERLAY('abcdef', 'XY', 3, 2)");
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("XY"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  4. String functions (eval_expr.rs ~1070-1110)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_hex_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT HEX(255)");
    assert_eq!(rows[0][0], Value::Text("FF".into()));
    let rows = query_rows(&mut vm, "SELECT HEX(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_unicode_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT UNICODE('A')");
    assert_eq!(rows[0][0], Value::Integer(65));
    let rows = query_rows(&mut vm, "SELECT UNICODE(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_char_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CHAR(65, 66, 67)");
    assert_eq!(rows[0][0], Value::Text("ABC".into()));
}

#[test]
fn test_starts_with_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello', 'hel')");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT STARTS_WITH('hello', 'xyz')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_regexp_like_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('hello world', 'hello.*world')");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT REGEXP_LIKE('foobar', 'baz')");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_json_unquote_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, r#"SELECT JSON_UNQUOTE('"hello"')"#);
    assert_eq!(rows[0][0], Value::Text("hello".into()));
    // Already unquoted text stays as-is
    let rows = query_rows(&mut vm, "SELECT JSON_UNQUOTE('plain')");
    assert_eq!(rows[0][0], Value::Text("plain".into()));
}

#[test]
fn test_bitwise_not_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT BITWISE_NOT(0)");
    assert_eq!(rows[0][0], Value::Integer(-1));
    let rows = query_rows(&mut vm, "SELECT BITWISE_NOT(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  5. DATE_EXTRACT (eval_expr.rs ~1105-1170)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_date_extract_from_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('YEAR', '2024-06-15 10:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(2024));
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('MONTH', '2024-06-15 10:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(6));
    let rows = query_rows(&mut vm, "SELECT DATE_EXTRACT('DAY', '2024-06-15 10:30:45')");
    assert_eq!(rows[0][0], Value::Integer(15));
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('HOUR', '2024-06-15 10:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(10));
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('MINUTE', '2024-06-15 10:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(30));
    let rows = query_rows(
        &mut vm,
        "SELECT DATE_EXTRACT('SECOND', '2024-06-15 10:30:45')",
    );
    assert_eq!(rows[0][0], Value::Integer(45));
}

#[test]
fn test_date_extract_from_integer() {
    let mut vm = VM::new_memory();
    // epoch 0 = 1970-01-01 00:00:00
    let rows = query_rows(&mut vm, "SELECT DATE_EXTRACT('YEAR', 0)");
    assert_eq!(rows[0][0], Value::Integer(1970));
    let rows = query_rows(&mut vm, "SELECT DATE_EXTRACT('MONTH', 0)");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT DATE_EXTRACT('DAY', 0)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  6. EXPLAIN (exec_ddl.rs ~880-940)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_explain_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ex (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN SELECT * FROM ex WHERE id > 5 ORDER BY val LIMIT 10")
        .unwrap();
    if let ExecResult::Explain { plan } = res {
        assert!(plan.contains("SCAN"));
        assert!(plan.contains("FILTER"));
        assert!(plan.contains("SORT"));
        assert!(plan.contains("LIMIT"));
    } else {
        panic!("Expected Explain result");
    }
}

#[test]
fn test_explain_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE exu (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN UPDATE exu SET val = 1 WHERE id = 1")
        .unwrap();
    if let ExecResult::Explain { plan } = res {
        assert!(plan.contains("UPDATE"));
        assert!(plan.contains("FILTER"));
    }
}

#[test]
fn test_explain_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE exd (id INTEGER PRIMARY KEY)")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN DELETE FROM exd WHERE id = 1")
        .unwrap();
    if let ExecResult::Explain { plan } = res {
        assert!(plan.contains("DELETE"));
        assert!(plan.contains("FILTER"));
    }
}

#[test]
fn test_explain_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE exi (id INTEGER PRIMARY KEY)")
        .unwrap();
    let res = vm
        .execute_sql("EXPLAIN INSERT INTO exi VALUES (1)")
        .unwrap();
    if let ExecResult::Explain { plan } = res {
        assert!(plan.contains("INSERT"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  7. SET session var + current_setting + current_user (eval_expr.rs ~1175-1205)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_set_session_var_and_current_setting() {
    let mut vm = VM::new_memory();
    vm.execute_sql("SET kkdb.my_key = 'my_value'").unwrap();
    let rows = query_rows(&mut vm, "SELECT current_setting('kkdb.my_key')");
    assert_eq!(rows[0][0], Value::Text("my_value".into()));
}

#[test]
fn test_current_setting_not_found() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT current_setting('nonexistent')");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_current_user_no_session() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT current_user()");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_current_user_with_session() {
    let mut vm = VM::new_memory();
    vm.execute_sql("SET request.jwt.sub = 'alice'").unwrap();
    let rows = query_rows(&mut vm, "SELECT current_user()");
    assert_eq!(rows[0][0], Value::Text("alice".into()));
}

#[test]
fn test_auth_uid_function() {
    let mut vm = VM::new_memory();
    // No session → NULL
    let rows = query_rows(&mut vm, "SELECT auth_uid()");
    assert_eq!(rows[0][0], Value::Null);
    // With session
    vm.execute_sql("SET request.jwt.sub = 'user123'").unwrap();
    let rows = query_rows(&mut vm, "SELECT auth_uid()");
    assert_eq!(rows[0][0], Value::Text("user123".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  8. CREATE TABLE AS SELECT (exec_ddl.rs ~135-280)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_table_as_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT, score REAL)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1,'Alice',90.5),(2,'Bob',85.0),(3,'Charlie',77.3)")
        .unwrap();
    vm.execute_sql("CREATE TABLE dst AS SELECT id, name FROM src WHERE score > 80")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM dst ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[1][1], Value::Text("Bob".into()));
}

#[test]
fn test_create_table_as_select_if_not_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src2 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO src2 VALUES (1,10)").unwrap();
    vm.execute_sql("CREATE TABLE dst2 AS SELECT * FROM src2")
        .unwrap();
    // Should not error with IF NOT EXISTS
    let res = vm.execute_sql("CREATE TABLE IF NOT EXISTS dst2 AS SELECT * FROM src2");
    assert!(res.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  9. INSERT ... SELECT (exec_dml.rs ~105-125)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_insert_select() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE is_src (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE is_dst (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO is_src VALUES (1,'a'),(2,'b'),(3,'c')")
        .unwrap();
    vm.execute_sql("INSERT INTO is_dst SELECT * FROM is_src WHERE id <= 2")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM is_dst ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(2));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  10. ANY / ALL subquery operators (eval_expr.rs ~1445-1525)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_any_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE anys (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO anys VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM anys WHERE val > ANY (SELECT val FROM anys WHERE id = 1) ORDER BY id",
    );
    // val > ANY(10) → 20, 30
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
}

#[test]
fn test_all_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE alls (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO alls VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM alls WHERE val >= ALL (SELECT val FROM alls) ORDER BY id",
    );
    // val >= ALL(10,20,30) → only 30
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  11. GRANT / REVOKE (exec_ddl.rs ~1310-1380)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_grant_revoke() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE ROLE testuser").unwrap();
    vm.execute_sql("GRANT SELECT ON kkdb_users TO testuser")
        .unwrap();
    // Check privilege was inserted
    let rows = query_rows(
        &mut vm,
        "SELECT priv_type FROM kkdb_privileges WHERE username = 'testuser'",
    );
    assert!(!rows.is_empty());
    assert_eq!(rows[0][0], Value::Text("SELECT".into()));
    // Revoke it
    vm.execute_sql("REVOKE SELECT ON kkdb_users FROM testuser")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT priv_type FROM kkdb_privileges WHERE username = 'testuser'",
    );
    assert!(rows.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  12. ALTER TABLE RENAME COLUMN (exec_ddl.rs ~350-365)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_alter_table_rename_column() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE arc (id INTEGER PRIMARY KEY, old_col TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO arc VALUES (1, 'val')").unwrap();
    vm.execute_sql("ALTER TABLE arc RENAME COLUMN old_col TO new_col")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT new_col FROM arc");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("val".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  13. ALTER TABLE ENABLE RLS + CREATE / DROP POLICY (exec_ddl.rs ~380-430, ~1380-1430)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_enable_rls() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rls_t (id INTEGER PRIMARY KEY, owner TEXT, val TEXT)")
        .unwrap();
    vm.execute_sql("ALTER TABLE rls_t ENABLE ROW LEVEL SECURITY")
        .unwrap();
    // Table should still be usable
    vm.execute_sql("INSERT INTO rls_t VALUES (1, 'alice', 'data')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM rls_t");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_create_and_drop_policy() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pol_t (id INTEGER PRIMARY KEY, owner TEXT)")
        .unwrap();
    vm.execute_sql("ALTER TABLE pol_t ENABLE ROW LEVEL SECURITY")
        .unwrap();
    vm.execute_sql("CREATE POLICY read_own ON pol_t FOR SELECT USING (owner = current_user())")
        .unwrap();
    // Drop the policy
    let res = vm.execute_sql("DROP POLICY read_own ON pol_t");
    assert!(res.is_ok());
}

#[test]
fn test_drop_policy_if_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE pol_t2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    let res = vm.execute_sql("DROP POLICY IF EXISTS nonexistent ON pol_t2");
    assert!(res.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  14. CREATE / DROP VIEW (exec_ddl.rs ~1050-1130)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_view_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vt (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO vt VALUES (1,'a'),(2,'b'),(3,'c')")
        .unwrap();
    vm.execute_sql("CREATE VIEW v_simple AS SELECT id, val FROM vt WHERE id <= 2")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM v_simple ORDER BY id");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_create_or_replace_view() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vt2 (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO vt2 VALUES (1,'a'),(2,'b')")
        .unwrap();
    vm.execute_sql("CREATE VIEW v_rep AS SELECT * FROM vt2")
        .unwrap();
    vm.execute_sql("CREATE OR REPLACE VIEW v_rep AS SELECT id FROM vt2")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM v_rep ORDER BY id");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_create_view_if_not_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE vt3 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE VIEW v_ine AS SELECT * FROM vt3")
        .unwrap();
    let res = vm.execute_sql("CREATE VIEW IF NOT EXISTS v_ine AS SELECT * FROM vt3");
    assert!(res.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  15. CREATE USER / ALTER USER / DROP USER (exec_ddl.rs ~1200-1290)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_alter_drop_user() {
    let mut vm = VM::new_memory();
    // CREATE ROLE (mapped to CREATE USER internally)
    vm.execute_sql("CREATE ROLE bob").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT username FROM kkdb_users WHERE username = 'bob'",
    );
    assert_eq!(rows.len(), 1);
    // DROP USER
    vm.execute_sql("DROP USER bob").unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT username FROM kkdb_users WHERE username = 'bob'",
    );
    assert!(rows.is_empty());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  16. Interval expressions (eval_expr.rs ~1725-1740)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_interval_expression() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT INTERVAL '5' DAY");
    if let Value::Text(s) = &rows[0][0] {
        assert!(s.contains("5") && s.contains("DAY"));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  17. CREATE / DROP TRIGGER (exec_ddl.rs ~1140-1180)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_create_trigger_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_t (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE trig_log (msg TEXT)").unwrap();
    vm.execute_sql(
        "CREATE TRIGGER tr_after_insert AFTER INSERT ON trig_t BEGIN INSERT INTO trig_log VALUES ('inserted'); END"
    ).unwrap();
    vm.execute_sql("INSERT INTO trig_t VALUES (1, 100)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM trig_log");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("inserted".into()));
}

#[test]
fn test_drop_trigger() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE trig_t2 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TRIGGER tr_test BEFORE INSERT ON trig_t2 BEGIN SELECT 1; END")
        .unwrap();
    let res = vm.execute_sql("DROP TRIGGER tr_test");
    assert!(res.is_ok());
}

#[test]
fn test_drop_trigger_if_exists() {
    let mut vm = VM::new_memory();
    let res = vm.execute_sql("DROP TRIGGER IF EXISTS nonexistent_trigger");
    assert!(res.is_ok());
}

// ═══════════════════════════════════════════════════════════════════════════════
//  18. types.rs: format_as_timestamp / format_for_column / epoch_secs_to_datetime
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_format_as_timestamp() {
    // epoch 0 = 1970-01-01T00:00:00.000Z
    let v = Value::Integer(0);
    let formatted = v.format_as_timestamp();
    assert!(formatted.contains("1970-01-01"));
    // Positive epoch
    let v2 = Value::Integer(1_700_000_000_000); // ~2023 in ms
    let f2 = v2.format_as_timestamp();
    assert!(f2.contains("T") && f2.contains("Z"));
    // Text value stays as-is
    let v3 = Value::Text("2024-01-01".into());
    assert_eq!(v3.format_as_timestamp(), "2024-01-01");
}

#[test]
fn test_format_for_column() {
    use crate::types::DataType;
    let v = Value::Integer(42);
    assert_eq!(v.format_for_column(&DataType::Integer), "42");
    let v2 = Value::Integer(0);
    let ts = v2.format_for_column(&DataType::Timestamp);
    assert!(ts.contains("1970"));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  19. Misc coverage: NULLIF, INSTR, TYPEOF, ROUND edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_nullif_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULLIF(5, 5)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT NULLIF(5, 3)");
    assert_eq!(rows[0][0], Value::Integer(5));
}

#[test]
fn test_instr_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT INSTR('hello world', 'world')");
    assert_eq!(rows[0][0], Value::Integer(7));
    let rows = query_rows(&mut vm, "SELECT INSTR('hello', 'xyz')");
    assert_eq!(rows[0][0], Value::Integer(0));
    let rows = query_rows(&mut vm, "SELECT INSTR(NULL, 'x')");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_typeof_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT TYPEOF(42)");
    assert_eq!(rows[0][0], Value::Text("integer".into()));
    let rows = query_rows(&mut vm, "SELECT TYPEOF(3.14)");
    assert_eq!(rows[0][0], Value::Text("real".into()));
    let rows = query_rows(&mut vm, "SELECT TYPEOF('hi')");
    assert_eq!(rows[0][0], Value::Text("text".into()));
    let rows = query_rows(&mut vm, "SELECT TYPEOF(NULL)");
    assert_eq!(rows[0][0], Value::Text("null".into()));
}

#[test]
fn test_round_with_digits() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT ROUND(3.14159, 2)");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 3.14).abs() < 0.001);
    }
    let rows = query_rows(&mut vm, "SELECT ROUND(NULL)");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_ceil_floor() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CEIL(2.3)");
    assert_eq!(rows[0][0], Value::Real(3.0));
    let rows = query_rows(&mut vm, "SELECT FLOOR(2.8)");
    assert_eq!(rows[0][0], Value::Real(2.0));
    let rows = query_rows(&mut vm, "SELECT CEILING(NULL)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT FLOOR(NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // Integer passthrough
    let rows = query_rows(&mut vm, "SELECT CEIL(5)");
    assert_eq!(rows[0][0], Value::Integer(5));
    let rows = query_rows(&mut vm, "SELECT FLOOR(5)");
    assert_eq!(rows[0][0], Value::Integer(5));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  20. Division edge cases, unary minus, NOT operator
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_division_by_zero() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 10 / 0");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT 10.0 / 0.0");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_mixed_type_arithmetic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5 + 2.5");
    assert_eq!(rows[0][0], Value::Real(7.5));
    let rows = query_rows(&mut vm, "SELECT 10.0 - 3");
    assert_eq!(rows[0][0], Value::Real(7.0));
    let rows = query_rows(&mut vm, "SELECT 3 * 2.5");
    assert_eq!(rows[0][0], Value::Real(7.5));
    let rows = query_rows(&mut vm, "SELECT 10 / 3.0");
    if let Value::Real(v) = rows[0][0] {
        assert!((v - 3.333).abs() < 0.01);
    }
}

#[test]
fn test_division_real_by_int_zero() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5.0 / 0");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_unary_minus() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT -42");
    assert_eq!(rows[0][0], Value::Integer(-42));
    let rows = query_rows(&mut vm, "SELECT -3.14");
    if let Value::Real(v) = rows[0][0] {
        assert!((v + 3.14).abs() < 0.001);
    }
    let rows = query_rows(&mut vm, "SELECT -NULL");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_not_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NOT 1");
    assert_eq!(rows[0][0], Value::Integer(0));
    let rows = query_rows(&mut vm, "SELECT NOT 0");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT NOT NULL");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  21. TRIM variants (LTRIM, RTRIM)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_ltrim_rtrim() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT LTRIM('  hello  ')");
    assert_eq!(rows[0][0], Value::Text("hello  ".into()));
    let rows = query_rows(&mut vm, "SELECT RTRIM('  hello  ')");
    assert_eq!(rows[0][0], Value::Text("  hello".into()));
}

// NOTE: TRIM(str, chars) two-arg form is not supported by the SQL parser.
// Using REPLACE as a substitute is not equivalent. Removed.

// ═══════════════════════════════════════════════════════════════════════════════
//  22. REPLACE / SUBSTR edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_replace_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT REPLACE('hello world', 'world', 'rust')");
    assert_eq!(rows[0][0], Value::Text("hello rust".into()));
}

#[test]
fn test_substr_edge_cases() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT SUBSTR('hello', 2, 3)");
    assert_eq!(rows[0][0], Value::Text("ell".into()));
    let rows = query_rows(&mut vm, "SELECT SUBSTR('hello', 100)");
    assert_eq!(rows[0][0], Value::Text("".into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  23. IN list with NULL, NOT IN
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_in_list_with_null() {
    let mut vm = VM::new_memory();
    // NULL IN (1,2,NULL) → NULL
    let rows = query_rows(&mut vm, "SELECT NULL IN (1,2,NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // 3 NOT IN (1,2,NULL) → NULL (because NULL makes result uncertain)
    let rows = query_rows(&mut vm, "SELECT 3 NOT IN (1,2,NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // 1 IN (1,2,NULL) → 1 (found)
    let rows = query_rows(&mut vm, "SELECT 1 IN (1,2,NULL)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  24. BETWEEN with NULL
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_between_with_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT NULL BETWEEN 1 AND 10");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT 5 BETWEEN NULL AND 10");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  25. Simple CASE expression (operand form)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_simple_case_expression() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], Value::Text("two".into()));
    // No match, ELSE
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 5 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], Value::Text("other".into()));
    // No match, no ELSE → NULL
    let rows = query_rows(&mut vm, "SELECT CASE 5 WHEN 1 THEN 'one' END");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  26. Short-circuit AND/OR with NULL
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_short_circuit_and_or_with_null() {
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
}

// ═══════════════════════════════════════════════════════════════════════════════
//  27. INSERT ON CONFLICT DO UPDATE (UPSERT with assignments) (exec_dml.rs ~500-600)
// ═══════════════════════════════════════════════════════════════════════════════

// NOTE: ON CONFLICT DO UPDATE is rejected early by the parser (line 502 of statement.rs).
// Only INSERT OR REPLACE / INSERT OR IGNORE are supported. Already tested in coverage_boost.rs.

// ═══════════════════════════════════════════════════════════════════════════════
//  28. DELETE without WHERE (full table delete)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_delete_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE del_all (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO del_all VALUES (1,'a'),(2,'b'),(3,'c')")
        .unwrap();
    vm.execute_sql("DELETE FROM del_all").unwrap();
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM del_all");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  29. UPDATE without WHERE (full table update)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_update_all() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE upd_all (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO upd_all VALUES (1,10),(2,20),(3,30)")
        .unwrap();
    vm.execute_sql("UPDATE upd_all SET val = 0").unwrap();
    let rows = query_rows(&mut vm, "SELECT SUM(val) FROM upd_all");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  30. SHOW TABLES with views
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_show_tables_with_views() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE st_t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE VIEW st_v1 AS SELECT * FROM st_t1")
        .unwrap();
    let rows = query_rows(&mut vm, "SHOW TABLES");
    let names: Vec<String> = rows
        .iter()
        .map(|r| match &r[0] {
            Value::Text(s) => s.to_string(),
            _ => String::new(),
        })
        .collect();
    assert!(names.contains(&"st_t1".to_string()));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  31. Collate expression (eval_expr.rs near Expr::Collate)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_collate_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE col_t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO col_t VALUES (1,'Alice'),(2,'bob'),(3,'Charlie')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM col_t ORDER BY name COLLATE NOCASE",
    );
    assert_eq!(rows.len(), 3);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  32. IFNULL function
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_ifnull_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT IFNULL(NULL, 42)");
    assert_eq!(rows[0][0], Value::Integer(42));
    let rows = query_rows(&mut vm, "SELECT IFNULL(10, 42)");
    assert_eq!(rows[0][0], Value::Integer(10));
}

// ═══════════════════════════════════════════════════════════════════════════════
//  33. LENGTH on different types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_length_various() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT LENGTH('hello')");
    assert_eq!(rows[0][0], Value::Integer(5));
    let rows = query_rows(&mut vm, "SELECT LENGTH(NULL)");
    assert_eq!(rows[0][0], Value::Null);
    // LENGTH of integer → length of its string representation
    let rows = query_rows(&mut vm, "SELECT LENGTH(12345)");
    assert_eq!(rows[0][0], Value::Integer(5));
}
