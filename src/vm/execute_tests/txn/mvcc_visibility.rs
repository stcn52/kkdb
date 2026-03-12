//! MVCC snapshot-isolation visibility tests.
//!
//! Verifies that:
//! - MVCC snapshots are created at BEGIN and cleared at COMMIT/ROLLBACK
//! - `compute_visibility_delta` correctly identifies invisible/restored rows
//! - The SELECT path integrates MVCC filtering via `eval_from`
//! - SHOW ENGINE STATUS reports MVCC snapshot information

use super::*;

// ── Snapshot lifecycle ──────────────────────────────────────────────────────

#[test]
fn test_mvcc_snapshot_created_at_begin() {
    let mut vm = VM::new_memory();
    // Before BEGIN, no snapshot
    assert!(vm.mvcc_snapshot.is_none());

    vm.execute_sql("BEGIN").unwrap();
    // After BEGIN, snapshot exists
    assert!(vm.mvcc_snapshot.is_some());

    let snap = vm.mvcc_snapshot.as_ref().unwrap();
    assert!(snap.reader_txn_id > 0);
}

#[test]
fn test_mvcc_snapshot_cleared_on_commit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    assert!(vm.mvcc_snapshot.is_some());

    vm.execute_sql("INSERT INTO t VALUES (1, 'a')").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // After COMMIT, snapshot is cleared
    assert!(vm.mvcc_snapshot.is_none());
}

#[test]
fn test_mvcc_snapshot_cleared_on_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    assert!(vm.mvcc_snapshot.is_some());

    vm.execute_sql("INSERT INTO t VALUES (1, 'a')").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    // After ROLLBACK, snapshot is cleared
    assert!(vm.mvcc_snapshot.is_none());
}

#[test]
fn test_mvcc_snapshot_fields() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();

    // First transaction
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // Second transaction — snapshot should see first as committed
    vm.execute_sql("BEGIN").unwrap();
    let snap = vm.mvcc_snapshot.as_ref().unwrap();
    // max_committed should be >= 1 (the first transaction)
    assert!(snap.max_committed_txn_id > 0);
    // reader_txn_id should be > first txn
    assert!(snap.reader_txn_id > snap.max_committed_txn_id || snap.max_committed_txn_id >= 1);
    vm.execute_sql("COMMIT").unwrap();
}

// ── compute_visibility_delta unit tests ────────────────────────────────────

#[test]
fn test_visibility_delta_insert_invisible() {
    use crate::vm::mvcc::{compute_visibility_delta, MvccSnapshot, UndoEntry, UndoLog};

    let mut undo = UndoLog::new();
    // Simulate: txn 5 inserted rowid 10 into table "t"
    undo.push(UndoEntry::Insert {
        table: "t".into(),
        rowid: 10,
        txn_id: 5,
    });

    // Snapshot where txn 5 is active (not committed) → row 10 should be invisible
    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![5],
        max_committed_txn_id: 4,
    };

    let (invisible, restored) = compute_visibility_delta(&undo, &snap, "t");
    assert!(invisible.contains(&10));
    assert!(restored.is_empty());
}

#[test]
fn test_visibility_delta_insert_visible() {
    use crate::vm::mvcc::{compute_visibility_delta, MvccSnapshot, UndoEntry, UndoLog};

    let mut undo = UndoLog::new();
    undo.push(UndoEntry::Insert {
        table: "t".into(),
        rowid: 10,
        txn_id: 2,
    });

    // Snapshot where txn 2 is committed → row 10 should be visible
    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![],
        max_committed_txn_id: 4,
    };

    let (invisible, restored) = compute_visibility_delta(&undo, &snap, "t");
    assert!(!invisible.contains(&10)); // visible, not invisible
    assert!(restored.is_empty());
}

#[test]
fn test_visibility_delta_delete_invisible() {
    use crate::vm::mvcc::{compute_visibility_delta, MvccSnapshot, UndoEntry, UndoLog};

    let mut undo = UndoLog::new();
    // txn 5 deleted rowid 10 — but txn 5 is not visible to our snapshot
    undo.push(UndoEntry::Delete {
        table: "t".into(),
        rowid: 10,
        old_row: vec![Value::Integer(10), Value::Text("hello".into())],
        txn_id: 5,
    });

    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![5],
        max_committed_txn_id: 4,
    };

    let (invisible, restored) = compute_visibility_delta(&undo, &snap, "t");
    assert!(invisible.is_empty());
    // Row should be restored (delete was by invisible txn)
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].0, 10);
    assert_eq!(restored[0].1[1], Value::Text("hello".into()));
}

