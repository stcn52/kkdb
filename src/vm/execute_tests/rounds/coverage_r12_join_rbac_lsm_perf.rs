// R12 — Coverage tests for Round 12 features:
//   1. Adaptive JOIN algorithm selection + materialized view registry
//   2. RBAC permissions model + audit persistence
//   3. LSM-Tree compaction + dictionary compression + hot/cold tiering
//   4. Performance counters + slow query log + plan cache stats
//   Integration: VM SQL tests

use super::*;
use std::sync::Arc;
use std::time::Duration;

// ════════════════════════════════════════════════════════════════════════
// 1. Adaptive JOIN algorithm selection
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_join_selector_small_tables() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(1024 * 1024);
    let left = TableStats::new(5, 64);
    let right = TableStats::new(20, 64);
    assert_eq!(sel.select(&left, &right), JoinAlgorithm::NestedLoop);
}

#[test]
fn test_join_selector_hash_join() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(10 * 1024 * 1024);
    let left = TableStats::new(5000, 100);
    let right = TableStats::new(500_000, 100);
    assert_eq!(sel.select(&left, &right), JoinAlgorithm::HashJoin);
}

#[test]
fn test_join_selector_sort_merge() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(1024); // tiny budget
    let mut left = TableStats::new(50_000, 200);
    let mut right = TableStats::new(50_000, 200);
    left.is_sorted_on_join_key = true;
    right.is_sorted_on_join_key = true;
    assert_eq!(sel.select(&left, &right), JoinAlgorithm::SortMerge);
}

#[test]
fn test_join_selector_cost_comparison() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(10 * 1024 * 1024);
    let left = TableStats::new(10_000, 100);
    let right = TableStats::new(10_000, 100);
    let nl = sel.estimate_cost(JoinAlgorithm::NestedLoop, &left, &right);
    let hj = sel.estimate_cost(JoinAlgorithm::HashJoin, &left, &right);
    assert!(hj < nl);
}

#[test]
fn test_join_selector_indexed() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(1024 * 1024);
    let left = TableStats::new(10, 64);
    let mut right = TableStats::new(1_000_000, 64);
    right.has_index_on_join_key = true;
    assert_eq!(sel.select(&left, &right), JoinAlgorithm::NestedLoop);
}

#[test]
fn test_join_selector_custom_threshold() {
    use crate::vm::adaptive_join::{JoinSelector, JoinAlgorithm, TableStats};
    let sel = JoinSelector::new(1024 * 1024).with_nested_loop_threshold(500);
    let left = TableStats::new(200, 64);
    let right = TableStats::new(300, 64);
    assert_eq!(sel.select(&left, &right), JoinAlgorithm::NestedLoop);
    assert_eq!(sel.nested_loop_threshold(), 500);
}

// ════════════════════════════════════════════════════════════════════════
// 2. Materialized View Registry
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_matview_registry_register() {
    use crate::vm::adaptive_join::{MaterializedViewRegistry, MaterializedViewDef};
    let mut reg = MaterializedViewRegistry::new();
    let view = MaterializedViewDef::new("mv_totals", "SELECT sum(x) FROM t", vec!["t".into()]);
    assert!(reg.register(view));
    assert_eq!(reg.len(), 1);
    assert!(!reg.register(MaterializedViewDef::new("mv_totals", "x", vec![])));
}

#[test]
fn test_matview_stale_views() {
    use crate::vm::adaptive_join::{MaterializedViewRegistry, MaterializedViewDef};
    let mut reg = MaterializedViewRegistry::new();
    let mut v1 = MaterializedViewDef::new("mv1", "SELECT 1", vec!["t1".into()]);
    v1.mark_refreshed();
    let v2 = MaterializedViewDef::new("mv2", "SELECT 2", vec!["t2".into()]);
    reg.register(v1);
    reg.register(v2);
    // mv2 is stale (default)
    let stale = reg.stale_views();
    assert!(stale.contains(&"mv2"));
}

#[test]
fn test_matview_invalidate_for_table() {
    use crate::vm::adaptive_join::{MaterializedViewRegistry, MaterializedViewDef};
    let mut reg = MaterializedViewRegistry::new();
    let mut v = MaterializedViewDef::new("mv1", "q", vec!["orders".into()]);
    v.mark_refreshed();
    reg.register(v);
    assert!(reg.stale_views().is_empty());
    reg.invalidate_for_table("orders");
    assert_eq!(reg.stale_views().len(), 1);
}

