// ═══════════════════════════════════════════════════════════════════════════════
// Round-4 coverage-boost tests
//
// Targets:
//   - lock_manager.rs  (18.4% → ~95%)
//   - connection_pool.rs  uncovered paths
//   - vector/mod.rs  uncovered methods
//   - query_cache + connection_pool edge cases
// ═══════════════════════════════════════════════════════════════════════════════

use super::VM;
use super::query_rows;
// ═══════════════════════════════════════════════════════════════════════════════
//  1. LockTable unit tests  (lock_manager.rs)
// ═══════════════════════════════════════════════════════════════════════════════

mod lock_manager_tests {
    use crate::vm::lock_manager::{LockMode, LockTable, global_lock_table};

    #[test]
    fn test_lock_reentrant_shared() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Shared, 1).unwrap();
        // Same txn re-acquiring Shared should succeed (no-op)
        lt.try_acquire("t1", LockMode::Shared, 1).unwrap();
        assert_eq!(lt.locks["t1"].len(), 1);
    }

    #[test]
    fn test_lock_upgrade_shared_to_exclusive() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Shared, 1).unwrap();
        // Same txn upgrading to Exclusive
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        assert_eq!(lt.locks["t1"][0].mode, LockMode::Exclusive);
    }

    #[test]
    fn test_multiple_shared_compatible() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Shared, 1).unwrap();
        lt.try_acquire("t1", LockMode::Shared, 2).unwrap();
        assert_eq!(lt.locks["t1"].len(), 2);
    }

    #[test]
    fn test_exclusive_blocks_shared() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        let err = lt.try_acquire("t1", LockMode::Shared, 2);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("Lock conflict"));
    }

    #[test]
    fn test_exclusive_blocks_exclusive() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        let err = lt.try_acquire("t1", LockMode::Exclusive, 2);
        assert!(err.is_err());
    }

    #[test]
    fn test_shared_blocks_exclusive_from_another() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Shared, 1).unwrap();
        let err = lt.try_acquire("t1", LockMode::Exclusive, 2);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("Lock conflict"));
    }

    #[test]
    fn test_release_all() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        lt.try_acquire("t2", LockMode::Shared, 1).unwrap();
        lt.release_all(1);
        assert!(lt.locks.is_empty());
        // After release, other txn can acquire
        lt.try_acquire("t1", LockMode::Exclusive, 2).unwrap();
    }

    #[test]
    fn test_deadlock_detection_true_cycle() {
        let mut lt = LockTable::new();
        // txn 1 holds t1, txn 2 holds t2
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        lt.try_acquire("t2", LockMode::Exclusive, 2).unwrap();
        // txn 1 waits for t2 → conflict (no deadlock yet, just conflict)
        let err1 = lt.try_acquire("t2", LockMode::Exclusive, 1);
        assert!(err1.is_err());
        // Now txn 2 waits for t1 → deadlock: 2 → t1 (held by 1), 1 was waiting for t2 (held by 2)
        // But since try_acquire cleans up waiters after check, we need to set up the waiter manually
        // to validate has_cycle. Instead, test the public API:
        // After txn 1's failed acquire, waiter is cleaned. Add manually:
        lt.waiters.entry(1).or_default().push("t2".into());
        let err2 = lt.try_acquire("t1", LockMode::Exclusive, 2);
        assert!(err2.is_err());
        let msg = format!("{}", err2.unwrap_err());
        assert!(msg.contains("Deadlock detected"));
    }

    #[test]
    fn test_diamond_wait_no_false_positive() {
        let mut lt = LockTable::new();
        // txn 1 holds t1
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        // txn 2 and txn 3 both wait for t1 (diamond shape, no cycle)
        lt.waiters.entry(2).or_default().push("t1".into());
        lt.waiters.entry(3).or_default().push("t1".into());
        // txn 4 wants t1 — should be a conflict, not a deadlock
        let err = lt.try_acquire("t1", LockMode::Exclusive, 4);
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("Lock conflict"));
        assert!(!msg.contains("Deadlock"));
    }

    #[test]
    fn test_case_insensitive_table_names() {
        let mut lt = LockTable::new();
        lt.try_acquire("MyTable", LockMode::Exclusive, 1).unwrap();
        // Same table different case should conflict
        let err = lt.try_acquire("mytable", LockMode::Exclusive, 2);
        assert!(err.is_err());
    }

    #[test]
    fn test_global_lock_table_returns_same_instance() {
        let a = global_lock_table();
        let b = global_lock_table();
        assert!(std::sync::Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn test_release_all_cleans_waiters() {
        let mut lt = LockTable::new();
        lt.try_acquire("t1", LockMode::Exclusive, 1).unwrap();
        lt.waiters.entry(1).or_default().push("t2".into());
        lt.release_all(1);
        assert!(!lt.waiters.contains_key(&1));
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  2. VectorIndex + VectorIndexRegistry uncovered methods
// ═══════════════════════════════════════════════════════════════════════════════

mod vector_coverage_tests {
    use crate::vector::distance::DistanceMetric;
    use crate::vector::{VectorIndex, VectorIndexRegistry, parse_vec_json};

    #[test]
    fn test_insert_vec_dimension_mismatch() {
        let vi = VectorIndex::new(
            "idx".into(), "t".into(), "v".into(), 0, 3, DistanceMetric::Cosine, 0,
        );
        let err = vi.insert_vec(1, vec![1.0, 2.0]); // dim 2 != 3
        assert!(err.is_err());
        let msg = format!("{}", err.unwrap_err());
        assert!(msg.contains("expected dim=3"));
    }

    #[test]
    fn test_delete_vec_and_counts() {
        let vi = VectorIndex::new(
            "idx".into(), "t".into(), "v".into(), 0, 3, DistanceMetric::Cosine, 0,
        );
        vi.insert_vec(1, vec![1.0, 0.0, 0.0]).unwrap();
        vi.insert_vec(2, vec![0.0, 1.0, 0.0]).unwrap();
        vi.insert_vec(3, vec![0.0, 0.0, 1.0]).unwrap();
        assert_eq!(vi.live_count(), 3);
        assert_eq!(vi.deleted_count(), 0);

        vi.delete_vec(2);
        assert_eq!(vi.live_count(), 2);
        assert_eq!(vi.deleted_count(), 1);
    }

    #[test]
    fn test_search_with_ef() {
        let vi = VectorIndex::new(
            "idx".into(), "t".into(), "v".into(), 0, 3, DistanceMetric::Cosine, 0,
        );
        vi.insert_vec(1, vec![1.0, 0.0, 0.0]).unwrap();
        vi.insert_vec(2, vec![0.0, 1.0, 0.0]).unwrap();
        let results = vi.search_with_ef(&[1.0, 0.0, 0.0], 1, 100);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 1);
    }

    #[test]
    fn test_vector_index_debug() {
        let vi = VectorIndex::new(
            "my_idx".into(), "tbl".into(), "col".into(), 0, 4, DistanceMetric::L2, 7,
        );
        let dbg = format!("{:?}", vi);
        assert!(dbg.contains("my_idx"));
        assert!(dbg.contains("tbl"));
        assert!(dbg.contains("col"));
        assert!(dbg.contains("dim: 4"));
        assert!(dbg.contains("index_id: 7"));
    }

    #[test]
    fn test_registry_drop() {
        let mut reg = VectorIndexRegistry::new();
        let vi = VectorIndex::new(
            "idx1".into(), "t1".into(), "v".into(), 0, 3, DistanceMetric::Cosine, 0,
        );
        reg.register(vi);
        assert!(reg.get("idx1").is_some());

        let dropped = reg.drop("idx1");
        assert!(dropped.is_some());
        assert!(reg.get("idx1").is_none());
        assert!(reg.for_table("t1").is_empty());
    }

    #[test]
    fn test_registry_drop_nonexistent() {
        let mut reg = VectorIndexRegistry::new();
        assert!(reg.drop("nope").is_none());
    }

    #[test]
    fn test_registry_iter() {
        let mut reg = VectorIndexRegistry::new();
        assert_eq!(reg.iter().count(), 0);
        reg.register(VectorIndex::new(
            "a".into(), "t".into(), "c".into(), 0, 2, DistanceMetric::Cosine, 0,
        ));
        reg.register(VectorIndex::new(
            "b".into(), "t".into(), "c".into(), 0, 2, DistanceMetric::L2, 1,
        ));
        assert_eq!(reg.iter().count(), 2);
    }

    #[test]
    fn test_registry_is_empty() {
        let mut reg = VectorIndexRegistry::new();
        assert!(reg.is_empty());
        reg.register(VectorIndex::new(
            "x".into(), "t".into(), "c".into(), 0, 2, DistanceMetric::Cosine, 0,
        ));
        assert!(!reg.is_empty());
    }

    #[test]
    fn test_parse_vec_json_edge_cases() {
        // Empty brackets
        assert!(parse_vec_json("[]").is_none());
        // Non-numeric
        assert!(parse_vec_json("[abc, def]").is_none());
        // No brackets
        let v = parse_vec_json("1.0, 2.0, 3.0").unwrap();
        assert_eq!(v, vec![1.0, 2.0, 3.0]);
        // Whitespace
        let v = parse_vec_json("  [  1.5 , 2.5 ]  ").unwrap();
        assert_eq!(v, vec![1.5, 2.5]);
        // Empty string
        assert!(parse_vec_json("").is_none());
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  3. ConnectionPool uncovered paths
// ═══════════════════════════════════════════════════════════════════════════════

mod connection_pool_coverage {
    use crate::vm::connection_pool::{ConnectionPool, PoolConfig};
    use super::super::VM;
    use crate::types::Value;

    #[test]
    fn test_pool_vm_accessor() {
        let pool = ConnectionPool::new_memory(2);
        let vm_ref = pool.vm();
        let mut vm = vm_ref.lock().unwrap();
        vm.execute_sql("CREATE TABLE pool_vm_test (id INTEGER PRIMARY KEY)").unwrap();
    }

    #[test]
    fn test_pool_execute_params() {
        let pool = ConnectionPool::new_memory(2);
        {
            let conn = pool.checkout().unwrap();
            conn.execute("CREATE TABLE param_test (id INTEGER PRIMARY KEY, val INTEGER)").unwrap();
            conn.execute("INSERT INTO param_test VALUES (1, 100)").unwrap();
        }
        {
            let conn = pool.checkout().unwrap();
            let result = conn.execute_params(
                "SELECT val FROM param_test WHERE id = ?",
                vec![Value::Integer(1)],
            ).unwrap();
            if let crate::vm::execute::ExecResult::QueryResult { rows, .. } = result {
                assert_eq!(rows[0][0], Value::Integer(100));
            } else {
                panic!("expected query result");
            }
        }
    }

    #[test]
    fn test_pool_no_timeout_config() {
        // checkout_timeout: None → uses cvar.wait() (no deadline)
        let vm = std::sync::Arc::new(std::sync::Mutex::new(
            VM::new_memory(),
        ));
        let pool = ConnectionPool::new(vm, PoolConfig {
            max_connections: 4,
            checkout_timeout: None,
        });
        let conn = pool.checkout().unwrap();
        assert_eq!(conn.id(), 1);
    }

    #[test]
    fn test_pool_config_default() {
        let cfg = PoolConfig::default();
        assert_eq!(cfg.max_connections, 64);
        assert!(cfg.checkout_timeout.is_some());
    }

    #[test]
    fn test_pool_connection_id_monotonic() {
        let pool = ConnectionPool::new_memory(10);
        let c1 = pool.checkout().unwrap();
        let c2 = pool.checkout().unwrap();
        assert_eq!(c1.id(), 1);
        assert_eq!(c2.id(), 2);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  4. Query cache edge cases via integration
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_query_cache_subquery_invalidation() {
    // Ensures subquery tables are tracked for invalidation
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE main_tbl (id INTEGER PRIMARY KEY, val TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE ref_tbl (id INTEGER PRIMARY KEY)").unwrap();
    vm.execute_sql("INSERT INTO main_tbl VALUES (1, 'a')").unwrap();

    // SELECT with IN subquery
    let rows = query_rows(
        &mut vm,
        "SELECT val FROM main_tbl WHERE id IN (SELECT id FROM ref_tbl)",
    );
    assert_eq!(rows.len(), 0);

    // Insert into subquery table
    vm.execute_sql("INSERT INTO ref_tbl VALUES (1)").unwrap();

    // Should NOT return cached (empty) result
    let rows = query_rows(
        &mut vm,
        "SELECT val FROM main_tbl WHERE id IN (SELECT id FROM ref_tbl)",
    );
    assert_eq!(rows.len(), 1);
}

#[test]
fn test_query_cache_scalar_subquery_invalidation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE sq_main (id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("CREATE TABLE sq_counts (cnt INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO sq_main VALUES (1, 'test')").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT name FROM sq_main WHERE id > (SELECT COALESCE(MAX(cnt), 0) FROM sq_counts)",
    );
    assert_eq!(rows.len(), 1);

    vm.execute_sql("INSERT INTO sq_counts VALUES (100)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT name FROM sq_main WHERE id > (SELECT COALESCE(MAX(cnt), 0) FROM sq_counts)",
    );
    assert_eq!(rows.len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════════
//  5. WAL group commit stats edge cases
// ═══════════════════════════════════════════════════════════════════════════════

mod wal_coverage {
    use crate::storage::wal::{Wal, WalSyncMode};
    use crate::storage::pager::PAGE_SIZE;

    #[test]
    fn test_wal_nosync_mode() {
        let uuid = [0u8; 16];
        let mut w = Wal::open_memory(&uuid);
        w.set_sync_mode(WalSyncMode::NoSync);
        assert!(matches!(w.sync_mode(), WalSyncMode::NoSync));
        // In NoSync mode, commits increment counter but skip fsync
        w.write_page(1, &[0u8; PAGE_SIZE]).unwrap();
        w.commit(1).unwrap();
        let stats = w.wal_stats();
        assert_eq!(stats.total_commits, 1);
        assert_eq!(stats.total_fsyncs, 0);
    }

    #[test]
    fn test_wal_group_commit_stats() {
        let uuid = [0u8; 16];
        let mut w = Wal::open_memory(&uuid);
        w.set_sync_mode(WalSyncMode::GroupCommit);
        // Multiple commits without sync (in-memory WAL has no file, so
        // pending_sync_commits won't increment — but stats are still tracked)
        for i in 0..3u32 {
            w.write_page(i + 1, &[0u8; PAGE_SIZE]).unwrap();
            w.commit(i + 1).unwrap();
        }
        let stats = w.wal_stats();
        assert_eq!(stats.total_commits, 3);
        assert_eq!(stats.total_fsyncs, 0);
        assert_eq!(stats.total_frames_written, 3);
        // group_sync on memory WAL returns 0 pending (no file backing)
        let synced = w.group_sync().unwrap();
        assert_eq!(synced, 0);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
//  6. SHOW ENGINE STATUS displays new sections
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_show_engine_status_wal_section() {
    let mut vm = VM::new_memory();
    let result = vm.execute_sql("SHOW ENGINE STATUS").unwrap();
    if let crate::vm::execute::ExecResult::Explain { plan } = result {
        assert!(plan.contains("WAL"));
        assert!(plan.contains("Query Cache"));
    } else {
        panic!("expected Explain result");
    }
}

#[test]
fn test_show_engine_status_query_cache_section() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE ses_tbl (id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("INSERT INTO ses_tbl VALUES (1, 'x')").unwrap();
    // Trigger a cache entry
    query_rows(&mut vm, "SELECT * FROM ses_tbl");
    let result = vm.execute_sql("SHOW ENGINE STATUS").unwrap();
    if let crate::vm::execute::ExecResult::Explain { plan } = result {
        assert!(plan.contains("Cache lookups"));
        assert!(plan.contains("Cache entries"));
    } else {
        panic!("expected Explain result");
    }
}
