//! R30: API 整理 + rustdoc 文档 + 运维增强（WAL checkpoint / REINDEX / SHUTDOWN）

use super::*;

// ── R30a: rustdoc 存在性检验 ─────────────────────────────────────────────

#[test]
fn test_r30_value_is_truthy() {
    assert!(Value::Integer(1).is_truthy());
    assert!(!Value::Integer(0).is_truthy());
    assert!(Value::Real(0.1).is_truthy());
    assert!(!Value::Real(0.0).is_truthy());
    assert!(Value::Text("hello".into()).is_truthy());
    assert!(!Value::Text("".into()).is_truthy());
    assert!(!Value::Null.is_truthy());
    assert!(!Value::Blob(vec![]).is_truthy());
    assert!(Value::Blob(vec![1]).is_truthy());
}

#[test]
fn test_r30_value_to_i64() {
    assert_eq!(Value::Integer(42).to_i64(), Some(42));
    assert_eq!(Value::Real(3.9).to_i64(), Some(3));
    assert_eq!(Value::Text("100".into()).to_i64(), Some(100));
    assert_eq!(Value::Text("abc".into()).to_i64(), None);
    assert_eq!(Value::Null.to_i64(), None);
    assert_eq!(Value::Blob(vec![]).to_i64(), None);
}

#[test]
fn test_r30_value_to_f64() {
    assert_eq!(Value::Integer(42).to_f64(), Some(42.0));
    assert_eq!(Value::Real(3.14).to_f64(), Some(3.14));
    assert_eq!(Value::Text("2.5".into()).to_f64(), Some(2.5));
    assert_eq!(Value::Text("xyz".into()).to_f64(), None);
    assert_eq!(Value::Null.to_f64(), None);
}

#[test]
fn test_r30_value_data_type() {
    use crate::types::DataType;
    assert_eq!(Value::Null.data_type(), DataType::Null);
    assert_eq!(Value::Integer(1).data_type(), DataType::Integer);
    assert_eq!(Value::Real(1.0).data_type(), DataType::Real);
    assert_eq!(Value::Text("x".into()).data_type(), DataType::Text);
    assert_eq!(Value::Blob(vec![]).data_type(), DataType::Blob);
}

#[test]
fn test_r30_datatype_from_str() {
    use crate::types::DataType;
    assert_eq!(DataType::from_str("INTEGER"), DataType::Integer);
    assert_eq!(DataType::from_str("int"), DataType::Integer);
    assert_eq!(DataType::from_str("BIGINT"), DataType::Integer);
    assert_eq!(DataType::from_str("REAL"), DataType::Real);
    assert_eq!(DataType::from_str("FLOAT"), DataType::Real);
    assert_eq!(DataType::from_str("DECIMAL"), DataType::Real);
    assert_eq!(DataType::from_str("DECIMAL(10,2)"), DataType::Real);
    assert_eq!(DataType::from_str("TEXT"), DataType::Text);
    assert_eq!(DataType::from_str("VARCHAR"), DataType::Text);
    assert_eq!(DataType::from_str("BLOB"), DataType::Blob);
    assert_eq!(DataType::from_str("TIMESTAMP"), DataType::Timestamp);
    assert_eq!(DataType::from_str("DATETIME"), DataType::Timestamp);
}

#[test]
fn test_r30_row_type_alias() {
    // Row is Vec<Value> — verify construction and indexing
    let row: crate::types::Row = vec![Value::Integer(1), Value::Text("hello".into())];
    assert_eq!(row.len(), 2);
    assert_eq!(row[0].to_i64(), Some(1));
}

#[test]
fn test_r30_prefix_page_codec() {
    use crate::types::{PrefixPageDecoder, PrefixPageEncoder};

    let mut encoder = PrefixPageEncoder::new();
    let mut decoder = PrefixPageDecoder::new();

    let rows = vec![
        vec![Value::Text("apple".into()), Value::Integer(1)],
        vec![Value::Text("application".into()), Value::Integer(2)],
        vec![Value::Text("apply".into()), Value::Integer(3)],
    ];

    let mut encoded_blobs = Vec::new();
    for row in &rows {
        encoded_blobs.push(encoder.encode(row));
    }

    // Decode and verify
    for (i, blob) in encoded_blobs.iter().enumerate() {
        let decoded = decoder.decode(blob).unwrap();
        // Check that the text column matches
        if let Value::Text(t) = &decoded[0] {
            if let Value::Text(expected) = &rows[i][0] {
                assert_eq!(t.as_ref(), expected.as_ref());
            }
        }
        // Check the integer column
        assert_eq!(decoded[1].to_i64(), rows[i][1].to_i64());
    }
}

// ── R30b: WAL checkpoint ─────────────────────────────────────────────────

#[test]
fn test_r30_pragma_wal_checkpoint_no_wal() {
    let mut vm = VM::new_memory();
    // WAL is not enabled on a memory DB by default
    let result = vm.execute_sql("PRAGMA wal_checkpoint").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("not enabled"), "message: {}", message);
        }
        other => panic!("Expected Ok, got {:?}", other),
    }
}

#[test]
fn test_r30_pragma_wal_checkpoint_with_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_r30_wal_cp");
    let mut vm = VM::open(db_path.to_str().unwrap()).unwrap();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'hello')").unwrap();

    // Enable WAL
    vm.pager.enable_wal().unwrap();

    // Insert more data (goes to WAL)
    vm.execute_sql("INSERT INTO t VALUES (2, 'world')").unwrap();

    // Checkpoint
    let result = vm.execute_sql("PRAGMA wal_checkpoint").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("completed"), "message: {}", message);
        }
        other => panic!("Expected Ok, got {:?}", other),
    }

    // Data should still be accessible
    let rows = query_rows(&mut vm, "SELECT * FROM t ORDER BY id");
    assert_eq!(rows.len(), 2);
}

