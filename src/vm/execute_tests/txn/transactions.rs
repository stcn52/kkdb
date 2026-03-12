use super::*;

// ---- Transaction Tests ----

#[test]
fn test_begin_commit_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_rollback_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 'Charlie')")
        .unwrap();

    // Verify rows visible within transaction
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 3);

    vm.execute_sql("ROLLBACK").unwrap();

    // After rollback, only original row remains
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_rollback_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE t1 SET name = 'Alicia' WHERE id = 1")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT name FROM t1 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("Alicia".into()));

    vm.execute_sql("ROLLBACK").unwrap();

    let rows = query_rows(&mut vm, "SELECT name FROM t1 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_rollback_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DELETE FROM t1 WHERE id = 2").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);

    vm.execute_sql("ROLLBACK").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_rollback_create_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    // Table should not exist after rollback
    let result = vm.execute_sql("SELECT * FROM t1");
    assert!(result.is_err());
}

#[test]
fn test_rollback_drop_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DROP TABLE t1").unwrap();

    // Table should be gone within transaction
    let result = vm.execute_sql("SELECT * FROM t1");
    assert!(result.is_err());

    vm.execute_sql("ROLLBACK").unwrap();

    // Table should be restored after rollback
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_nested_begin_error() {
    let mut vm = VM::new_memory();
    vm.execute_sql("BEGIN").unwrap();
    let result = vm.execute_sql("BEGIN");
    assert!(result.is_err()); // nested BEGIN should fail
}

#[test]
fn test_rollback_without_begin() {
    let mut vm = VM::new_memory();
    // ROLLBACK without BEGIN should be a no-op (SQLite behavior)
    let result = vm.execute_sql("ROLLBACK");
    assert!(result.is_ok());
}

#[test]
fn test_commit_without_begin() {
    let mut vm = VM::new_memory();
    // COMMIT without BEGIN should still work (just flushes)
    let result = vm.execute_sql("COMMIT");
    assert!(result.is_ok());
}

#[test]
fn test_transaction_multiple_operations() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 30)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DELETE FROM t1 WHERE id = 1").unwrap();
    vm.execute_sql("UPDATE t1 SET val = 200 WHERE id = 2")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (4, 40)").unwrap();

    // Verify mid-transaction state
    let rows = query_rows(&mut vm, "SELECT id, val FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 3); // 1 deleted, 1 updated, 1 inserted
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[0][1], Value::Integer(200));
    assert_eq!(rows[2][0], Value::Integer(4));

    vm.execute_sql("ROLLBACK").unwrap();

    // Everything should be back to original
    let rows = query_rows(&mut vm, "SELECT id, val FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(10));
    assert_eq!(rows[1][0], Value::Integer(2));
    assert_eq!(rows[1][1], Value::Integer(20));
    assert_eq!(rows[2][0], Value::Integer(3));
    assert_eq!(rows[2][1], Value::Integer(30));
}

#[test]
fn test_commit_then_new_transaction() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();

    // First transaction: committed
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1)").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // Second transaction: rolled back
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2)").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    // Only committed row should remain
    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(1));
}

// ==============================================================
// CASE WHEN
// ==============================================================

#[test]
fn test_case_when_searched() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, -5)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 0)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT CASE WHEN val > 0 THEN 'pos' WHEN val < 0 THEN 'neg' ELSE 'zero' END FROM t1 ORDER BY id",
    );
    assert_eq!(rows[0][0], Value::Text("pos".into()));
    assert_eq!(rows[1][0], Value::Text("neg".into()));
    assert_eq!(rows[2][0], Value::Text("zero".into()));
}

#[test]
fn test_case_when_simple() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, x INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 1)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 2)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 99)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT CASE x WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'other' END FROM t1 ORDER BY id",
    );
    assert_eq!(rows[0][0], Value::Text("one".into()));
    assert_eq!(rows[1][0], Value::Text("two".into()));
    assert_eq!(rows[2][0], Value::Text("other".into()));
}

#[test]
fn test_case_when_no_else_returns_null() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT CASE WHEN 1 = 2 THEN 'x' END");
    assert_eq!(rows[0][0], Value::Null);
}

