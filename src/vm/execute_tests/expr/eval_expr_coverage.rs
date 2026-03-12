//! Coverage-boosting tests for eval_expr.rs
//!
//! Targets uncovered branches: CAST, CASE/WHEN, ANY/ALL, EXISTS,
//! date functions, string functions, JSON helpers, bitwise ops, etc.

use super::*;

// ═══════════════════════════════════════════════════════════════════════════════
// CAST expressions (~123 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_cast_text_to_integer() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST('42' AS INTEGER)")[0][0],
        Value::Integer(42)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST('3.14' AS INTEGER)")[0][0],
        Value::Integer(3)
    );
    // Non-numeric text → error
    let result = vm.execute_sql("SELECT CAST('abc' AS INTEGER)");
    assert!(result.is_err());
}

#[test]
#[allow(clippy::approx_constant)]
fn test_cast_text_to_real() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST('3.14' AS REAL)")[0][0],
        Value::Real(3.14)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST('42' AS REAL)")[0][0],
        Value::Real(42.0)
    );
}

#[test]
fn test_cast_integer_to_text() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(42 AS TEXT)")[0][0],
        Value::Text("42".to_string().into())
    );
}

#[test]
fn test_cast_real_to_text() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST(3.14 AS TEXT)");
    match &rows[0][0] {
        Value::Text(s) => assert!(s.contains("3.14"), "expected '3.14' in '{}'", s),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn test_cast_null() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(NULL AS INTEGER)")[0][0],
        Value::Null
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(NULL AS TEXT)")[0][0],
        Value::Null
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(NULL AS REAL)")[0][0],
        Value::Null
    );
}

#[test]
fn test_cast_integer_to_real() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(5 AS REAL)")[0][0],
        Value::Real(5.0)
    );
}

#[test]
fn test_cast_real_to_integer() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CAST(9.99 AS INTEGER)")[0][0],
        Value::Integer(9)
    );
}

#[test]
fn test_cast_to_blob() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CAST('hello' AS BLOB)");
    assert_eq!(rows[0][0], Value::Blob(b"hello".to_vec()));
}

#[test]
fn test_cast_blob_to_text() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(b BLOB)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (X'68656C6C6F')")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT CAST(b AS TEXT) FROM t");
    assert_eq!(rows[0][0], Value::Text("hello".to_string().into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// CASE/WHEN expressions (~29 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_case_simple() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 1 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], Value::Text("one".to_string().into()));
}

#[test]
fn test_case_simple_else() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 3 WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], Value::Text("other".to_string().into()));
}

#[test]
fn test_case_simple_no_else() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CASE 99 WHEN 1 THEN 'one' END");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_case_searched() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE WHEN 1 > 2 THEN 'a' WHEN 2 > 1 THEN 'b' ELSE 'c' END",
    );
    assert_eq!(rows[0][0], Value::Text("b".to_string().into()));
}

#[test]
fn test_case_searched_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CASE WHEN NULL THEN 'a' ELSE 'b' END");
    assert_eq!(rows[0][0], Value::Text("b".to_string().into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Date/time functions (~39 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_date_extract_from_text() {
    let mut vm = VM::new_memory();
    // EXTRACT(YEAR FROM '2024-03-15')
    let rows = query_rows(&mut vm, "SELECT EXTRACT(YEAR FROM '2024-03-15')");
    assert_eq!(rows[0][0], Value::Integer(2024));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(MONTH FROM '2024-03-15')");
    assert_eq!(rows[0][0], Value::Integer(3));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(DAY FROM '2024-03-15')");
    assert_eq!(rows[0][0], Value::Integer(15));
}

#[test]
fn test_date_extract_datetime() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT EXTRACT(HOUR FROM '2024-03-15 14:30:45')");
    assert_eq!(rows[0][0], Value::Integer(14));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(MINUTE FROM '2024-03-15 14:30:45')");
    assert_eq!(rows[0][0], Value::Integer(30));
    let rows = query_rows(&mut vm, "SELECT EXTRACT(SECOND FROM '2024-03-15 14:30:45')");
    assert_eq!(rows[0][0], Value::Integer(45));
}

// ═══════════════════════════════════════════════════════════════════════════════
// String functions (~37 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_starts_with() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT STARTS_WITH('hello world', 'hello')")[0][0],
        Value::Integer(1)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT STARTS_WITH('hello', 'world')")[0][0],
        Value::Integer(0)
    );
}

#[test]
fn test_hex_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT HEX(255)");
    assert_eq!(rows[0][0], Value::Text("FF".to_string().into()));
    let rows = query_rows(&mut vm, "SELECT HEX(0)");
    assert_eq!(rows[0][0], Value::Text("0".to_string().into()));
}