#[test]
fn test_matview_unregister() {
    use crate::vm::adaptive_join::{MaterializedViewRegistry, MaterializedViewDef};
    let mut reg = MaterializedViewRegistry::new();
    reg.register(MaterializedViewDef::new("mv1", "q", vec![]));
    assert!(reg.unregister("mv1"));
    assert!(!reg.unregister("mv1"));
    assert!(reg.is_empty());
}

// ════════════════════════════════════════════════════════════════════════
// 3. RBAC permissions model
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_rbac_create_user_and_role() {
    use crate::vm::rbac::{RbacManager, Privilege};
    let mut mgr = RbacManager::new();
    assert!(mgr.create_user("alice"));
    assert!(!mgr.create_user("alice")); // duplicate
    assert!(mgr.create_role("reader"));
    assert!(!mgr.create_role("reader"));
    assert_eq!(mgr.user_count(), 1);
    assert_eq!(mgr.role_count(), 1);
}

#[test]
fn test_rbac_grant_role_and_check() {
    use crate::vm::rbac::{RbacManager, Privilege};
    let mut mgr = RbacManager::new();
    mgr.create_user("bob");
    mgr.create_role("editor");
    mgr.get_role_mut("editor").unwrap().grant_global(Privilege::Select);
    mgr.get_role_mut("editor").unwrap().grant_global(Privilege::Update);
    mgr.grant_role("bob", "editor");

    assert!(mgr.check_privilege("bob", "t", Privilege::Select));
    assert!(mgr.check_privilege("bob", "t", Privilege::Update));
    assert!(!mgr.check_privilege("bob", "t", Privilege::Delete));
}

#[test]
fn test_rbac_superuser() {
    use crate::vm::rbac::{RbacManager, Privilege};
    let mut mgr = RbacManager::new();
    mgr.create_user("root");
    mgr.set_superuser("root", true);
    assert!(mgr.check_privilege("root", "any", Privilege::Drop));
}

#[test]
fn test_rbac_direct_table_privilege() {
    use crate::vm::rbac::{RbacManager, Privilege};
    let mut mgr = RbacManager::new();
    mgr.create_user("carol");
    mgr.grant_direct_table("carol", "orders", Privilege::Insert);
    assert!(mgr.check_privilege("carol", "orders", Privilege::Insert));
    assert!(!mgr.check_privilege("carol", "users", Privilege::Insert));
}

#[test]
fn test_rbac_drop_role_cascades() {
    use crate::vm::rbac::{RbacManager, Privilege};
    let mut mgr = RbacManager::new();
    mgr.create_user("dave");
    mgr.create_role("temp");
    mgr.grant_role("dave", "temp");
    mgr.drop_role("temp");
    assert!(!mgr.get_user("dave").unwrap().roles.contains("temp"));
}

#[test]
fn test_rbac_privilege_parse() {
    use crate::vm::rbac::Privilege;
    assert_eq!(Privilege::from_str_name("select"), Some(Privilege::Select));
    assert_eq!(Privilege::from_str_name("ALL"), Some(Privilege::All));
    assert_eq!(Privilege::from_str_name("bogus"), None);
}

#[test]
fn test_rbac_grant_all() {
    use crate::vm::rbac::{RbacManager, Privilege, Role};
    let mut role = Role::new("admin");
    role.grant_global(Privilege::All);
    assert!(role.has_privilege("t", Privilege::Select));
    assert!(role.has_privilege("t", Privilege::Drop));
}

// ════════════════════════════════════════════════════════════════════════
// 4. Audit Persistence
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_audit_persistence_append_flush() {
    use crate::vm::rbac::AuditPersistence;
    let mut ap = AuditPersistence::new(3);
    assert!(!ap.append("e1".into()));
    assert!(!ap.append("e2".into()));
    assert!(ap.append("e3".into())); // full
    assert!(ap.is_full());
    let flushed = ap.flush();
    assert_eq!(flushed.len(), 3);
    assert_eq!(ap.buffered_count(), 0);
    assert_eq!(ap.total_flushed(), 3);
}

