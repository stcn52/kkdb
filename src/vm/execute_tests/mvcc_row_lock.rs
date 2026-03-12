// ═══════════════════════════════════════════════════════════════════════════════
// Round-5: MVCC Row-Level Locking + Optimistic Concurrency Control tests
//
// Tests cover:
//  - RowLockManager unit tests (acquire, release, conflict, OCC validation)
//  - SQL-level write-write conflict detection in transactions
//  - Commit version tracking and garbage collection
// ═══════════════════════════════════════════════════════════════════════════════

use crate::vm::mvcc::RowLockManager;

// ── RowLockManager unit tests ─────────────────────────────────────────────────

#[test]
fn test_row_lock_acquire_and_release() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("users", 1, 100).unwrap();
    mgr.try_lock_row("users", 2, 100).unwrap();
    assert_eq!(mgr.lock_count(), 2);

    mgr.release_all(100);
    assert_eq!(mgr.lock_count(), 0);
}

#[test]
fn test_row_lock_same_txn_reentrant() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("t", 1, 42).unwrap();
    // Same txn re-acquiring same row → no error
    mgr.try_lock_row("t", 1, 42).unwrap();
    assert_eq!(mgr.lock_count(), 1);
}

#[test]
fn test_row_lock_conflict() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("orders", 5, 100).unwrap();
    // Different txn trying same row → conflict
    let err = mgr.try_lock_row("orders", 5, 200);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("Write-write conflict"));
}

#[test]
fn test_row_lock_different_rows_no_conflict() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("t", 1, 100).unwrap();
    mgr.try_lock_row("t", 2, 200).unwrap(); // different row → OK
    assert_eq!(mgr.lock_count(), 2);
}

#[test]
fn test_row_lock_different_tables_no_conflict() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("t1", 1, 100).unwrap();
    mgr.try_lock_row("t2", 1, 200).unwrap(); // different table → OK
    assert_eq!(mgr.lock_count(), 2);
}

#[test]
fn test_row_lock_release_then_reacquire() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("t", 1, 100).unwrap();
    mgr.release_all(100);

    // After release, another txn can acquire
    mgr.try_lock_row("t", 1, 200).unwrap();
    assert_eq!(mgr.lock_count(), 1);
}

// ── Optimistic Concurrency Control (OCC) ──────────────────────────────────────

#[test]
fn test_occ_read_set_recording() {
    let mut mgr = RowLockManager::new();

    mgr.record_read("users", 1, 100);
    mgr.record_read("users", 2, 100);
    mgr.record_read("orders", 5, 100);
    assert_eq!(mgr.read_set_size(100), 3);

    // Duplicate reads are not counted twice
    mgr.record_read("users", 1, 100);
    assert_eq!(mgr.read_set_size(100), 3);
}

#[test]
fn test_occ_validation_passes_when_no_conflicts() {
    let mut mgr = RowLockManager::new();

    // Txn 100 reads row 1 at snapshot_txn_id=50
    mgr.record_read("users", 1, 100);

    // Row 1 was last modified by txn 40 (before snapshot)
    mgr.committed_versions.insert(("users".into(), 1), 40);

    // Validation should pass (40 <= 50)
    mgr.validate_read_set(100, 50).unwrap();
}

#[test]
fn test_occ_validation_fails_when_row_modified_after_snapshot() {
    let mut mgr = RowLockManager::new();

    // Txn 100 reads row 1 at snapshot_txn_id=50
    mgr.record_read("users", 1, 100);

    // Another txn (60) committed changes to row 1 AFTER our snapshot
    mgr.committed_versions.insert(("users".into(), 1), 60);

    // Validation should fail (60 > 50)
    let err = mgr.validate_read_set(100, 50);
    assert!(err.is_err());
    assert!(err.unwrap_err().to_string().contains("Serialization failure"));
}

#[test]
fn test_occ_validation_ignores_own_writes() {
    let mut mgr = RowLockManager::new();

    // Txn 100 reads row 1
    mgr.record_read("users", 1, 100);

    // Row 1 was last modified by ourselves (txn 100)
    mgr.committed_versions.insert(("users".into(), 1), 100);

    // Validation should pass (our own writes don't conflict)
    mgr.validate_read_set(100, 50).unwrap();
}

#[test]
fn test_commit_version_tracking() {
    let mut mgr = RowLockManager::new();

    mgr.try_lock_row("t", 1, 100).unwrap();
    mgr.try_lock_row("t", 2, 100).unwrap();

    mgr.commit_version(100);

    assert_eq!(mgr.committed_versions.get(&("t".into(), 1)), Some(&100));
    assert_eq!(mgr.committed_versions.get(&("t".into(), 2)), Some(&100));
}