#[test]
fn test_case_when_null_operand() {
    // NULL WHEN comparison: NULL never matches anything
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE NULL WHEN NULL THEN 'match' ELSE 'no' END",
    );
    assert_eq!(rows[0][0], Value::Text("no".into()));
}

#[test]
fn test_case_when_in_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 5)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 15)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 WHERE CASE WHEN val > 10 THEN 1 ELSE 0 END = 1",
    );
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_case_when_in_order_by() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 5)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (2, 15)").unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (3, 10)").unwrap();

    // Sort by category: 'big' < 'small' alphabetically
    let rows = query_rows(
        &mut vm,
        "SELECT id FROM t1 ORDER BY CASE WHEN val >= 10 THEN 'big' ELSE 'small' END, id",
    );
    // 'big': ids 2,3 come first alphabetically; 'small': id 1 comes after
    assert_eq!(rows[0][0], Value::Integer(2));
    assert_eq!(rows[1][0], Value::Integer(3));
    assert_eq!(rows[2][0], Value::Integer(1));
}

// ==============================================================
// INSERT INTO ... SELECT
// ==============================================================

#[test]
fn test_insert_select_all_columns() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("CREATE TABLE dst (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("INSERT INTO dst SELECT id, name FROM src")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM dst ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
    assert_eq!(rows[1][1], Value::Text("Bob".into()));
}

#[test]
fn test_insert_select_with_column_list_and_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, x INTEGER, y INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE dst (a INTEGER, b INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1, 10, 100)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2, 20, 200)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (3, 5, 50)").unwrap();

    vm.execute_sql("INSERT INTO dst (a, b) SELECT x, y FROM src WHERE x >= 10")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM dst ORDER BY a");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(10));
    assert_eq!(rows[1][0], Value::Integer(20));
}

#[test]
fn test_insert_select_zero_rows() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE dst (id INTEGER PRIMARY KEY)")
        .unwrap();

    match vm
        .execute_sql("INSERT INTO dst SELECT * FROM src WHERE id > 1000")
        .unwrap()
    {
        ExecResult::RowsAffected { count, .. } => assert_eq!(count, 0),
        _ => panic!("expected RowsAffected"),
    }
    let rows = query_rows(&mut vm, "SELECT * FROM dst");
    assert!(rows.is_empty());
}

// ==============================================================
// CREATE TABLE AS SELECT
// ==============================================================

#[test]
fn test_create_table_as_select_basic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2, 'Bob')").unwrap();

    vm.execute_sql("CREATE TABLE dst AS SELECT * FROM src")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM dst ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Text("Alice".into()));
}

#[test]
fn test_create_table_as_select_with_filter() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO src VALUES (3, 30)").unwrap();

    vm.execute_sql("CREATE TABLE dst AS SELECT id, val FROM src WHERE val >= 20")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM dst ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_create_table_as_select_empty_result() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();

    vm.execute_sql("CREATE TABLE dst AS SELECT * FROM src")
        .unwrap();

    // Table should exist with correct schema
    let rows = query_rows(&mut vm, "SELECT * FROM dst");
    assert!(rows.is_empty());
}

#[test]
fn test_create_table_as_select_if_not_exists() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1)").unwrap();
    vm.execute_sql("CREATE TABLE dst AS SELECT * FROM src")
        .unwrap();

    // IF NOT EXISTS should not fail even though dst already exists
    let result = vm.execute_sql("CREATE TABLE IF NOT EXISTS dst AS SELECT * FROM src");
    assert!(result.is_ok());
}

// ==============================================================
// Transaction Atomicity (rollback on partial failures)
// ==============================================================

