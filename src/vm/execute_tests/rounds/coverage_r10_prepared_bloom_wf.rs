// ── Round 10 Coverage Tests ──────────────────────────────────────────────────
//
// Tests for:
//   1. PreparedStore unit tests (prepare, execute, deallocate)
//   2. BloomFilter unit tests (insert, may_contain, merge, serialize)
//   3. WaitForGraph deadlock detection
//   4. TransactionTimeoutManager
//   5. PREPARE / EXECUTE / DEALLOCATE SQL integration
//   6. RANGE / GROUPS window frame support
//   7. INSERT OR REPLACE / INSERT OR IGNORE (UPSERT) integration

use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

fn vm() -> VM {
    VM::new_memory()
}

fn rows(r: &ExecResult) -> &Vec<Vec<Value>> {
    match r {
        ExecResult::QueryResult { rows, .. } => rows,
        other => panic!("expected QueryResult, got {:?}", other),
    }
}

// ════════════════════════════════════════════════════════════════════════════════
//  1. PreparedStore unit tests
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn prepared_store_basic() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    assert_eq!(store.count(), 0);
    store
        .prepare("my_stmt", "SELECT * FROM t WHERE id = ?")
        .unwrap();
    assert_eq!(store.count(), 1);
    let stmt = store.get("my_stmt").unwrap();
    assert_eq!(stmt.param_count, 1);
    assert_eq!(stmt.exec_count, 0);
    assert_eq!(stmt.name, "my_stmt");
}

#[test]
fn prepared_store_case_insensitive() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    store.prepare("MyStmt", "SELECT 1").unwrap();
    assert!(store.get("MYSTMT").is_some());
    assert!(store.get("mystmt").is_some());
    // Duplicate name should error
    let err = store.prepare("mystmt", "SELECT 2");
    assert!(err.is_err());
}

#[test]
fn prepared_store_get_for_execute() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    store.prepare("s1", "SELECT ?, ?").unwrap();
    let (sql, count) = store.get_for_execute("s1").unwrap();
    assert_eq!(sql, "SELECT ?, ?");
    assert_eq!(count, 2);
    // exec_count should increment
    assert_eq!(store.get("s1").unwrap().exec_count, 1);
    let _ = store.get_for_execute("s1").unwrap();
    assert_eq!(store.get("s1").unwrap().exec_count, 2);
}

#[test]
fn prepared_store_not_found() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    assert!(store.get_for_execute("nonexistent").is_err());
}

#[test]
fn prepared_store_deallocate() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    store.prepare("s1", "SELECT 1").unwrap();
    store.prepare("s2", "SELECT 2").unwrap();
    assert_eq!(store.count(), 2);
    assert!(store.deallocate("s1"));
    assert_eq!(store.count(), 1);
    assert!(!store.deallocate("s1")); // already removed
}

#[test]
fn prepared_store_clear() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    store.prepare("a", "SELECT 1").unwrap();
    store.prepare("b", "SELECT 2").unwrap();
    store.prepare("c", "SELECT 3").unwrap();
    assert_eq!(store.count(), 3);
    store.clear();
    assert_eq!(store.count(), 0);
}

#[test]
fn prepared_store_names() {
    let mut store = crate::vm::prepared::PreparedStore::new();
    store.prepare("alpha", "SELECT 1").unwrap();
    store.prepare("beta", "SELECT 2").unwrap();
    let mut names = store.names();
    names.sort();
    assert_eq!(names, vec!["alpha", "beta"]);
}

// ════════════════════════════════════════════════════════════════════════════════
//  2. BloomFilter unit tests
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn bloom_filter_basic_insert_contain() {
    let mut bf = crate::storage::bloom::BloomFilter::new(128, 7);
    bf.insert(b"hello");
    bf.insert(b"world");
    assert!(bf.may_contain(b"hello"));
    assert!(bf.may_contain(b"world"));
    assert_eq!(bf.item_count(), 2);
}

#[test]
fn bloom_filter_for_capacity() {
    let bf = crate::storage::bloom::BloomFilter::for_capacity(1000);
    assert!(bf.size_bytes() > 0);
}

#[test]
fn bloom_filter_serialize_roundtrip() {
    let mut bf = crate::storage::bloom::BloomFilter::new(32, 5);
    bf.insert(b"key1");
    bf.insert(b"key2");
    let bytes = bf.to_bytes();
    let bf2 = crate::storage::bloom::BloomFilter::from_bytes(&bytes).unwrap();
    assert!(bf2.may_contain(b"key1"));
    assert!(bf2.may_contain(b"key2"));
    assert_eq!(bf.size_bytes(), bf2.size_bytes());
}

