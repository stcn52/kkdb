// R11 — Coverage tests for Round 11 features:
//   1. Histogram selectivity estimation (ColumnStats)
//   2. Audit log + SQL injection detection
//   3. Page-level checksums + incremental backup
//   4. Consistent hashing + shard routing
//   Integration tests via VM: schema, PREPARE/EXECUTE, window functions

use super::*;

// ════════════════════════════════════════════════════════════════════════
// 1. Histogram selectivity estimation (ColumnStats / HistogramBucket)
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_selectivity_eq_basic() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 1000,
        null_count: 50,
        ndv: 10,
        min: Some(Value::Integer(1)),
        max: Some(Value::Integer(100)),
        histogram: None,
    };
    let sel = stats.selectivity_eq(&Value::Integer(42));
    assert!((sel - 0.1).abs() < 1e-9, "1/10 = 0.1, got {}", sel);
}

#[test]
fn test_selectivity_eq_zero_ndv() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 0,
        min: None,
        max: None,
        histogram: None,
    };
    assert_eq!(stats.selectivity_eq(&Value::Integer(1)), 0.0);
}

#[test]
fn test_selectivity_eq_zero_total() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 0,
        null_count: 0,
        ndv: 0,
        min: None,
        max: None,
        histogram: None,
    };
    assert_eq!(stats.selectivity_eq(&Value::Integer(1)), 0.0);
}

#[test]
fn test_selectivity_lt_linear_interpolation() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 100,
        min: Some(Value::Integer(0)),
        max: Some(Value::Integer(100)),
        histogram: None,
    };
    let sel = stats.selectivity_lt(&Value::Integer(50));
    assert!((sel - 0.5).abs() < 1e-9, "expected 0.5, got {}", sel);
}

#[test]
fn test_selectivity_lt_with_histogram() {
    use crate::schema::{ColumnStats, HistogramBucket};
    let stats = ColumnStats {
        total_count: 1000,
        null_count: 0,
        ndv: 100,
        min: Some(Value::Integer(1)),
        max: Some(Value::Integer(1000)),
        histogram: Some(vec![
            HistogramBucket::new(Value::Integer(100), 100, 10),
            HistogramBucket::new(Value::Integer(500), 500, 40),
            HistogramBucket::new(Value::Integer(1000), 1000, 50),
        ]),
    };
    // value = 200 → falls in bucket [100, 500], cumulative = 500
    let sel = stats.selectivity_lt(&Value::Integer(200));
    assert!(
        (sel - 0.5).abs() < 1e-9,
        "expected 0.5 from bucket, got {}",
        sel
    );
}

#[test]
fn test_selectivity_lt_above_all_buckets() {
    use crate::schema::{ColumnStats, HistogramBucket};
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 10,
        min: Some(Value::Integer(1)),
        max: Some(Value::Integer(100)),
        histogram: Some(vec![
            HistogramBucket::new(Value::Integer(50), 50, 5),
            HistogramBucket::new(Value::Integer(100), 100, 5),
        ]),
    };
    // value = 200 is above all upper bounds
    let sel = stats.selectivity_lt(&Value::Integer(200));
    assert!((sel - 1.0).abs() < 1e-9);
}

#[test]
fn test_selectivity_lt_below_min() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 10,
        min: Some(Value::Integer(10)),
        max: Some(Value::Integer(100)),
        histogram: None,
    };
    let sel = stats.selectivity_lt(&Value::Integer(5));
    assert_eq!(sel, 0.0);
}

#[test]
fn test_selectivity_lt_above_max() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 10,
        min: Some(Value::Integer(10)),
        max: Some(Value::Integer(100)),
        histogram: None,
    };
    let sel = stats.selectivity_lt(&Value::Integer(200));
    assert_eq!(sel, 1.0);
}

#[test]
fn test_selectivity_between() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 100,
        min: Some(Value::Integer(0)),
        max: Some(Value::Integer(100)),
        histogram: None,
    };
    let sel = stats.selectivity_between(&Value::Integer(25), &Value::Integer(75));
    assert!((sel - 0.5).abs() < 1e-9, "expected 0.5, got {}", sel);
}

#[test]
fn test_null_fraction() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 200,
        null_count: 50,
        ndv: 10,
        min: None,
        max: None,
        histogram: None,
    };
    assert!((stats.null_fraction() - 0.25).abs() < 1e-9);
}