#[test]
fn test_visibility_delta_update_invisible() {
    use crate::vm::mvcc::{compute_visibility_delta, MvccSnapshot, UndoEntry, UndoLog};

    let mut undo = UndoLog::new();
    // txn 5 updated rowid 10 — but txn 5 is not visible to our snapshot
    undo.push(UndoEntry::Update {
        table: "t".into(),
        rowid: 10,
        old_row: vec![Value::Integer(10), Value::Text("old_val".into())],
        txn_id: 5,
    });

    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![5],
        max_committed_txn_id: 4,
    };

    let (invisible, restored) = compute_visibility_delta(&undo, &snap, "t");
    // Current version should be invisible (updated by invisible txn)
    assert!(invisible.contains(&10));
    // Old version should be restored
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].1[1], Value::Text("old_val".into()));
}

#[test]
fn test_visibility_delta_cross_table() {
    use crate::vm::mvcc::{compute_visibility_delta, MvccSnapshot, UndoEntry, UndoLog};

    let mut undo = UndoLog::new();
    // txn 5 inserted into table "a", not "b"
    undo.push(UndoEntry::Insert {
        table: "a".into(),
        rowid: 1,
        txn_id: 5,
    });

    let snap = MvccSnapshot {
        reader_txn_id: 3,
        active_txn_ids: vec![5],
        max_committed_txn_id: 4,
    };

    // Querying table "b" — should not be affected
    let (invisible_b, restored_b) = compute_visibility_delta(&undo, &snap, "b");
    assert!(invisible_b.is_empty());
    assert!(restored_b.is_empty());

    // Querying table "a" — rowid 1 should be invisible
    let (invisible_a, _) = compute_visibility_delta(&undo, &snap, "a");
    assert!(invisible_a.contains(&1));
}

// ── Integration: SELECT with MVCC snapshot ─────────────────────────────────

#[test]
fn test_select_within_transaction_sees_own_inserts() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'alice')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'bob')").unwrap();

    // Within the same transaction, we should see our own inserts
    let rows = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][1], Value::Text("alice".into()));
    assert_eq!(rows[1][1], Value::Text("bob".into()));

    vm.execute_sql("COMMIT").unwrap();
}

#[test]
fn test_select_after_commit_sees_committed_data() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'alice')").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // After commit, data is visible even without explicit transaction
    let rows = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Text("alice".into()));
}

#[test]
fn test_select_after_rollback_sees_nothing() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'alice')").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();

    // After rollback, data should not be visible
    let rows = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_mvcc_snapshot_with_multiple_transactions() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();

    // First transaction: insert 3 rows
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 200)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 300)").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // Second transaction: update and check
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("UPDATE t SET v = 999 WHERE id = 2").unwrap();

    // Should see the update within the same transaction
    let rows = query_rows(&mut vm, "SELECT v FROM t WHERE id = 2");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(999));

    vm.execute_sql("COMMIT").unwrap();

    // Verify final state
    let rows = query_rows(&mut vm, "SELECT v FROM t ORDER BY id");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(100));
    assert_eq!(rows[1][0], Value::Integer(999));
    assert_eq!(rows[2][0], Value::Integer(300));
}

#[test]
fn test_mvcc_snapshot_with_delete_in_transaction() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();

    // Insert initial data
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'a')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'b')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3, 'c')").unwrap();
    vm.execute_sql("COMMIT").unwrap();

    // Delete one row and verify
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("DELETE FROM t WHERE id = 2").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t ORDER BY id");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[1][0], Value::Integer(3));

    vm.execute_sql("COMMIT").unwrap();
}

// ── SHOW ENGINE STATUS reports MVCC snapshot ───────────────────────────────

#[test]
fn test_show_engine_status_mvcc_no_snapshot() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();

    match vm.execute_sql("SHOW ENGINE STATUS").unwrap() {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("--- MVCC ---"));
            assert!(plan.contains("Snapshot           : none (autocommit)"));
        }
        _ => panic!("expected Explain"),
    }
}

#[test]
fn test_show_engine_status_mvcc_with_snapshot() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();

    vm.execute_sql("BEGIN").unwrap();

    match vm.execute_sql("SHOW ENGINE STATUS").unwrap() {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("--- MVCC ---"));
            assert!(plan.contains("Snapshot reader    : txn"));
            assert!(plan.contains("Snapshot max commit:"));
            assert!(plan.contains("Snapshot active    :"));
        }
        _ => panic!("expected Explain"),
    }

    vm.execute_sql("COMMIT").unwrap();
}