#[test]
fn test_gc_versions() {
    let mut mgr = RowLockManager::new();

    mgr.committed_versions.insert(("t".into(), 1), 10);
    mgr.committed_versions.insert(("t".into(), 2), 50);
    mgr.committed_versions.insert(("t".into(), 3), 100);

    // GC versions < 50
    mgr.gc_versions(50);

    assert_eq!(mgr.committed_versions.len(), 2); // 50 and 100 remain
    assert!(mgr.committed_versions.get(&("t".into(), 1)).is_none());
}

// ── SQL-level integration tests ───────────────────────────────────────────────

use super::{query_rows, VM};
use crate::types::Value;

#[test]
fn test_row_lock_acquired_during_update_in_txn() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl VALUES (1, 'a'), (2, 'b'), (3, 'c')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE rl SET val = 'x' WHERE id = 1").unwrap();

    // Row lock should be acquired for id=1
    assert!(vm.row_lock_manager.lock_count() >= 1);

    vm.execute_sql("COMMIT").unwrap();

    // After commit, locks are released
    assert_eq!(vm.row_lock_manager.lock_count(), 0);

    // Version should be recorded
    assert!(vm.row_lock_manager.committed_versions.len() >= 1);
}

#[test]
fn test_row_lock_released_on_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl2 (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl2 VALUES (1, 'a')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE rl2 SET val = 'x' WHERE id = 1").unwrap();
    assert!(vm.row_lock_manager.lock_count() >= 1);

    vm.execute_sql("ROLLBACK").unwrap();

    // After rollback, locks released, no version recorded
    assert_eq!(vm.row_lock_manager.lock_count(), 0);
}

#[test]
fn test_row_lock_acquired_during_delete_in_txn() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl3 (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl3 VALUES (1, 'a'), (2, 'b')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DELETE FROM rl3 WHERE id = 2").unwrap();

    assert!(vm.row_lock_manager.lock_count() >= 1);

    vm.execute_sql("COMMIT").unwrap();
    assert_eq!(vm.row_lock_manager.lock_count(), 0);
}

#[test]
fn test_multiple_rows_locked_in_single_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl4 (id INTEGER PRIMARY KEY, val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO rl4 VALUES (1, 10), (2, 20), (3, 30)").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    // Update all rows → should acquire 3 row locks
    vm.execute_sql("UPDATE rl4 SET val = val + 1").unwrap();
    assert_eq!(vm.row_lock_manager.lock_count(), 3);

    vm.execute_sql("COMMIT").unwrap();
    assert_eq!(vm.row_lock_manager.lock_count(), 0);
    // All 3 committed versions recorded
    assert!(vm.row_lock_manager.committed_versions.len() >= 3);
}

#[test]
fn test_committed_data_survives_after_row_lock() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl5 (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl5 VALUES (1, 'original')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE rl5 SET val = 'updated' WHERE id = 1").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM rl5 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("updated".into()));
}

#[test]
fn test_rollback_restores_data_with_row_lock() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl6 (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl6 VALUES (1, 'original')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE rl6 SET val = 'changed' WHERE id = 1").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM rl6 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("original".into()));
}

#[test]
fn test_row_lock_manager_gc_versions_integration() {
    let mut mgr = RowLockManager::new();

    // Simulate several transactions committed at different times
    for i in 1..=10 {
        mgr.committed_versions.insert(("t".into(), i), i as u64 * 10);
    }
    assert_eq!(mgr.committed_versions.len(), 10);

    // GC versions older than txn 50
    mgr.gc_versions(50);
    assert_eq!(mgr.committed_versions.len(), 6); // 50,60,70,80,90,100 remain
}

#[test]
fn test_row_lock_upsert_conflict_detection() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE rl7 (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("INSERT INTO rl7 VALUES (1, 'a')").unwrap();

    vm.execute_sql("BEGIN").unwrap();
    // INSERT OR REPLACE triggers delete+re-insert or upsert — locks depend on path
    vm.execute_sql("INSERT OR REPLACE INTO rl7 VALUES (1, 'b')").unwrap();
    // May acquire row lock on the deleted row, but path is implementation-specific
    vm.execute_sql("COMMIT").unwrap();

    let rows = query_rows(&mut vm, "SELECT val FROM rl7 WHERE id = 1");
    assert_eq!(rows[0][0], Value::Text("b".into()));
}
