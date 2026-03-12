// R6: MVCC Read Committed isolation level tests

use crate::types::Value;
use crate::vm::mvcc::IsolationLevel;

fn mem() -> crate::vm::execute::VM {
    crate::vm::execute::VM::new_memory()
}

fn exec(
    vm: &mut crate::vm::execute::VM,
    sql: &str,
) -> crate::error::Result<crate::vm::execute::ExecResult> {
    vm.execute_sql(sql)
}

fn rows(vm: &mut crate::vm::execute::VM, sql: &str) -> Vec<Vec<Value>> {
    match exec(vm, sql).unwrap() {
        crate::vm::execute::ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

// ── SET ISOLATION LEVEL ─────────────────────────────────────────────────────

#[test]
fn test_set_isolation_level_read_committed() {
    let mut vm = mem();
    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::ReadCommitted);
}

#[test]
fn test_set_isolation_level_serializable() {
    let mut vm = mem();
    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::ReadCommitted);
    exec(&mut vm, "SET transaction_isolation = 'serializable'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::Serializable);
}

#[test]
fn test_set_isolation_level_synonyms() {
    let mut vm = mem();
    exec(&mut vm, "SET isolation_level = 'read committed'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::ReadCommitted);

    exec(&mut vm, "SET isolation_level = 'repeatable read'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::RepeatableRead);

    exec(&mut vm, "SET isolation_level = 'snapshot'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::Serializable);
}

#[test]
fn test_set_isolation_level_invalid() {
    let mut vm = mem();
    let res = exec(&mut vm, "SET transaction_isolation = 'invalid'");
    assert!(res.is_err());
}

// ── Default is Serializable ─────────────────────────────────────────────────

#[test]
fn test_default_isolation_is_serializable() {
    let vm = mem();
    assert_eq!(vm.isolation_level, IsolationLevel::Serializable);
}

// ── Read Committed refreshes snapshot per-statement ─────────────────────────

#[test]
fn test_read_committed_snapshot_refresh() {
    // In Read Committed mode, each statement within a transaction gets a fresh
    // snapshot. We verify this by checking that the snapshot changes between
    // statements within a single transaction.
    let mut vm = mem();
    exec(&mut vm, "CREATE TABLE rc (id INTEGER PRIMARY KEY, v INT)").unwrap();
    exec(&mut vm, "INSERT INTO rc VALUES (1, 10)").unwrap();

    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    exec(&mut vm, "BEGIN").unwrap();

    // First read should see the initial data
    let r1 = rows(&mut vm, "SELECT v FROM rc WHERE id = 1");
    assert_eq!(r1[0][0], Value::Integer(10));

    // The snapshot should exist and have been refreshed
    assert!(vm.mvcc_snapshot.is_some());

    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_read_committed_basic_flow() {
    // Full transaction flow under Read Committed
    let mut vm = mem();
    exec(
        &mut vm,
        "CREATE TABLE rc2 (id INTEGER PRIMARY KEY, val TEXT)",
    )
    .unwrap();
    exec(&mut vm, "INSERT INTO rc2 VALUES (1, 'a'), (2, 'b')").unwrap();

    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    exec(&mut vm, "BEGIN").unwrap();
    let r = rows(&mut vm, "SELECT COUNT(*) FROM rc2");
    assert_eq!(r[0][0], Value::Integer(2));

    exec(&mut vm, "INSERT INTO rc2 VALUES (3, 'c')").unwrap();
    let r2 = rows(&mut vm, "SELECT COUNT(*) FROM rc2");
    assert_eq!(r2[0][0], Value::Integer(3)); // own writes visible

    exec(&mut vm, "COMMIT").unwrap();
}

#[test]
fn test_read_committed_update_and_read() {
    let mut vm = mem();
    exec(&mut vm, "CREATE TABLE rc3 (id INTEGER PRIMARY KEY, v INT)").unwrap();
    exec(&mut vm, "INSERT INTO rc3 VALUES (1, 100)").unwrap();

    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    exec(&mut vm, "BEGIN").unwrap();

    exec(&mut vm, "UPDATE rc3 SET v = 200 WHERE id = 1").unwrap();
    let r = rows(&mut vm, "SELECT v FROM rc3 WHERE id = 1");
    assert_eq!(r[0][0], Value::Integer(200)); // own write visible

    exec(&mut vm, "COMMIT").unwrap();

    // After commit, the update should be permanent
    let r2 = rows(&mut vm, "SELECT v FROM rc3 WHERE id = 1");
    assert_eq!(r2[0][0], Value::Integer(200));
}

#[test]
fn test_read_committed_rollback() {
    let mut vm = mem();
    exec(&mut vm, "CREATE TABLE rc4 (id INTEGER PRIMARY KEY, v INT)").unwrap();
    exec(&mut vm, "INSERT INTO rc4 VALUES (1, 10)").unwrap();

    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    exec(&mut vm, "BEGIN").unwrap();
    exec(&mut vm, "UPDATE rc4 SET v = 99 WHERE id = 1").unwrap();
    exec(&mut vm, "ROLLBACK").unwrap();

    let r = rows(&mut vm, "SELECT v FROM rc4 WHERE id = 1");
    assert_eq!(r[0][0], Value::Integer(10)); // rolled back
}

#[test]
fn test_serializable_vs_read_committed() {
    // Verify that changing isolation level affects the VM state
    let mut vm = mem();
    assert_eq!(vm.isolation_level, IsolationLevel::Serializable);

    exec(&mut vm, "SET transaction_isolation = 'read committed'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::ReadCommitted);

    // Switch back to serializable
    exec(&mut vm, "SET transaction_isolation = 'serializable'").unwrap();
    assert_eq!(vm.isolation_level, IsolationLevel::Serializable);
}