#[test]
fn test_unicode_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT UNICODE('A')");
    assert_eq!(rows[0][0], Value::Integer(65));
}

#[test]
fn test_char_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CHAR(72, 101, 108, 108, 111)");
    assert_eq!(rows[0][0], Value::Text("Hello".to_string().into()));
}

#[test]
fn test_position_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT POSITION('ll' IN 'hello')");
    assert_eq!(rows[0][0], Value::Integer(3));
}

#[test]
fn test_overlay_function() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT OVERLAY('hello' PLACING 'XX' FROM 2 FOR 3)");
    assert_eq!(rows[0][0], Value::Text("hXXo".to_string().into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Math functions (~28 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_ceil_floor() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT CEIL(3.2)")[0][0],
        Value::Real(4.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT CEILING(3.2)")[0][0],
        Value::Real(4.0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT FLOOR(3.8)")[0][0],
        Value::Real(3.0)
    );
    // Integer passthrough
    assert_eq!(
        query_rows(&mut vm, "SELECT CEIL(5)")[0][0],
        Value::Integer(5)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT FLOOR(5)")[0][0],
        Value::Integer(5)
    );
}

#[test]
fn test_round_function() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT ROUND(3.567, 2)")[0][0],
        Value::Real(3.57)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT ROUND(3.5)")[0][0],
        Value::Real(4.0)
    );
}

#[test]
fn test_power_function() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT POWER(2, 10)")[0][0],
        Value::Real(1024.0)
    );
}

