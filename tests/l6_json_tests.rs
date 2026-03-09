// L6 JSON types and functions integration tests
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use std::fs;

fn setup_vm(db_dir: &str) -> VM {
    let _ = fs::remove_dir_all(db_dir);
    VM::open(db_dir).unwrap()
}

fn first_row(result: ExecResult) -> Vec<Value> {
    if let ExecResult::QueryResult { rows, .. } = result {
        rows.into_iter().next().unwrap_or_default()
    } else {
        panic!("Expected QueryResult")
    }
}

fn int(v: i64) -> Value {
    Value::Integer(v)
}
fn text(s: &str) -> Value {
    Value::Text(s.to_string().into())
}

#[test]
fn test_l6_json_extract_typed() {
    let mut vm = setup_vm("test_l6_json_extract_db");
    // JSON_EXTRACT should return native types
    let r = first_row(vm.execute_sql(r#"SELECT JSON_EXTRACT('{"name":"Alice","age":30}', '$.name'), JSON_EXTRACT('{"name":"Alice","age":30}', '$.age');"#).unwrap());
    assert_eq!(r[0], text("Alice"), "name should be Text");
    assert_eq!(r[1], int(30), "age should be Integer");

    // null value
    let r2 = first_row(
        vm.execute_sql(r#"SELECT JSON_EXTRACT('{"x":null}', '$.x');"#)
            .unwrap(),
    );
    assert_eq!(r2[0], Value::Null);
}

#[test]
fn test_l6_json_type() {
    let mut vm = setup_vm("test_l6_json_type_db");
    let r = first_row(vm.execute_sql(r#"SELECT JSON_TYPE('{"a":1}'), JSON_TYPE('[1,2]'), JSON_TYPE('"hello"'), JSON_TYPE('42'), JSON_TYPE('true'), JSON_TYPE('null');"#).unwrap());
    assert_eq!(r[0], text("OBJECT"));
    assert_eq!(r[1], text("ARRAY"));
    assert_eq!(r[2], text("STRING"));
    assert_eq!(r[3], text("INTEGER"));
    assert_eq!(r[4], text("BOOLEAN"));
    assert_eq!(r[5], text("NULL"));
}

#[test]
fn test_l6_json_valid() {
    let mut vm = setup_vm("test_l6_json_valid_db");
    let r = first_row(vm.execute_sql(r#"SELECT JSON_VALID('{"a":1}'), JSON_VALID('[1,2,3]'), JSON_VALID('not json'), JSON_VALID('null');"#).unwrap());
    assert_eq!(r[0], int(1), "object is valid");
    assert_eq!(r[1], int(1), "array is valid");
    assert_eq!(r[2], int(0), "bare string without quotes is invalid");
    assert_eq!(r[3], int(1), "null is valid");
}

#[test]
fn test_l6_json_length() {
    let mut vm = setup_vm("test_l6_json_length_db");
    let r = first_row(
        vm.execute_sql(
            r#"SELECT JSON_LENGTH('[1,2,3]'), JSON_LENGTH('{"a":1,"b":2}'), JSON_LENGTH('[]');"#,
        )
        .unwrap(),
    );
    assert_eq!(r[0], int(3), "array of 3 elements");
    assert_eq!(r[1], int(2), "object with 2 keys");
    assert_eq!(r[2], int(0), "empty array");
}

#[test]
fn test_l6_json_keys() {
    let mut vm = setup_vm("test_l6_json_keys_db");
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_KEYS('{"name":"Alice","age":30}');"#)
            .unwrap(),
    );
    // Should return a JSON array with the keys
    if let Value::Text(s) = &r[0] {
        assert!(s.contains("\"name\""), "should contain 'name' key");
        assert!(s.contains("\"age\""), "should contain 'age' key");
        assert!(
            s.starts_with('[') && s.ends_with(']'),
            "should be a JSON array"
        );
    } else {
        panic!("expected Text result from JSON_KEYS, got {:?}", r[0]);
    }
}

#[test]
fn test_l6_json_remove() {
    let mut vm = setup_vm("test_l6_json_remove_db");
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_REMOVE('{"a":1,"b":2,"c":3}', '$.b');"#)
            .unwrap(),
    );
    if let Value::Text(s) = &r[0] {
        assert!(!s.contains("\"b\""), "key 'b' should be removed");
        assert!(s.contains("\"a\""), "key 'a' should remain");
        assert!(s.contains("\"c\""), "key 'c' should remain");
    } else {
        panic!("expected Text result from JSON_REMOVE, got {:?}", r[0]);
    }
}

#[test]
fn test_l6_json_set() {
    let mut vm = setup_vm("test_l6_json_set_db");
    // JSON_SET: update existing key or insert new key
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_SET('{"a":1}', '$.b', 2);"#)
            .unwrap(),
    );
    if let Value::Text(s) = &r[0] {
        assert!(s.contains("\"b\""), "new key 'b' should be added: {s}");
        assert!(s.contains('2'), "value 2 should be there: {s}");
    } else {
        panic!("expected Text from JSON_SET, got {:?}", r[0]);
    }
}

#[test]
fn test_l6_json_array_and_object() {
    let mut vm = setup_vm("test_l6_json_array_obj_db");
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_ARRAY(1, 'hello', NULL), JSON_OBJECT('x', 10, 'y', 20);"#)
            .unwrap(),
    );
    assert_eq!(r[0], text(r#"[1, "hello", null]"#), "JSON_ARRAY");
    assert_eq!(r[1], text(r#"{"x": 10, "y": 20}"#), "JSON_OBJECT");
}

#[test]
fn test_l6_json_quote_unquote() {
    let mut vm = setup_vm("test_l6_json_quote_db");
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_QUOTE('hello'), JSON_UNQUOTE('"world"');"#)
            .unwrap(),
    );
    assert_eq!(r[0], text(r#""hello""#), "JSON_QUOTE should wrap in quotes");
    assert_eq!(r[1], text("world"), "JSON_UNQUOTE should strip quotes");
}

#[test]
fn test_l6_json_member_of() {
    let mut vm = setup_vm("test_l6_json_member_db");
    let r = first_row(
        vm.execute_sql(r#"SELECT JSON_MEMBER_OF(2, '[1,2,3]'), JSON_MEMBER_OF(5, '[1,2,3]');"#)
            .unwrap(),
    );
    assert_eq!(r[0], int(1), "2 should be member of [1,2,3]");
    assert_eq!(r[1], int(0), "5 should not be member of [1,2,3]");
}