#[test]
fn test_txn_atomicity_values_unique_conflict() {
    // Multi-row VALUES INSERT that fails midway 锟?table should be unchanged.
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY, name TEXT UNIQUE)")
        .unwrap();
    vm.execute_sql("INSERT INTO t1 VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("CREATE UNIQUE INDEX idx_name ON t1 (name)")
        .unwrap();

    // This batch has a duplicate name 'Alice' 锟?should fail atomically
    let result = vm.execute_sql("INSERT INTO t1 VALUES (2, 'Bob'), (3, 'Charlie'), (4, 'Alice')");
    assert!(result.is_err());

    // Table should only have the original row (not partial 2,3)
    let rows = query_rows(&mut vm, "SELECT id FROM t1 ORDER BY id");
    assert_eq!(rows.len(), 1, "partial rows must be rolled back");
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_txn_atomicity_insert_select_unique_conflict() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY, val INTEGER)")
        .unwrap();
    vm.execute_sql("CREATE TABLE dst (id INTEGER PRIMARY KEY)")
        .unwrap();
    // Pre-existing row in dst
    vm.execute_sql("INSERT INTO dst VALUES (2)").unwrap();
    // src has rows 1, 2, 3 锟?inserting into dst will conflict on id=2
    vm.execute_sql("INSERT INTO src VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO src VALUES (3, 30)").unwrap();

    let result = vm.execute_sql("INSERT INTO dst SELECT id FROM src ORDER BY id");
    assert!(result.is_err());

    // dst must still have only the original row id=2
    let rows = query_rows(&mut vm, "SELECT id FROM dst");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_txn_explicit_begin_insert_select_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t1 (id INTEGER PRIMARY KEY)")
        .unwrap();

    vm.execute_sql("CREATE TABLE src (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO src VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO src VALUES (2)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t1 SELECT id FROM src").unwrap();
    // Now rollback the explicit transaction
    vm.execute_sql("ROLLBACK").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t1");
    assert!(rows.is_empty(), "ROLLBACK must undo INSERT SELECT");
}

#[test]
fn test_ctas_rollback_on_failure() {
    let mut vm = VM::new_memory();
    // Source table does not exist 锟?CTAS should fail and not leave a partial table
    let result = vm.execute_sql("CREATE TABLE dst AS SELECT * FROM nonexistent_table");
    assert!(result.is_err());

    // dst table must not exist
    let result2 = vm.execute_sql("SELECT * FROM dst");
    assert!(result2.is_err(), "dst must not exist after failed CTAS");
}

// ==================== CAST ====================

#[test]
fn test_cast_to_integer() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT CAST('42' AS INTEGER), CAST(3.7 AS INTEGER), CAST(NULL AS INTEGER)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Integer(42));
        assert_eq!(rows[0][1], Value::Integer(3));
        assert_eq!(rows[0][2], Value::Null);
    }
}

#[test]
#[allow(clippy::approx_constant)]
fn test_cast_to_real() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT CAST('3.14' AS REAL), CAST(7 AS REAL)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        let v = match &rows[0][0] {
            Value::Real(f) => *f,
            _ => panic!("not real"),
        };
        assert!((v - 3.14).abs() < 1e-10);
        assert_eq!(rows[0][1], Value::Real(7.0));
    }
}

#[test]
fn test_cast_to_text() {
    let mut vm = VM::new_memory();
    let r = vm.execute_sql("SELECT CAST(42 AS TEXT)").unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Text("42".into()));
    }
}

// ==================== NULLIF ====================

#[test]
fn test_nullif() {
    let mut vm = VM::new_memory();
    let r = vm.execute_sql("SELECT NULLIF(1, 1), NULLIF(1, 2)").unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Null);
        assert_eq!(rows[0][1], Value::Integer(1));
    }
}

// ==================== Math functions ====================

#[test]
fn test_round() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT ROUND(3.456, 2), ROUND(3.5), ROUND(-2.7)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        let v = match &rows[0][0] {
            Value::Real(f) => *f,
            _ => panic!(),
        };
        assert!((v - 3.46).abs() < 1e-10);
        assert_eq!(rows[0][1], Value::Real(4.0));
        assert_eq!(rows[0][2], Value::Real(-3.0));
    }
}

#[test]
fn test_ceil_floor() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT CEIL(3.2), CEILING(3.9), FLOOR(3.9), FLOOR(-1.1)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Real(4.0));
        assert_eq!(rows[0][1], Value::Real(4.0));
        assert_eq!(rows[0][2], Value::Real(3.0));
        assert_eq!(rows[0][3], Value::Real(-2.0));
    }
}

// ==================== String functions ====================

#[test]
fn test_instr() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql(
            "SELECT INSTR('hello world', 'world'), INSTR('hello', 'xyz'), INSTR('abc', 'a')",
        )
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Integer(7));
        assert_eq!(rows[0][1], Value::Integer(0));
        assert_eq!(rows[0][2], Value::Integer(1));
    }
}

