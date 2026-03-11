use super::*;

#[test]
fn test_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
    let err = KkdbError::from(io_err);
    assert!(format!("{}", err).contains("IO error"));
}

#[test]
fn test_syntax_error() {
    let err = KkdbError::SyntaxError("bad token".into());
    assert_eq!(format!("{}", err), "Syntax error: bad token");
}

#[test]
fn test_parse_error() {
    let err = KkdbError::ParseError("unexpected".into());
    assert_eq!(format!("{}", err), "Parse error: unexpected");
}

#[test]
fn test_runtime_error() {
    let err = KkdbError::RuntimeError("fail".into());
    assert_eq!(format!("{}", err), "Runtime error: fail");
}

#[test]
fn test_type_error() {
    let err = KkdbError::TypeError("mismatch".into());
    assert_eq!(format!("{}", err), "Type error: mismatch");
}

#[test]
fn test_table_already_exists() {
    let err = KkdbError::TableAlreadyExists("t1".into());
    assert_eq!(format!("{}", err), "Table 't1' already exists");
}

#[test]
fn test_table_not_found() {
    let err = KkdbError::TableNotFound("t2".into());
    assert_eq!(format!("{}", err), "Table 't2' not found");
}

#[test]
fn test_column_not_found() {
    let err = KkdbError::ColumnNotFound("col1".into());
    assert_eq!(format!("{}", err), "Column 'col1' not found");
}

#[test]
fn test_column_count_mismatch() {
    let err = KkdbError::ColumnCountMismatch {
        expected: 3,
        got: 2,
    };
    assert_eq!(
        format!("{}", err),
        "Column count mismatch: expected 3, got 2"
    );
}

#[test]
fn test_page_out_of_range() {
    let err = KkdbError::PageOutOfRange(999);
    assert_eq!(format!("{}", err), "Page 999 out of range");
}

#[test]
fn test_btree_error() {
    let err = KkdbError::BTreeError("split fail".into());
    assert_eq!(format!("{}", err), "B-tree error: split fail");
}

#[test]
fn test_database_full() {
    let err = KkdbError::DatabaseFull;
    assert_eq!(format!("{}", err), "Database is full");
}

#[test]
fn test_corrupt_database() {
    let err = KkdbError::CorruptDatabase("bad header".into());
    assert_eq!(format!("{}", err), "Corrupt database: bad header");
}

#[test]
fn test_constraint_violation() {
    let err = KkdbError::ConstraintViolation("NOT NULL".into());
    assert_eq!(format!("{}", err), "Constraint violation: NOT NULL");
}

#[test]
fn test_internal_error() {
    let err = KkdbError::Internal("oops".into());
    assert_eq!(format!("{}", err), "Internal error: oops");
}

#[test]
#[allow(clippy::unnecessary_literal_unwrap)]
fn test_result_type_alias() {
    let ok: Result<i32> = Ok(42);
    assert_eq!(ok.unwrap(), 42);
    let err: Result<i32> = Err(KkdbError::DatabaseFull);
    assert!(err.is_err());
}