// ── R30b: REINDEX ────────────────────────────────────────────────────────

#[test]
fn test_r30_reindex_no_indexes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    let result = vm.execute_sql("REINDEX t").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("No indexes"), "message: {}", message);
        }
        other => panic!("Expected Ok, got {:?}", other),
    }
}

#[test]
fn test_r30_reindex_with_indexes() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT, age INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_t_name ON t (name)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_t_age ON t (age)").unwrap();

    // Insert data
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice', 30)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'Bob', 25)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 'Charlie', 35)")
        .unwrap();

    // Reindex
    let result = vm.execute_sql("REINDEX t").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("Rebuilt 2"), "message: {}", message);
        }
        other => panic!("Expected Ok, got {:?}", other),
    }

    // Verify data is still intact and indexes work
    let rows = query_rows(&mut vm, "SELECT * FROM t WHERE name = 'Bob'");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0].to_i64(), Some(2));
}

#[test]
fn test_r30_reindex_unique_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, email TEXT)")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX idx_t_email ON t (email)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'a@b.com')")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'c@d.com')")
        .unwrap();

    let result = vm.execute_sql("REINDEX t").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(message.contains("Rebuilt 1"), "message: {}", message);
        }
        other => panic!("Expected Ok, got {:?}", other),
    }

    // Verify unique constraint still works after reindex
    let err = vm.execute_sql("INSERT INTO t VALUES (3, 'a@b.com')");
    assert!(
        err.is_err(),
        "Unique constraint should be enforced after REINDEX"
    );
}

#[test]
fn test_r30_reindex_empty_name() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("REINDEX ");
    assert!(result.is_err());
}

// ── R30b: SHUTDOWN ───────────────────────────────────────────────────────

#[test]
fn test_r30_shutdown_memory_db() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();

    let result = vm.execute_sql("SHUTDOWN").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("shutdown completed"),
                "message: {}",
                message
            );
        }
        other => panic!("Expected Ok, got {:?}", other),
    }
}

#[test]
fn test_r30_shutdown_file_db() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_r30_shutdown");
    let mut vm = VM::open(db_path.to_str().unwrap()).unwrap();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'before shutdown')")
        .unwrap();

    let result = vm.execute_sql("SHUTDOWN").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("shutdown completed"),
                "message: {}",
                message
            );
        }
        other => panic!("Expected Ok, got {:?}", other),
    }

    // Verify data survives by reopening
    drop(vm);
    let mut vm2 = VM::open(db_path.to_str().unwrap()).unwrap();
    let rows = query_rows(&mut vm2, "SELECT * FROM t");
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_r30_shutdown_with_wal() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_r30_shutdown_wal");
    let mut vm = VM::open(db_path.to_str().unwrap()).unwrap();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.pager.enable_wal().unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();

    // Shutdown should checkpoint WAL
    let result = vm.execute_sql("SHUTDOWN").unwrap();
    match result {
        ExecResult::Ok { message } => {
            assert!(
                message.contains("shutdown completed"),
                "message: {}",
                message
            );
        }
        other => panic!("Expected Ok, got {:?}", other),
    }
}

#[test]
fn test_r30_shutdown_clears_caches() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    // Execute multiple SQL to fill statement cache
    for i in 0..10 {
        vm.execute_sql(&format!("INSERT INTO t VALUES ({})", i))
            .unwrap();
    }
    vm.execute_sql("SELECT * FROM t").unwrap();

    vm.execute_sql("SHUTDOWN").unwrap();

    // Cache should be empty — subsequent SQL should still work
    let result = vm.execute_sql("SELECT count(*) FROM t").unwrap();
    match result {
        ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows[0][0].to_i64(), Some(10));
        }
        other => panic!("Expected QueryResult, got {:?}", other),
    }
}

// ── R30: Combined operations flow ────────────────────────────────────────

#[test]
fn test_r30_full_ops_flow() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_r30_full_ops");
    let mut vm = VM::open(db_path.to_str().unwrap()).unwrap();

    // Create table and index
    vm.execute_sql("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT, price REAL)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_products_name ON products (name)")
        .unwrap();

    // Insert data
    for i in 0..20 {
        vm.execute_sql(&format!(
            "INSERT INTO products VALUES ({}, 'product_{}', {}.99)",
            i,
            i,
            i * 10
        ))
        .unwrap();
    }

    // REINDEX
    let reindex_result = vm.execute_sql("REINDEX products").unwrap();
    match &reindex_result {
        ExecResult::Ok { message } => assert!(message.contains("Rebuilt 1")),
        other => panic!("Expected Ok, got {:?}", other),
    }

    // Verify queries still work after reindex
    let rows = query_rows(&mut vm, "SELECT * FROM products WHERE name = 'product_5'");
    assert_eq!(rows.len(), 1);

    // Enable WAL and add more data
    vm.pager.enable_wal().unwrap();
    vm.execute_sql("INSERT INTO products VALUES (100, 'extra', 999.99)")
        .unwrap();

    // WAL Checkpoint
    let cp_result = vm.execute_sql("PRAGMA wal_checkpoint").unwrap();
    match &cp_result {
        ExecResult::Ok { message } => assert!(message.contains("completed")),
        other => panic!("Expected Ok, got {:?}", other),
    }

    // SHUTDOWN
    let shutdown_result = vm.execute_sql("SHUTDOWN").unwrap();
    match &shutdown_result {
        ExecResult::Ok { message } => assert!(message.contains("shutdown completed")),
        other => panic!("Expected Ok, got {:?}", other),
    }
}