// ════════════════════════════════════════════════════════════════════════
// 5. LSM compaction
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_lsm_flush_compact() {
    use crate::storage::lsm::LsmCompactor;
    let mut c = LsmCompactor::new(3, 4, 10);
    for _ in 0..4 {
        c.flush_memtable(1024);
    }
    assert!(c.needs_compaction());
    let n = c.compact();
    assert_eq!(n, 1);
    assert_eq!(c.level(0).unwrap().run_count, 0);
    assert_eq!(c.level(1).unwrap().run_count, 1);
    assert_eq!(c.total_bytes_compacted(), 4096);
}

#[test]
fn test_lsm_cascading() {
    use crate::storage::lsm::LsmCompactor;
    let mut c = LsmCompactor::new(4, 2, 2);
    for _ in 0..16 {
        c.flush_memtable(100);
        c.compact_all();
    }
    assert!(c.total_compactions() > 0);
}

// ════════════════════════════════════════════════════════════════════════
// 6. Dictionary compression
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_dict_compression_encode_decode() {
    use crate::storage::lsm::DictionaryCompressor;
    let mut d = DictionaryCompressor::new();
    let c1 = d.encode("apple");
    let c2 = d.encode("banana");
    assert_ne!(c1, c2);
    assert_eq!(d.encode("apple"), c1); // same code
    assert_eq!(d.decode(c1), Some("apple"));
    assert_eq!(d.decode(c2), Some("banana"));
    assert_eq!(d.len(), 2);
}

#[test]
fn test_dict_compression_savings() {
    use crate::storage::lsm::DictionaryCompressor;
    let mut d = DictionaryCompressor::new();
    d.encode("category_a");
    d.encode("category_b");
    let savings = d.estimate_savings(1000, 10);
    assert!(savings > 0, "expected savings, got {}", savings);
}

// ════════════════════════════════════════════════════════════════════════
// 7. Hot/Cold tiering
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_hot_cold_tiering() {
    use crate::storage::lsm::HotColdTiering;
    let mut t = HotColdTiering::new(5);
    for _ in 0..10 { t.record_access(1); }
    for _ in 0..2 { t.record_access(2); }
    assert!(t.is_hot(1));
    assert!(t.is_cold(2));
    assert_eq!(t.tracked_pages(), 2);
}

#[test]
fn test_hot_cold_decay() {
    use crate::storage::lsm::HotColdTiering;
    let mut t = HotColdTiering::new(5);
    for _ in 0..10 { t.record_access(1); }
    t.decay(); // 10 → 5
    assert!(t.is_hot(1));
    t.decay(); // 5 → 2
    assert!(t.is_cold(1));
}

// ════════════════════════════════════════════════════════════════════════
// 8. Performance counters
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_perf_counters_snapshot() {
    use crate::vm::perf_counter::PerfCounters;
    let c = PerfCounters::new();
    c.inc_queries();
    c.inc_queries();
    c.add_rows_read(500);
    c.add_rows_written(100);
    c.inc_tx_committed();
    c.inc_deadlocks();
    let s = c.snapshot();
    assert_eq!(s.queries_executed, 2);
    assert_eq!(s.rows_read, 500);
    assert_eq!(s.rows_written, 100);
    assert_eq!(s.transactions_committed, 1);
    assert_eq!(s.deadlocks_detected, 1);
}

#[test]
fn test_perf_counters_cache_ratio() {
    use crate::vm::perf_counter::PerfCounters;
    let c = PerfCounters::new();
    for _ in 0..3 { c.inc_cache_hit(); }
    c.inc_cache_miss();
    assert!((c.cache_hit_ratio() - 0.75).abs() < 1e-9);
}

// ════════════════════════════════════════════════════════════════════════
// 9. Slow query log
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_slow_query_log() {
    use crate::vm::perf_counter::SlowQueryLog;
    let mut log = SlowQueryLog::new(Duration::from_millis(100), 5);
    assert!(!log.record("fast", Duration::from_millis(5), 10, 10));
    assert!(log.record("slow", Duration::from_millis(500), 100000, 50));
    assert_eq!(log.len(), 1);
    assert_eq!(log.total_slow(), 1);
}