#[test]
fn test_sign_function() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT SIGN(-5)")[0][0],
        Value::Integer(-1)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT SIGN(0)")[0][0],
        Value::Integer(0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT SIGN(42)")[0][0],
        Value::Integer(1)
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Regex function (~32 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_regexp_like() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT REGEXP_LIKE('hello123', 'hello')")[0][0],
        Value::Integer(1)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT REGEXP_LIKE('hello', '^world$')")[0][0],
        Value::Integer(0)
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// LIKE with escape char (~9 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_like_escape() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(s TEXT)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES ('100%'), ('1000'), ('10')")
        .unwrap();
    // LIKE '100\\%' ESCAPE '\\' should match only '100%'
    let rows = query_rows(&mut vm, "SELECT s FROM t WHERE s LIKE '100\\%' ESCAPE '\\'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("100%".to_string().into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// BETWEEN edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_between_null_boundary() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 5 BETWEEN NULL AND 10");
    assert_eq!(rows[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
// IN list NULL semantics
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_in_list_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1 IN (2, 3, NULL)");
    assert_eq!(rows[0][0], Value::Null);
    let rows = query_rows(&mut vm, "SELECT 1 IN (1, 2, NULL)");
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Bitwise / shift operators (~20 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_bitwise_ops() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE bits(a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO bits VALUES(6, 3)").unwrap();
    assert_eq!(
        query_rows(&mut vm, "SELECT a & b FROM bits")[0][0],
        Value::Integer(2)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT a | b FROM bits")[0][0],
        Value::Integer(7)
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// IS DISTINCT FROM
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_is_distinct_from() {
    let mut vm = VM::new_memory();
    // NULL IS DISTINCT FROM NULL → false (0)
    let rows = query_rows(&mut vm, "SELECT NULL IS DISTINCT FROM NULL");
    assert_eq!(rows[0][0], Value::Integer(0));
    // 1 IS DISTINCT FROM NULL → true (1)
    let rows = query_rows(&mut vm, "SELECT 1 IS DISTINCT FROM NULL");
    assert_eq!(rows[0][0], Value::Integer(1));
    // 1 IS DISTINCT FROM 1 → false (0)
    let rows = query_rows(&mut vm, "SELECT 1 IS DISTINCT FROM 1");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Unary minus / NOT
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_unary_not() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT NOT 1")[0][0], Value::Integer(0));
    assert_eq!(query_rows(&mut vm, "SELECT NOT 0")[0][0], Value::Integer(1));
    assert_eq!(query_rows(&mut vm, "SELECT NOT NULL")[0][0], Value::Null);
}

#[test]
#[allow(clippy::approx_constant)]
fn test_unary_minus() {
    let mut vm = VM::new_memory();
    assert_eq!(query_rows(&mut vm, "SELECT -42")[0][0], Value::Integer(-42));
    assert_eq!(
        query_rows(&mut vm, "SELECT -3.14")[0][0],
        Value::Real(-3.14)
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// NULL propagation in binary ops
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_null_and_or_propagation() {
    let mut vm = VM::new_memory();
    // NULL AND false = false
    assert_eq!(
        query_rows(&mut vm, "SELECT NULL AND 0")[0][0],
        Value::Integer(0)
    );
    // NULL AND true = NULL
    assert_eq!(query_rows(&mut vm, "SELECT NULL AND 1")[0][0], Value::Null);
    // NULL OR true = true
    assert_eq!(
        query_rows(&mut vm, "SELECT NULL OR 1")[0][0],
        Value::Integer(1)
    );
    // NULL OR false = NULL
    assert_eq!(query_rows(&mut vm, "SELECT NULL OR 0")[0][0], Value::Null);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Subquery: EXISTS (~15 uncovered lines)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_exists_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'a')").unwrap();
    // EXISTS with rows
    let rows = query_rows(&mut vm, "SELECT EXISTS(SELECT 1 FROM t WHERE id = 1)");
    assert_eq!(rows[0][0], Value::Integer(1));
    // EXISTS with no rows
    let rows = query_rows(&mut vm, "SELECT EXISTS(SELECT 1 FROM t WHERE id = 999)");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════════════════════════════
// Scalar subquery
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_scalar_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums(n INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO nums VALUES (10), (20), (30)")
        .unwrap();
    let rows = query_rows(&mut vm, "SELECT (SELECT MAX(n) FROM nums)");
    assert_eq!(rows[0][0], Value::Integer(30));
}

// ═══════════════════════════════════════════════════════════════════════════════
// IN subquery
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_in_subquery() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ids(id INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO ids VALUES (1), (3), (5)")
        .unwrap();
    vm.execute_sql("CREATE TABLE data(id INTEGER, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO data VALUES (1, 'a'), (2, 'b'), (3, 'c'), (4, 'd')")
        .unwrap();
    let rows = query_rows(
        &mut vm,
        "SELECT name FROM data WHERE id IN (SELECT id FROM ids) ORDER BY id",
    );
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("a".to_string().into()));
    assert_eq!(rows[1][0], Value::Text("c".to_string().into()));
}

// ═══════════════════════════════════════════════════════════════════════════════
// JSON helper functions (json_array_get, json_set_path, json_remove_path)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_json_quote() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_QUOTE('hello')");
    assert_eq!(rows[0][0], Value::Text("\"hello\"".to_string().into()));
}

#[test]
fn test_json_valid() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT JSON_VALID('{\"a\":1}')")[0][0],
        Value::Integer(1)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT JSON_VALID('not json')")[0][0],
        Value::Integer(0)
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT JSON_VALID('[1,2,3]')")[0][0],
        Value::Integer(1)
    );
}

#[test]
fn test_json_extract() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT JSON_EXTRACT('{\"name\":\"Alice\",\"age\":30}', '$.name')",
    );
    assert_eq!(rows[0][0], Value::Text("Alice".to_string().into()));
    let rows = query_rows(
        &mut vm,
        "SELECT JSON_EXTRACT('{\"name\":\"Alice\",\"age\":30}', '$.age')",
    );
    assert_eq!(rows[0][0], Value::Integer(30));
}

#[test]
fn test_json_length() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_LENGTH('[1,2,3]')");
    assert_eq!(rows[0][0], Value::Integer(3));
    let rows = query_rows(&mut vm, "SELECT JSON_LENGTH('{\"a\":1,\"b\":2}')");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_json_keys() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_KEYS('{\"b\":1,\"a\":2}')");
    match &rows[0][0] {
        Value::Text(s) => {
            // keys order may vary, just check both are present
            assert!(s.contains("\"a\"") && s.contains("\"b\""), "got: {}", s);
        }
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn test_json_set() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_SET('{\"a\":1}', '$.b', 2)");
    match &rows[0][0] {
        Value::Text(s) => {
            assert!(s.contains("\"a\"") && s.contains("\"b\""), "got: {}", s);
        }
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn test_json_remove() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT JSON_REMOVE('{\"a\":1,\"b\":2}', '$.a')");
    match &rows[0][0] {
        Value::Text(s) => {
            assert!(!s.contains("\"a\""), "expected 'a' removed, got: {}", s);
            assert!(s.contains("\"b\""), "expected 'b' kept, got: {}", s);
        }
        other => panic!("expected Text, got {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Misc scalar functions
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_nullif_function() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT NULLIF(1, 1)")[0][0],
        Value::Null
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT NULLIF(1, 2)")[0][0],
        Value::Integer(1)
    );
}

#[test]
fn test_typeof_function() {
    let mut vm = VM::new_memory();
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(42)")[0][0],
        Value::Text("integer".to_string().into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(3.14)")[0][0],
        Value::Text("real".to_string().into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF('hi')")[0][0],
        Value::Text("text".to_string().into())
    );
    assert_eq!(
        query_rows(&mut vm, "SELECT TYPEOF(NULL)")[0][0],
        Value::Text("null".to_string().into())
    );
}