#[test]
fn test_null_fraction_zero_total() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 0,
        null_count: 0,
        ndv: 0,
        min: None,
        max: None,
        histogram: None,
    };
    assert_eq!(stats.null_fraction(), 0.0);
}

#[test]
fn test_histogram_bucket_new() {
    use crate::schema::HistogramBucket;
    let b = HistogramBucket::new(Value::Integer(42), 100, 5);
    assert_eq!(b.upper_bound, Value::Integer(42));
    assert_eq!(b.cumulative_count, 100);
    assert_eq!(b.ndv_in_bucket, 5);
}

#[test]
fn test_selectivity_lt_empty_histogram() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 10,
        min: Some(Value::Integer(0)),
        max: Some(Value::Integer(100)),
        histogram: Some(vec![]),
    };
    let sel = stats.selectivity_lt(&Value::Integer(50));
    assert!((sel - 0.5).abs() < 1e-9);
}

#[test]
fn test_selectivity_lt_no_min_max() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 10,
        min: None,
        max: None,
        histogram: None,
    };
    let sel = stats.selectivity_lt(&Value::Integer(50));
    assert!((sel - 0.5).abs() < 1e-9);
}

#[test]
fn test_selectivity_lt_equal_min_max() {
    use crate::schema::ColumnStats;
    let stats = ColumnStats {
        total_count: 100,
        null_count: 0,
        ndv: 1,
        min: Some(Value::Integer(42)),
        max: Some(Value::Integer(42)),
        histogram: None,
    };
    let sel = stats.selectivity_lt(&Value::Integer(42));
    assert!((sel - 0.5).abs() < 1e-9);
}

// ════════════════════════════════════════════════════════════════════════
// 2. Audit log + SQL injection detection
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_audit_log_record_and_len() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    assert_eq!(log.len(), 0);
    log.record("admin", "SELECT 1", true, 1, None);
    assert_eq!(log.len(), 1);
    let entries = log.entries();
    assert_eq!(entries[0].user, "admin");
    assert_eq!(entries[0].sql, "SELECT 1");
    assert!(entries[0].success);
}

#[test]
fn test_audit_log_capacity_eviction() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(3);
    log.enable();
    for i in 0..5 {
        log.record("u", &format!("SELECT {}", i), true, 0, None);
    }
    assert_eq!(log.len(), 3);
    // oldest entries evicted: should only have 2, 3, 4
    let entries = log.entries();
    assert!(entries[0].sql.contains('2'));
    assert!(entries[2].sql.contains('4'));
}

#[test]
fn test_audit_log_category_detection() {
    use crate::vm::audit::{AuditCategory, AuditLog};
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    log.record("u", "CREATE TABLE t(id INT)", true, 0, None);
    log.record("u", "INSERT INTO t VALUES(1)", true, 1, None);
    log.record("u", "SELECT * FROM t", true, 1, None);
    log.record("u", "BEGIN", true, 0, None);

    assert_eq!(log.count_by_category(AuditCategory::Ddl), 1);
    assert_eq!(log.count_by_category(AuditCategory::Dml), 1);
    assert_eq!(log.count_by_category(AuditCategory::Query), 1);
    assert_eq!(log.count_by_category(AuditCategory::Txn), 1);
}

#[test]
fn test_audit_log_search() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    log.record("admin", "SELECT * FROM users", true, 5, None);
    log.record("admin", "INSERT INTO orders VALUES(1)", true, 1, None);
    log.record("guest", "SELECT 1", true, 1, None);

    let results = log.search("users");
    assert_eq!(results.len(), 1);
    assert!(results[0].sql.contains("users"));
}

#[test]
fn test_audit_log_failure_count() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    log.record("u", "SELECT 1", true, 1, None);
    log.record("u", "BAD SQL", false, 0, Some("parse error"));
    log.record("u", "ALSO BAD", false, 0, Some("nope"));
    assert_eq!(log.failure_count(), 2);
}

#[test]
fn test_audit_log_drain() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    log.record("u", "SELECT 1", true, 1, None);
    log.record("u", "SELECT 2", true, 1, None);
    let drained = log.drain();
    assert_eq!(drained.len(), 2);
    assert_eq!(log.len(), 0);
}

#[test]
fn test_audit_log_last_n() {
    use crate::vm::audit::AuditLog;
    let mut log = AuditLog::with_capacity(100);
    log.enable();
    for i in 0..10 {
        log.record("u", &format!("SELECT {}", i), true, 0, None);
    }
    let last3 = log.last_n(3);
    assert_eq!(last3.len(), 3);
    assert!(last3[0].sql.contains('7'));
    assert!(last3[2].sql.contains('9'));
}