#[test]
fn bloom_filter_from_bytes_too_short() {
    assert!(crate::storage::bloom::BloomFilter::from_bytes(&[0u8; 5]).is_none());
}

#[test]
fn bloom_filter_merge() {
    let mut bf1 = crate::storage::bloom::BloomFilter::new(16, 3);
    let mut bf2 = crate::storage::bloom::BloomFilter::new(16, 3);
    bf1.insert(b"a");
    bf2.insert(b"b");
    assert!(bf1.merge(&bf2));
    assert!(bf1.may_contain(b"a"));
    assert!(bf1.may_contain(b"b"));
}

#[test]
fn bloom_filter_merge_size_mismatch() {
    let mut bf1 = crate::storage::bloom::BloomFilter::new(16, 3);
    let bf2 = crate::storage::bloom::BloomFilter::new(32, 3);
    assert!(!bf1.merge(&bf2)); // different sizes → false
}

#[test]
fn bloom_filter_fill_ratio() {
    let mut bf = crate::storage::bloom::BloomFilter::new(8, 2);
    let r0 = bf.fill_ratio();
    assert!((r0 - 0.0).abs() < f64::EPSILON);
    bf.insert(b"x");
    let r1 = bf.fill_ratio();
    assert!(r1 > 0.0);
}

#[test]
fn bloom_filter_estimated_fpr() {
    let bf = crate::storage::bloom::BloomFilter::for_capacity(100);
    let fpr = bf.estimated_fpr();
    assert!((0.0..=1.0).contains(&fpr));
}

#[test]
fn bloom_filter_clear() {
    let mut bf = crate::storage::bloom::BloomFilter::new(16, 3);
    bf.insert(b"data");
    assert!(bf.item_count() > 0);
    bf.clear();
    assert_eq!(bf.item_count(), 0);
    assert!((bf.fill_ratio() - 0.0).abs() < f64::EPSILON);
}

// ════════════════════════════════════════════════════════════════════════════════
//  3. WaitForGraph deadlock detection
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn wait_for_graph_no_deadlock() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    wfg.add_wait(1, 2); // txn 1 waits for txn 2
    wfg.add_wait(2, 3); // txn 2 waits for txn 3
                        // Checking if adding edge 3→99 would create a cycle: no
    assert!(wfg.detect_deadlock(3, 99).is_none());
}

#[test]
fn wait_for_graph_simple_cycle() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    wfg.add_wait(1, 2);
    wfg.add_wait(2, 3);
    // Adding 3→1 would create cycle: 3 → 1 → 2 → 3
    let cycle = wfg.detect_deadlock(3, 1);
    assert!(cycle.is_some());
    let c = cycle.unwrap();
    assert!(c.contains(&1));
    assert!(c.contains(&2));
    assert!(c.contains(&3));
}

#[test]
fn wait_for_graph_self_cycle() {
    let wfg = crate::vm::mvcc::WaitForGraph::new();
    // Adding 1→1 would be self cycle
    let cycle = wfg.detect_deadlock(1, 1);
    assert!(cycle.is_some());
}

#[test]
fn wait_for_graph_remove_wait() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    wfg.add_wait(1, 2);
    wfg.add_wait(2, 3);
    // 3→1 would deadlock
    assert!(wfg.detect_deadlock(3, 1).is_some());
    // Remove edge 2→3
    wfg.remove_wait(2);
    // Now 3→1 should not deadlock because chain is broken
    assert!(wfg.detect_deadlock(3, 1).is_none());
}

#[test]
fn wait_for_graph_remove_transaction() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    wfg.add_wait(1, 2);
    wfg.add_wait(2, 3);
    wfg.remove_transaction(2); // removes edge 2→3 and any edge pointing to 2
    assert_eq!(wfg.edge_count(), 0); // 1→2 also removed because holder=2 removed
}

#[test]
fn wait_for_graph_edge_count() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    assert_eq!(wfg.edge_count(), 0);
    wfg.add_wait(1, 2);
    wfg.add_wait(2, 3);
    assert_eq!(wfg.edge_count(), 2);
    wfg.remove_wait(1);
    assert_eq!(wfg.edge_count(), 1);
}