#[test]
fn test_ltrim_rtrim() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT LTRIM('  hello'), RTRIM('hello  ')")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Text("hello".into()));
        assert_eq!(rows[0][1], Value::Text("hello".into()));
    }
}

#[test]
fn test_hex_unicode_char() {
    let mut vm = VM::new_memory();
    let r = vm
        .execute_sql("SELECT HEX(255), UNICODE('A'), CHAR(65)")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows[0][0], Value::Text("FF".into()));
        assert_eq!(rows[0][1], Value::Integer(65));
        assert_eq!(rows[0][2], Value::Text("A".into()));
    }
}

// ==================== DROP INDEX ====================

#[test]
fn test_drop_index() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("CREATE INDEX idx_val ON t (val)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'hello')").unwrap();
    let r1 = vm
        .execute_sql("SELECT * FROM t WHERE val = 'hello'")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r1 {
        assert_eq!(rows.len(), 1);
    }
    vm.execute_sql("DROP INDEX idx_val").unwrap();
    let r2 = vm
        .execute_sql("SELECT * FROM t WHERE val = 'hello'")
        .unwrap();
    if let ExecResult::QueryResult { rows, .. } = r2 {
        assert_eq!(rows.len(), 1);
    }
    vm.execute_sql("DROP INDEX IF EXISTS idx_val").unwrap();
}

// ==================== INSERT OR REPLACE ====================

#[test]
fn test_insert_or_replace() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice')").unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO t VALUES (1, 'Bob')")
        .unwrap();
    let r = vm.execute_sql("SELECT name FROM t WHERE id = 1").unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Text("Bob".into()));
    }
}

// ==================== INSERT OR IGNORE ====================

#[test]
fn test_insert_or_ignore() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice')").unwrap();
    vm.execute_sql("INSERT OR IGNORE INTO t VALUES (1, 'Bob')")
        .unwrap();
    let r = vm.execute_sql("SELECT name FROM t WHERE id = 1").unwrap();
    if let ExecResult::QueryResult { rows, .. } = r {
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0][0], Value::Text("Alice".into()));
    }
    vm.execute_sql("INSERT OR IGNORE INTO t VALUES (2, 'Carol')")
        .unwrap();
    let r2 = vm.execute_sql("SELECT COUNT(*) FROM t").unwrap();
    if let ExecResult::QueryResult { rows, .. } = r2 {
        assert_eq!(rows[0][0], Value::Integer(2));
    }
}

// ── MVCC Undo Log & Snapshot Isolation Tests ────────────────────────────────

#[test]
fn test_mvcc_undo_log_records_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'b')").unwrap();

    // Undo log should have 2 insert entries
    assert_eq!(vm.mvcc_undo_log.len(), 2);
    let stats = vm.mvcc_undo_log.stats();
    assert_eq!(stats.inserts, 2);
    assert_eq!(stats.updates, 0);
    assert_eq!(stats.deletes, 0);

    vm.execute_sql("COMMIT").unwrap();
    // After commit, undo log is cleared
    assert!(vm.mvcc_undo_log.is_empty());
}

#[test]
fn test_mvcc_undo_log_records_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'old')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE t SET val = 'new' WHERE id = 1")
        .unwrap();

    assert_eq!(vm.mvcc_undo_log.len(), 1);
    let stats = vm.mvcc_undo_log.stats();
    assert_eq!(stats.updates, 1);

    vm.execute_sql("ROLLBACK").unwrap();
    assert!(vm.mvcc_undo_log.is_empty());
    // After rollback, original value restored
    let rows = query_rows(&mut vm, "SELECT val FROM t WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("old".into()));
}

#[test]
fn test_mvcc_undo_log_records_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'keep')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'remove')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 2").unwrap();

    // Should have 1 delete entry
    assert_eq!(vm.mvcc_undo_log.len(), 1);
    let stats = vm.mvcc_undo_log.stats();
    assert_eq!(stats.deletes, 1);

    vm.execute_sql("ROLLBACK").unwrap();
    // After rollback, the deleted row should be restored
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_mvcc_undo_log_savepoint_marker() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("SAVEPOINT sp1").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3)").unwrap();

    // Undo log: Insert(1) + Savepoint(sp1) + Insert(2) + Insert(3) = 4 entries
    assert_eq!(vm.mvcc_undo_log.len(), 4);
    let stats = vm.mvcc_undo_log.stats();
    assert_eq!(stats.inserts, 3);
    assert_eq!(stats.savepoints, 1);

    vm.execute_sql("COMMIT").unwrap();
}