#[test]
fn test_detect_sql_injection_safe() {
    use crate::vm::audit::detect_sql_injection;
    assert!(!detect_sql_injection("SELECT * FROM users WHERE id = 1"));
    assert!(!detect_sql_injection("INSERT INTO t VALUES(1, 'hello')"));
}

#[test]
fn test_detect_sql_injection_union_attack() {
    use crate::vm::audit::detect_sql_injection;
    // Must have quote before UNION to be flagged
    assert!(detect_sql_injection(
        "SELECT * FROM users WHERE id = ' UNION SELECT * FROM passwords"
    ));
}

#[test]
fn test_detect_sql_injection_comment_attack() {
    use crate::vm::audit::detect_sql_injection;
    // Pattern requires OR + 1=1 + --
    assert!(detect_sql_injection(
        "SELECT * FROM users WHERE id = 1 OR 1=1 --"
    ));
}

#[test]
fn test_detect_sql_injection_char_bypass() {
    use crate::vm::audit::detect_sql_injection;
    assert!(detect_sql_injection("SELECT CHAR(68) DROP TABLE"));
}

#[test]
fn test_detect_sql_injection_semicolon() {
    use crate::vm::audit::detect_sql_injection;
    assert!(detect_sql_injection("SELECT 1; DROP TABLE users"));
}

// ════════════════════════════════════════════════════════════════════════
// 3. Page-level checksums + incremental backup
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_page_checksum_registry_basic() {
    use crate::storage::backup::PageChecksumRegistry;
    use crate::storage::pager::PAGE_SIZE;

    let mut reg = PageChecksumRegistry::new();
    let page_data = vec![0xABu8; PAGE_SIZE];
    assert!(reg.update(1, &page_data));
    assert!(!reg.update(1, &page_data)); // no change
    assert_eq!(reg.len(), 1);

    // Verify ok
    assert_eq!(reg.verify(1, &page_data), Some(true));

    // Verify corruption
    let bad_data = vec![0xCDu8; PAGE_SIZE];
    assert_eq!(reg.verify(1, &bad_data), Some(false));
}

#[test]
fn test_page_checksum_registry_get_and_remove() {
    use crate::storage::backup::PageChecksumRegistry;
    use crate::storage::pager::PAGE_SIZE;

    let mut reg = PageChecksumRegistry::new();
    let data = vec![0x00u8; PAGE_SIZE];
    reg.update(42, &data);

    assert!(reg.get(42).is_some());
    assert!(reg.get(99).is_none());
    assert!(reg.remove(42));
    assert!(!reg.remove(42));
    assert!(reg.is_empty());
}

#[test]
fn test_incremental_backup_workflow() {
    use crate::storage::backup::IncrementalBackup;

    let mut bk = IncrementalBackup::new();
    assert_eq!(bk.epoch(), 0);

    // Epoch 0: pages 1, 3, 5 dirty
    bk.mark_dirty(1);
    bk.mark_dirty(3);
    bk.mark_dirty(5);
    assert_eq!(bk.dirty_count(), 3);
    let m0 = bk.snapshot(100);
    assert_eq!(m0.dirty_pages, vec![1, 3, 5]);
    assert_eq!(m0.epoch, 0);
    assert_eq!(bk.epoch(), 1);

    // Epoch 1: pages 2, 3 dirty
    bk.mark_dirty(2);
    bk.mark_dirty(3);
    let m1 = bk.snapshot(105);
    assert_eq!(m1.dirty_pages, vec![2, 3]);

    // Merge
    let merged = bk.dirty_pages_in_range(0, 1);
    assert_eq!(merged, vec![1, 2, 3, 5]);
    assert_eq!(bk.snapshot_count(), 2);
}

#[test]
fn test_incremental_backup_reset() {
    use crate::storage::backup::IncrementalBackup;
    let mut bk = IncrementalBackup::new();
    bk.mark_dirty(1);
    bk.snapshot(10);
    bk.reset();
    assert_eq!(bk.epoch(), 0);
    assert_eq!(bk.snapshot_count(), 0);
    assert_eq!(bk.dirty_count(), 0);
}