#[test]
fn wait_for_graph_edges() {
    let mut wfg = crate::vm::mvcc::WaitForGraph::new();
    wfg.add_wait(10, 20);
    wfg.add_wait(20, 30);
    let mut edges = wfg.edges();
    edges.sort();
    assert_eq!(edges, vec![(10, 20), (20, 30)]);
}

// ════════════════════════════════════════════════════════════════════════════════
//  4. TransactionTimeoutManager
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn txn_timeout_basic() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_millis(50));
    mgr.begin(1);
    assert!(!mgr.is_timed_out(1));
    assert_eq!(mgr.active_count(), 1);
}

#[test]
fn txn_timeout_elapsed() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_secs(60));
    mgr.begin(42);
    let e = mgr.elapsed(42);
    assert!(e.is_some());
    assert!(e.unwrap() < std::time::Duration::from_secs(5));
    assert!(mgr.elapsed(999).is_none());
}

#[test]
fn txn_timeout_end() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_secs(60));
    mgr.begin(1);
    mgr.begin(2);
    assert_eq!(mgr.active_count(), 2);
    mgr.end(1);
    assert_eq!(mgr.active_count(), 1);
    assert!(mgr.elapsed(1).is_none());
}

#[test]
fn txn_timeout_custom_per_txn() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_secs(3600));
    mgr.begin(1);
    // Set a very short custom timeout
    mgr.set_timeout(1, std::time::Duration::from_millis(1));
    std::thread::sleep(std::time::Duration::from_millis(5));
    assert!(mgr.is_timed_out(1));
}

#[test]
fn txn_timeout_timed_out_list() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_millis(1));
    mgr.begin(1);
    mgr.begin(2);
    std::thread::sleep(std::time::Duration::from_millis(5));
    let timed_out = mgr.timed_out_transactions();
    assert!(timed_out.contains(&1));
    assert!(timed_out.contains(&2));
}

#[test]
fn txn_timeout_default_timeout_accessor() {
    let mgr = crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_secs(42));
    assert_eq!(mgr.default_timeout(), std::time::Duration::from_secs(42));
}

#[test]
fn txn_timeout_set_default() {
    let mut mgr =
        crate::vm::mvcc::TransactionTimeoutManager::new(std::time::Duration::from_secs(10));
    mgr.set_default_timeout(std::time::Duration::from_secs(99));
    assert_eq!(mgr.default_timeout(), std::time::Duration::from_secs(99));
}

// ════════════════════════════════════════════════════════════════════════════════
//  5. PREPARE / EXECUTE / DEALLOCATE SQL integration
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn sql_prepare_execute_select() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'Alice')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'Bob')").unwrap();
    vm.execute_sql("PREPARE get_all AS SELECT * FROM t ORDER BY id")
        .unwrap();
    let r = vm.execute_sql("EXECUTE get_all").unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 2);
}

#[test]
fn sql_prepare_duplicate_error() {
    let mut vm = vm();
    vm.execute_sql("PREPARE s1 AS SELECT 1").unwrap();
    let err = vm.execute_sql("PREPARE s1 AS SELECT 2");
    assert!(err.is_err());
}

#[test]
fn sql_deallocate_not_found() {
    let mut vm = vm();
    let err = vm.execute_sql("DEALLOCATE nonexistent");
    assert!(err.is_err());
}

#[test]
fn sql_deallocate_prepared() {
    let mut vm = vm();
    vm.execute_sql("PREPARE s1 AS SELECT 1").unwrap();
    vm.execute_sql("DEALLOCATE s1").unwrap();
    // Now executing should fail
    let err = vm.execute_sql("EXECUTE s1");
    assert!(err.is_err());
}

#[test]
fn sql_deallocate_all() {
    let mut vm = vm();
    vm.execute_sql("PREPARE a1 AS SELECT 1").unwrap();
    vm.execute_sql("PREPARE a2 AS SELECT 2").unwrap();
    let r = vm.execute_sql("DEALLOCATE ALL").unwrap();
    match &r {
        ExecResult::Ok { message } => {
            assert!(message.contains("2 statements"));
        }
        _ => panic!("expected Ok"),
    }
}

#[test]
fn sql_prepare_insert_and_query() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("PREPARE ins AS INSERT INTO users VALUES (1, 'Alice')")
        .unwrap();
    vm.execute_sql("PREPARE sel AS SELECT * FROM users")
        .unwrap();
    vm.execute_sql("EXECUTE ins").unwrap();
    let r = vm.execute_sql("EXECUTE sel").unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 1);
    assert_eq!(vm.prepared_store.count(), 2);
}