#[test]
fn test_mvcc_txn_registry_begin_commit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();

    // Before BEGIN, no active transaction
    assert_eq!(vm.current_txn_id, 0);

    vm.execute_sql("BEGIN").unwrap();
    let txn_id = vm.current_txn_id;
    assert!(txn_id > 0);
    assert_eq!(vm.txn_registry.active_count(), 1);

    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    assert_eq!(vm.current_txn_id, 0);
    assert_eq!(vm.txn_registry.active_count(), 0);
}

#[test]
fn test_mvcc_txn_registry_begin_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    let txn_id = vm.current_txn_id;
    assert!(txn_id > 0);

    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    assert_eq!(vm.current_txn_id, 0);
    assert_eq!(vm.txn_registry.active_count(), 0);

    // Row should not exist
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM t");
    assert_eq!(rows[0][0], Value::Integer(0));
}

#[test]
fn test_mvcc_txn_id_monotonic() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    let txn1 = vm.current_txn_id;
    vm.execute_sql("COMMIT").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    let txn2 = vm.current_txn_id;
    vm.execute_sql("COMMIT").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    let txn3 = vm.current_txn_id;
    vm.execute_sql("ROLLBACK").unwrap();

    assert!(txn1 < txn2);
    assert!(txn2 < txn3);
}

#[test]
fn test_mvcc_undo_entry_has_txn_id() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'x')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    let txn_id = vm.current_txn_id;
    vm.execute_sql("INSERT INTO t VALUES (2, 'y')").unwrap();
    vm.execute_sql("UPDATE t SET val = 'z' WHERE id = 1").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 2").unwrap();

    // All entries should have the current transaction ID
    for entry in vm.mvcc_undo_log.iter() {
        assert_eq!(entry.txn_id(), txn_id);
    }

    vm.execute_sql("ROLLBACK").unwrap();
}

#[test]
fn test_mvcc_snapshot_visibility_check() {
    use crate::vm::mvcc::MvccSnapshot;

    // Simulate a scenario: txn 1 committed, txn 2 active, reader is txn 3
    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![2],
        max_committed_txn_id: 1,
    };

    // txn 1 committed → visible
    assert!(snap.is_visible(1));
    // txn 2 still active → invisible
    assert!(!snap.is_visible(2));
    // txn 3 is our own → visible
    assert!(snap.is_visible(3));
    // txn 4 created after snapshot → invisible
    assert!(!snap.is_visible(4));
}

#[test]
fn test_mvcc_mixed_dml_undo_stats() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 30)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (4, 40)").unwrap();
    vm.execute_sql("UPDATE t SET v = 11 WHERE id = 1").unwrap();
    vm.execute_sql("UPDATE t SET v = 22 WHERE id = 2").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 3").unwrap();

    let stats = vm.mvcc_undo_log.stats();
    assert_eq!(stats.inserts, 1);
    assert_eq!(stats.updates, 2);
    assert_eq!(stats.deletes, 1);
    assert_eq!(stats.total_entries, 4);
    assert!(stats.size_bytes > 0);

    vm.execute_sql("COMMIT").unwrap();
}

#[test]
fn test_mvcc_registry_purge_on_commit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)").unwrap();

    // First transaction
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // After commit, undo log should be cleared (purged)
    assert!(vm.mvcc_undo_log.is_empty());

    // Next txn ID should be higher
    assert!(vm.txn_registry.next_id() > 1);
}

#[test]
fn test_mvcc_clustered_index_flag() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
        .unwrap();

    let table = vm.schema.get_table("t").unwrap();
    assert!(table.clustered_index);
    assert!(table.pk_is_integer_clustered());
    assert_eq!(table.primary_key_column(), Some("id"));
    assert_eq!(table.primary_key_col_index(), Some(0));
}