// ════════════════════════════════════════════════════════════════════════
// 4. Consistent hashing + shard routing
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_hash_ring_basic() {
    use crate::raft::consistent_hash::HashRing;
    let mut ring = HashRing::new(100);
    ring.add_node("shard-a");
    ring.add_node("shard-b");
    ring.add_node("shard-c");
    assert_eq!(ring.node_count(), 3);

    let node = ring.get_node("my-key");
    assert!(node.is_some());
    let n = node.unwrap();
    assert!(n == "shard-a" || n == "shard-b" || n == "shard-c");
}

#[test]
fn test_hash_ring_consistency() {
    use crate::raft::consistent_hash::HashRing;
    let mut ring = HashRing::new(150);
    ring.add_node("A");
    ring.add_node("B");

    let key = "user:12345";
    let n1 = ring.get_node(key).unwrap().to_string();
    let n2 = ring.get_node(key).unwrap().to_string();
    assert_eq!(n1, n2, "same key must always map to same node");
}

#[test]
fn test_hash_ring_add_remove() {
    use crate::raft::consistent_hash::HashRing;
    let mut ring = HashRing::new(50);
    ring.add_node("X");
    ring.add_node("Y");
    assert_eq!(ring.node_count(), 2);

    ring.remove_node("X");
    assert_eq!(ring.node_count(), 1);
    // All keys should route to Y now
    assert_eq!(ring.get_node("anything").unwrap(), "Y");
}

#[test]
fn test_hash_ring_get_nodes_replication() {
    use crate::raft::consistent_hash::HashRing;
    let mut ring = HashRing::new(100);
    ring.add_node("R1");
    ring.add_node("R2");
    ring.add_node("R3");
    let nodes = ring.get_nodes("some-key", 2);
    assert_eq!(nodes.len(), 2);
    assert_ne!(nodes[0], nodes[1], "replicas must be on different nodes");
}

#[test]
fn test_hash_ring_empty() {
    use crate::raft::consistent_hash::HashRing;
    let ring = HashRing::new(50);
    assert!(ring.get_node("key").is_none());
    assert!(ring.get_nodes("key", 3).is_empty());
}

#[test]
fn test_hash_ring_distribution() {
    use crate::raft::consistent_hash::HashRing;
    let mut ring = HashRing::new(100);
    ring.add_node("alpha");
    ring.add_node("beta");
    let dist = ring.distribution(&["k1", "k2", "k3", "k4"]);
    assert!(!dist.is_empty());
    assert!(dist.contains_key("alpha") || dist.contains_key("beta"));
}

#[test]
fn test_shard_router_route() {
    use crate::raft::consistent_hash::ShardRouter;
    let router = ShardRouter::new(&["shard-1", "shard-2", "shard-3"], 100);
    let shard = router.route("users", "user:42");
    assert!(shard.is_some());
    assert_eq!(router.shard_count(), 3);
}

#[test]
fn test_shard_router_replicated() {
    use crate::raft::consistent_hash::ShardRouter;
    let router = ShardRouter::new(&["s1", "s2", "s3"], 100);
    let shards = router.route_replicated("orders", "order:1", 2);
    assert_eq!(shards.len(), 2);
    assert_ne!(shards[0], shards[1]);
}

#[test]
fn test_shard_router_add_remove() {
    use crate::raft::consistent_hash::ShardRouter;
    let mut router = ShardRouter::new(&["s1", "s2"], 50);
    assert_eq!(router.shard_count(), 2);
    router.add_shard("s3");
    assert_eq!(router.shard_count(), 3);
    router.remove_shard("s1");
    assert_eq!(router.shard_count(), 2);
}

// ════════════════════════════════════════════════════════════════════════
// 5. VM integration tests
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_vm_prepare_execute_deallocate() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES(1, 'alice')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(2, 'bob')").unwrap();

    // PREPARE
    let r = vm.execute_sql("PREPARE stmt AS SELECT * FROM t WHERE id = 1");
    assert!(r.is_ok());

    // EXECUTE
    let r = vm.execute_sql("EXECUTE stmt");
    assert!(r.is_ok());
    match r.unwrap() {
        ExecResult::QueryResult { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0][1], Value::Text("alice".into()));
        }
        other => panic!("expected QueryResult, got {:?}", other),
    }

    // DEALLOCATE
    let r = vm.execute_sql("DEALLOCATE stmt");
    assert!(r.is_ok());
}

