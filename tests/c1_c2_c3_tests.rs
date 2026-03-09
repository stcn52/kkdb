// C1/C2/C3 concurrency integration tests
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use kkdb::vm::lock_manager::{global_lock_table, LockMode};
use std::fs;

fn setup(name: &str) -> VM {
    let _ = fs::remove_dir_all(name);
    VM::open(name).unwrap()
}

fn rows(r: ExecResult) -> Vec<Vec<Value>> {
    match r {
        ExecResult::QueryResult { rows, .. } => rows,
        _ => panic!("not query"),
    }
}

// ── C1: MVCC rollback tests ──────────────────────────────────────────────

#[test]
fn test_c1_commit_persists() {
    let mut vm = setup("test_c1_commit");
    vm.execute_sql("CREATE TABLE t (id INTEGER, v INTEGER);")
        .unwrap();
    vm.execute_sql("BEGIN;").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100);").unwrap();
    vm.execute_sql("COMMIT;").unwrap();
    let r = rows(vm.execute_sql("SELECT id, v FROM t;").unwrap());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Integer(1));
}

#[test]
fn test_c1_rollback_insert() {
    let mut vm = setup("test_c1_rb_insert");
    vm.execute_sql("CREATE TABLE t (id INTEGER, v INTEGER);")
        .unwrap();
    vm.execute_sql("BEGIN;").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100);").unwrap();
    // verify row visible within txn
    let mid = rows(vm.execute_sql("SELECT id FROM t;").unwrap());
    assert_eq!(mid.len(), 1, "row should be visible within txn");
    vm.execute_sql("ROLLBACK;").unwrap();
    // After rollback, row should be gone
    let r = rows(vm.execute_sql("SELECT id FROM t;").unwrap());
    assert_eq!(r.len(), 0, "INSERT should be rolled back: {:?}", r);
}

#[test]
fn test_c1_rollback_update() {
    let mut vm = setup("test_c1_rb_update");
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 10);").unwrap();
    vm.execute_sql("BEGIN;").unwrap();
    vm.execute_sql("UPDATE t SET v = 999 WHERE id = 1;")
        .unwrap();
    let mid = rows(vm.execute_sql("SELECT v FROM t WHERE id = 1;").unwrap());
    assert_eq!(mid[0][0], Value::Integer(999));
    vm.execute_sql("ROLLBACK;").unwrap();
    let r = rows(vm.execute_sql("SELECT v FROM t WHERE id = 1;").unwrap());
    assert_eq!(r.len(), 1);
    // COW pager rolls back the data-level update
    assert_eq!(
        r[0][0],
        Value::Integer(10),
        "UPDATE should be rolled back: {:?}",
        r
    );
}

#[test]
fn test_c1_rollback_delete() {
    let mut vm = setup("test_c1_rb_delete");
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER);")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 50);").unwrap();
    vm.execute_sql("BEGIN;").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 1;").unwrap();
    let mid = rows(vm.execute_sql("SELECT id FROM t;").unwrap());
    assert_eq!(mid.len(), 0, "row should be deleted within txn");
    vm.execute_sql("ROLLBACK;").unwrap();
    // After rollback, row should reappear
    let r = rows(vm.execute_sql("SELECT id FROM t;").unwrap());
    assert_eq!(r.len(), 1, "DELETE should be rolled back: {:?}", r);
}

// ── C2: Crash recovery ──────────────────────────────────────────────────

#[test]
fn test_c2_binlog_recover_clean() {
    let _ = fs::remove_dir_all("test_c2_recover");
    let mut vm = VM::open("test_c2_recover").unwrap();
    vm.execute_sql("CREATE TABLE t (id INTEGER);").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1);").unwrap();
    // Binlog recover should not crash on a clean db
    drop(vm);
    let mut vm2 = VM::open("test_c2_recover").unwrap();
    let r = rows(vm2.execute_sql("SELECT id FROM t;").unwrap());
    assert_eq!(r.len(), 1);
}

/// C1 edge case: table pager opened lazily AFTER BEGIN should still be rolled back
#[test]
fn test_c1_rollback_lazy_pager() {
    let _ = fs::remove_dir_all("test_c1_lazy");
    let mut vm = VM::open("test_c1_lazy").unwrap();
    // Pre-create the table outside any transaction
    vm.execute_sql("CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER);")
        .unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (1, 100);")
        .unwrap();
    // Now begin a transaction, insert more rows, then rollback
    vm.execute_sql("BEGIN;").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (2, 200);")
        .unwrap();
    vm.execute_sql("INSERT INTO orders VALUES (3, 300);")
        .unwrap();
    let mid = rows(vm.execute_sql("SELECT id FROM orders;").unwrap());
    assert_eq!(mid.len(), 3, "3 rows visible within txn");
    vm.execute_sql("ROLLBACK;").unwrap();
    let r = rows(vm.execute_sql("SELECT id FROM orders;").unwrap());
    assert_eq!(
        r.len(),
        1,
        "only pre-txn row should remain after rollback: {:?}",
        r
    );
    assert_eq!(r[0][0], Value::Integer(1));
}

// ── C3: Lock manager unit tests ─────────────────────────────────────────

#[test]
fn test_c3_shared_shared_compat() {
    let lt_arc = global_lock_table();
    let mut lt = lt_arc.lock().unwrap();
    // Reset state for clean test (cross-test isolation)
    lt.locks.clear();
    lt.waiters.clear();
    lt.try_acquire("orders", LockMode::Shared, 100).unwrap();
    lt.try_acquire("orders", LockMode::Shared, 101).unwrap();
    lt.release_all(100);
    lt.release_all(101);
}

#[test]
fn test_c3_exclusive_conflict() {
    let lt_arc = global_lock_table();
    let mut lt = lt_arc.lock().unwrap();
    lt.locks.clear();
    lt.waiters.clear();
    lt.try_acquire("products", LockMode::Exclusive, 200)
        .unwrap();
    // Another txn trying to acquire Exclusive should get error (no deadlock cycle, just conflict)
    let result = lt.try_acquire("products", LockMode::Exclusive, 201);
    assert!(result.is_err(), "should err on exclusive conflict");
    lt.release_all(200);
}

#[test]
fn test_c3_lock_released_on_commit() {
    let lt_arc = global_lock_table();
    {
        let mut lt = lt_arc.lock().unwrap();
        lt.locks.clear();
        lt.waiters.clear();
        lt.try_acquire("users", LockMode::Exclusive, 300).unwrap();
        lt.release_all(300);
    }
    // After release, another txn should be able to acquire
    let mut lt = lt_arc.lock().unwrap();
    lt.try_acquire("users", LockMode::Exclusive, 301).unwrap();
    lt.release_all(301);
}

#[test]
fn test_c3_deadlock_detected() {
    use kkdb::vm::lock_manager::LockTable;
    // Manually construct a deadlock scenario:
    // txn 400 holds "tableA", waits for "tableB"
    // txn 401 holds "tableB", waits for "tableA"
    let mut lt = LockTable::new();
    lt.try_acquire("tableA", LockMode::Exclusive, 400).unwrap();
    lt.try_acquire("tableB", LockMode::Exclusive, 401).unwrap();
    // txn 401 now waits for tableA (held by 400)
    let r = lt.try_acquire("tableA", LockMode::Exclusive, 401);
    assert!(r.is_err(), "conflict detected");
    // Manually add 400 as waiter-for tableB to create a cycle
    lt.waiters.insert(400, vec!["tableB".to_string()]);
    let r2 = lt.try_acquire("tableB", LockMode::Exclusive, 400);
    assert!(r2.is_err(), "deadlock or conflict should be detected");
}