// ════════════════════════════════════════════════════════════════════════════════
//  6. Window function RANGE frame tests
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn window_range_unbounded_preceding_current_row() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE sales (dept TEXT, amount INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('A', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('A', 20)")
        .unwrap();
    vm.execute_sql("INSERT INTO sales VALUES ('A', 30)")
        .unwrap();
    // RANGE: peers with same ORDER BY key share the same frame boundary
    let r = vm.execute_sql(
        "SELECT amount, SUM(amount) OVER (ORDER BY amount RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) as running FROM sales"
    ).unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 4);
    // Row 1 (amount=10): sum of [10] = 10
    // Row 2 (amount=20): sum of [10,20,20] = 50 (peers included)
    // Row 3 (amount=20): sum of [10,20,20] = 50 (same as row 2)
    // Row 4 (amount=30): sum of [10,20,20,30] = 80
    assert_eq!(rs[0][1], Value::Integer(10));
    assert_eq!(rs[1][1], Value::Integer(50));
    assert_eq!(rs[2][1], Value::Integer(50));
    assert_eq!(rs[3][1], Value::Integer(80));
}

#[test]
fn window_range_current_row_unbounded_following() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE t (v INTEGER)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (3)").unwrap();
    let r = vm.execute_sql(
        "SELECT v, SUM(v) OVER (ORDER BY v RANGE BETWEEN CURRENT ROW AND UNBOUNDED FOLLOWING) as s FROM t"
    ).unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 4);
    // v=1: sum [1,2,2,3] = 8
    // v=2: sum [2,2,3] = 7
    // v=2: sum [2,2,3] = 7
    // v=3: sum [3] = 3
    assert_eq!(rs[0][1], Value::Integer(8));
    assert_eq!(rs[1][1], Value::Integer(7));
    assert_eq!(rs[2][1], Value::Integer(7));
    assert_eq!(rs[3][1], Value::Integer(3));
}

// ════════════════════════════════════════════════════════════════════════════════
//  7. INSERT OR REPLACE / INSERT OR IGNORE (UPSERT)
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn upsert_insert_or_replace() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO kv VALUES (1, 'old')").unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO kv VALUES (1, 'new')")
        .unwrap();
    let r = vm.execute_sql("SELECT v FROM kv WHERE k = 1").unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0][0], Value::Text("new".into()));
}

#[test]
fn upsert_insert_or_ignore() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE kv (k INTEGER PRIMARY KEY, v TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO kv VALUES (1, 'first')")
        .unwrap();
    vm.execute_sql("INSERT OR IGNORE INTO kv VALUES (1, 'second')")
        .unwrap();
    let r = vm.execute_sql("SELECT v FROM kv WHERE k = 1").unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0][0], Value::Text("first".into()));
}

#[test]
fn upsert_insert_or_replace_multiple() {
    let mut vm = vm();
    vm.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, qty INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO items VALUES (1, 'apple', 10)")
        .unwrap();
    vm.execute_sql("INSERT INTO items VALUES (2, 'banana', 5)")
        .unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO items VALUES (1, 'apple', 15)")
        .unwrap();
    vm.execute_sql("INSERT OR REPLACE INTO items VALUES (3, 'cherry', 7)")
        .unwrap();
    let r = vm
        .execute_sql("SELECT id, name, qty FROM items ORDER BY id")
        .unwrap();
    let rs = rows(&r);
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0][2], Value::Integer(15)); // updated
    assert_eq!(rs[2][1], Value::Text("cherry".into())); // new
}

// ════════════════════════════════════════════════════════════════════════════════
//  8. VM field initialization
// ════════════════════════════════════════════════════════════════════════════════

#[test]
fn vm_has_prepared_store() {
    let vm = vm();
    assert_eq!(vm.prepared_store.count(), 0);
}

#[test]
fn vm_has_wait_for_graph() {
    let vm = vm();
    assert_eq!(vm.wait_for_graph.edge_count(), 0);
}

#[test]
fn vm_has_txn_timeout_mgr() {
    let vm = vm();
    assert_eq!(vm.txn_timeout_mgr.active_count(), 0);
    assert_eq!(
        vm.txn_timeout_mgr.default_timeout(),
        std::time::Duration::from_secs(30)
    );
}
