// O2 Cost-Based Optimizer + O3 Adaptive Index integration tests
use kkdb::types::Value;
use kkdb::vm::execute::{ExecResult, VM};
use std::fs;

fn setup(name: &str) -> VM {
    let _ = fs::remove_dir_all(name);
    VM::open(name).unwrap()
}

fn rows(r: ExecResult) -> Vec<Vec<Value>> {
    match r { ExecResult::QueryResult { rows, .. } => rows, _ => panic!("not query result") }
}

// ── O2: CBO uses ANALYZE TABLE stats to choose index vs seq scan ──────────

#[test]
fn test_o2_analyze_enables_cbo() {
    let mut vm = setup("test_o2_cbo");
    vm.execute_sql("CREATE TABLE nums (id INTEGER PRIMARY KEY, val INTEGER);").unwrap();
    // Insert 100 rows
    for i in 1..=100i64 {
        vm.execute_sql(&format!("INSERT INTO nums VALUES ({}, {});", i, i * 10)).unwrap();
    }
    vm.execute_sql("CREATE INDEX idx_nums_val ON nums (val);").unwrap();
    vm.execute_sql("ANALYZE TABLE nums;").unwrap();
    // Query with high selectivity (1 out of 100) — CBO should choose index
    let r = rows(vm.execute_sql("SELECT id FROM nums WHERE val = 500;").unwrap());
    assert_eq!(r.len(), 1, "exact match row");
    assert_eq!(r[0][0], Value::Integer(50));
}

#[test]
fn test_o2_cbo_prefers_seq_scan_for_large_range() {
    let mut vm = setup("test_o2_seqscan");
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER);").unwrap();
    for i in 1..=20i64 {
        vm.execute_sql(&format!("INSERT INTO t VALUES ({}, {});", i, i)).unwrap();
    }
    vm.execute_sql("CREATE INDEX idx_t_x ON t (x);").unwrap();
    vm.execute_sql("ANALYZE TABLE t;").unwrap();
    // Query that touches >50% of rows — CBO should prefer seq scan (returns correct result either way)
    let r = rows(vm.execute_sql("SELECT COUNT(*) FROM t WHERE x > 5;").unwrap());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Integer(15));
}

#[test]
fn test_o2_without_stats_still_uses_index() {
    let mut vm = setup("test_o2_no_stats");
    vm.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY, code TEXT);").unwrap();
    vm.execute_sql("INSERT INTO items VALUES (1, 'ABC');").unwrap();
    vm.execute_sql("INSERT INTO items VALUES (2, 'DEF');").unwrap();
    vm.execute_sql("CREATE INDEX idx_items_code ON items (code);").unwrap();
    // No ANALYZE — CBO falls back to always using index when available
    let r = rows(vm.execute_sql("SELECT id FROM items WHERE code = 'ABC';").unwrap());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0][0], Value::Integer(1));
}

// ── O3: Adaptive index decisions ─────────────────────────────────────────

#[test]
fn test_o3_adaptive_index_auto_created() {
    let mut vm = setup("test_o3_adaptive");
    vm.execute_sql("CREATE TABLE events (id INTEGER PRIMARY KEY, category TEXT);").unwrap();
    for i in 1..=10 {
        vm.execute_sql(&format!("INSERT INTO events VALUES ({}, 'cat_{}')", i, i % 3)).unwrap();
    }
    // Set threshold to 3 for fast testing
    vm.adaptive_threshold = 3;
    // Run 3 full-scan WHERE queries on `category` (no index yet) — hits threshold at query #3
    for _ in 0..3 {
        let _ = vm.execute_sql("SELECT id FROM events WHERE category = 'cat_1';").unwrap();
    }
    // Run one more query: execute_sql entry drains the pending auto-index queue
    let _ = vm.execute_sql("SELECT COUNT(*) FROM events;").unwrap();
    // After 3 threshold accesses + 1 drain trigger, O3 should have auto-created idx_events_category_auto
    let index_exists = vm.schema.indexes.contains_key("idx_events_category_auto");
    assert!(index_exists, "O3 should have auto-created index on events.category");
}

#[test]
fn test_o3_no_duplicate_index() {
    let mut vm = setup("test_o3_nodup");
    vm.execute_sql("CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT);").unwrap();
    vm.execute_sql("CREATE INDEX idx_products_name ON products (name);").unwrap();
    vm.execute_sql("INSERT INTO products VALUES (1, 'Widget');").unwrap();
    vm.adaptive_threshold = 1;
    // Even after many queries, no auto-index should be duplicated
    for _ in 0..5 {
        let _ = vm.execute_sql("SELECT id FROM products WHERE name = 'Widget';").unwrap();
    }
    // Only the original index should exist
    let auto_idx = vm.schema.indexes.contains_key("idx_products_name_auto");
    assert!(!auto_idx, "Should NOT create auto-index when manual index already exists");
}

#[test]
fn test_o3_counter_tracks_per_column() {
    let mut vm = setup("test_o3_counter");
    vm.execute_sql("CREATE TABLE log (id INTEGER PRIMARY KEY, level TEXT, msg TEXT);").unwrap();
    vm.adaptive_threshold = 100; // High threshold so auto-create doesn't fire
    vm.execute_sql("INSERT INTO log VALUES (1, 'INFO', 'hello');").unwrap();
    // Access `level` column 3 times
    for _ in 0..3 {
        let _ = vm.execute_sql("SELECT id FROM log WHERE level = 'INFO';").unwrap();
    }
    // Access `msg` column only 1 time
    let _ = vm.execute_sql("SELECT id FROM log WHERE msg = 'hello';").unwrap();
    let level_count = vm.query_access_counter
        .get(&("log".to_string(), "level".to_string())).copied().unwrap_or(0);
    let msg_count = vm.query_access_counter
        .get(&("log".to_string(), "msg".to_string())).copied().unwrap_or(0);
    assert_eq!(level_count, 3, "level column accessed 3 times");
    assert_eq!(msg_count, 1, "msg column accessed 1 time");
}