#[test]
fn test_slow_query_slowest() {
    use crate::vm::perf_counter::SlowQueryLog;
    let mut log = SlowQueryLog::new(Duration::from_millis(1), 10);
    log.record("Q1", Duration::from_millis(10), 0, 0);
    log.record("Q2", Duration::from_millis(999), 0, 0);
    log.record("Q3", Duration::from_millis(50), 0, 0);
    assert_eq!(log.slowest().unwrap().sql, "Q2");
}

#[test]
fn test_slow_query_avg() {
    use crate::vm::perf_counter::SlowQueryLog;
    let mut log = SlowQueryLog::new(Duration::from_millis(1), 10);
    log.record("Q1", Duration::from_millis(100), 0, 0);
    log.record("Q2", Duration::from_millis(200), 0, 0);
    assert_eq!(log.avg_duration().unwrap(), Duration::from_millis(150));
}

// ════════════════════════════════════════════════════════════════════════
// 10. Plan cache stats
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_plan_cache_stats() {
    use crate::vm::perf_counter::PlanCacheStats;
    let mut s = PlanCacheStats::new();
    s.record_hit();
    s.record_hit();
    s.record_miss();
    assert!((s.hit_ratio() - 2.0/3.0).abs() < 1e-9);
    s.record_insert();
    s.record_eviction();
    assert!((s.eviction_ratio() - 1.0).abs() < 1e-9);
}

// ════════════════════════════════════════════════════════════════════════
// 11. VM integration tests
// ════════════════════════════════════════════════════════════════════════

#[test]
fn test_vm_complex_join() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE orders(id INTEGER PRIMARY KEY, customer_id INTEGER, amount REAL)").unwrap();
    vm.execute_sql("CREATE TABLE customers(id INTEGER PRIMARY KEY, name TEXT)").unwrap();
    vm.execute_sql("INSERT INTO customers VALUES(1, 'Alice')").unwrap();
    vm.execute_sql("INSERT INTO customers VALUES(2, 'Bob')").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES(1, 1, 100.0)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES(2, 1, 200.0)").unwrap();
    vm.execute_sql("INSERT INTO orders VALUES(3, 2, 50.0)").unwrap();

    let rows = query_rows(&mut vm, "SELECT c.name, SUM(o.amount) FROM customers c JOIN orders o ON c.id = o.customer_id GROUP BY c.name ORDER BY c.name");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0], Value::Text("Alice".into()));
}

#[test]
fn test_vm_subquery_in_where() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(id INTEGER PRIMARY KEY, val INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(1, 10)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(2, 20)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(3, 30)").unwrap();

    let rows = query_rows(&mut vm, "SELECT * FROM t WHERE val > (SELECT val FROM t WHERE id = 1)");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_vm_aggregate_functions() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE nums(v INTEGER)").unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO nums VALUES({})", i)).unwrap();
    }

    let rows = query_rows(&mut vm, "SELECT COUNT(*), SUM(v), AVG(v), MIN(v), MAX(v) FROM nums");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], Value::Integer(10)); // COUNT
}

#[test]
fn test_vm_case_expression() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE items(name TEXT, price REAL)").unwrap();
    vm.execute_sql("INSERT INTO items VALUES('a', 10.0)").unwrap();
    vm.execute_sql("INSERT INTO items VALUES('b', 50.0)").unwrap();
    vm.execute_sql("INSERT INTO items VALUES('c', 100.0)").unwrap();

    let rows = query_rows(&mut vm, "SELECT name, CASE WHEN price < 30 THEN 'cheap' WHEN price < 80 THEN 'mid' ELSE 'expensive' END FROM items ORDER BY name");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][1], Value::Text("cheap".into()));
    assert_eq!(rows[2][1], Value::Text("expensive".into()));
}

#[test]
fn test_vm_create_drop_table() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE temp(id INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO temp VALUES(1)").unwrap();
    vm.execute_sql("DROP TABLE temp").unwrap();
    let r = vm.execute_sql("SELECT * FROM temp");
    assert!(r.is_err());
}

#[test]
fn test_vm_transaction_commit_rollback() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT)").unwrap();
    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(1, 'a')").unwrap();
    vm.execute_sql("COMMIT").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows.len(), 1);

    vm.execute_sql("BEGIN").unwrap();
    vm.execute_sql("INSERT INTO t VALUES(2, 'b')").unwrap();
    vm.execute_sql("ROLLBACK").unwrap();
    let rows = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows.len(), 1); // rolled back
}