#[test]
fn test_vm_window_row_number() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE scores(name TEXT, score INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO scores VALUES('a', 90)")
        .unwrap();
    vm.execute_sql("INSERT INTO scores VALUES('b', 80)")
        .unwrap();
    vm.execute_sql("INSERT INTO scores VALUES('c', 70)")
        .unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT name, ROW_NUMBER() OVER (ORDER BY score DESC) FROM scores",
    );
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Integer(1));
    assert_eq!(rows[2][1], Value::Integer(3));
}

#[test]
fn test_vm_window_rank_dense_rank() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE s(v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO s VALUES(10)").unwrap();
    vm.execute_sql("INSERT INTO s VALUES(10)").unwrap();
    vm.execute_sql("INSERT INTO s VALUES(20)").unwrap();

    let rows = query_rows(
        &mut vm,
        "SELECT v, RANK() OVER (ORDER BY v), DENSE_RANK() OVER (ORDER BY v) FROM s",
    );
    assert_eq!(rows.len(), 3);
    // RANK: 1, 1, 3;  DENSE_RANK: 1, 1, 2
    assert_eq!(rows[2][1], Value::Integer(3)); // RANK
    assert_eq!(rows[2][2], Value::Integer(2)); // DENSE_RANK
}

#[test]
fn test_vm_insert_or_replace() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE kv(k INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO kv VALUES(1, 'old')").unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO kv VALUES(1, 'new')")
        .unwrap();

    let rows = query_rows(&mut vm, "SELECT v FROM kv WHERE k = 1");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Text("new".into()));
}

#[test]
fn test_vm_insert_or_ignore() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE kv(k INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO kv VALUES(1, 'first')").unwrap();
    let r = vm.execute_sql("INSERT OR IGNORE INTO kv VALUES(1, 'second')");
    assert!(r.is_ok()); // should not error

    let rows = query_rows(&mut vm, "SELECT v FROM kv WHERE k = 1");
    assert_eq!(rows[0][0], Value::Text("first".into()));
}

#[test]
fn test_vm_bloom_filter_api() {
    use crate::storage::bloom::BloomFilter;
    let mut bf = BloomFilter::for_capacity(1000);
    bf.insert(b"key-1");
    bf.insert(b"key-2");
    assert!(bf.may_contain(b"key-1"));
    assert!(bf.may_contain(b"key-2"));
    // false positive possible but unlikely for this key
    let fp_count: usize = (0..100)
        .map(|i| bf.may_contain(format!("nonexist-{}", i).as_bytes()) as usize)
        .sum();
    assert!(fp_count < 10, "too many false positives: {}", fp_count);
}

#[test]
fn test_vm_bloom_filter_serialize() {
    use crate::storage::bloom::BloomFilter;
    let mut bf = BloomFilter::for_capacity(500);
    bf.insert(b"hello");
    bf.insert(b"world");
    let bytes = bf.to_bytes();
    let bf2 = BloomFilter::from_bytes(&bytes).unwrap();
    assert!(bf2.may_contain(b"hello"));
    assert!(bf2.may_contain(b"world"));
}

#[test]
fn test_vm_deadlock_detection() {
    use crate::vm::mvcc::WaitForGraph;
    let mut wfg = WaitForGraph::new();
    // T1 waits for T2, T2 waits for T1 → cycle
    wfg.add_wait(1, 2);
    let cycle = wfg.detect_deadlock(2, 1);
    assert!(cycle.is_some(), "should detect deadlock cycle");
    let path = cycle.unwrap();
    assert!(path.contains(&1) && path.contains(&2));
}

#[test]
fn test_vm_self_deadlock() {
    use crate::vm::mvcc::WaitForGraph;
    let wfg = WaitForGraph::new();
    let cycle = wfg.detect_deadlock(1, 1);
    assert!(cycle.is_some());
}

#[test]
fn test_vm_txn_timeout_manager() {
    use crate::vm::mvcc::TransactionTimeoutManager;
    use std::time::Duration;
    let mut mgr = TransactionTimeoutManager::new(Duration::from_millis(50));
    mgr.begin(1);
    mgr.begin(2);
    assert_eq!(mgr.active_count(), 2);
    assert!(!mgr.is_timed_out(1));

    // Override with very short timeout
    mgr.set_timeout(2, Duration::from_millis(1));
    std::thread::sleep(Duration::from_millis(10));
    assert!(mgr.is_timed_out(2));
    let timed_out = mgr.timed_out_transactions();
    assert!(timed_out.contains(&2));
}
