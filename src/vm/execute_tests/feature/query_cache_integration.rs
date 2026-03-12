//! Integration tests for Query Cache functionality:
//! - Cache hit/miss via VM.execute_sql
//! - Auto-invalidation on DML
//! - Cache disabled during transactions
//! - SHOW ENGINE STATUS reports cache stats

use super::*;

#[test]
fn test_query_cache_hit() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 'alice')").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 'bob')").unwrap();

    // First SELECT — cache miss
    let rows1 = query_rows(&mut vm, "SELECT * FROM t ORDER BY id");
    assert_eq!(rows1.len(), 2);
    assert_eq!(vm.query_cache.stat_misses, 1);
    assert_eq!(vm.query_cache.stat_hits, 0);

    // Second identical SELECT — cache hit
    let rows2 = query_rows(&mut vm, "SELECT * FROM t ORDER BY id");
    assert_eq!(rows2, rows1);
    assert_eq!(vm.query_cache.stat_hits, 1);
}

#[test]
fn test_query_cache_invalidation_on_insert() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();

    // Cache SELECT
    let rows1 = query_rows(&mut vm, "SELECT v FROM t");
    assert_eq!(rows1.len(), 1);

    // INSERT invalidates cache
    vm.execute_sql("INSERT INTO t VALUES (2, 200)").unwrap();

    // Next SELECT should be a cache miss (and return 2 rows)
    let rows2 = query_rows(&mut vm, "SELECT v FROM t");
    assert_eq!(rows2.len(), 2);
    assert!(vm.query_cache.stat_invalidations >= 1);
}

#[test]
fn test_query_cache_invalidation_on_update() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();

    let rows1 = query_rows(&mut vm, "SELECT v FROM t");
    assert_eq!(rows1[0][0], Value::Integer(100));

    vm.execute_sql("UPDATE t SET v = 999 WHERE id = 1").unwrap();

    let rows2 = query_rows(&mut vm, "SELECT v FROM t");
    assert_eq!(rows2[0][0], Value::Integer(999));
}

#[test]
fn test_query_cache_invalidation_on_delete() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();
    vm.execute_sql("INSERT INTO t VALUES (2, 200)").unwrap();

    let rows1 = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows1.len(), 2);

    vm.execute_sql("DELETE FROM t WHERE id = 1").unwrap();

    let rows2 = query_rows(&mut vm, "SELECT * FROM t");
    assert_eq!(rows2.len(), 1);
}

#[test]
fn test_query_cache_disabled_in_transaction() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();

    // Cache a result
    query_rows(&mut vm, "SELECT * FROM t");
    let hits_before = vm.query_cache.stat_hits;

    // Inside a transaction, cache should NOT be used
    vm.execute_sql("BEGIN").unwrap();
    query_rows(&mut vm, "SELECT * FROM t");
    // Cache should not have been hit (snapshot active → cache bypassed)
    assert_eq!(vm.query_cache.stat_hits, hits_before);
    vm.execute_sql("COMMIT").unwrap();
}

#[test]
fn test_query_cache_different_queries_different_entries() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY, v INTEGER)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1, 100)").unwrap();

    query_rows(&mut vm, "SELECT * FROM t");
    query_rows(&mut vm, "SELECT v FROM t");
    assert_eq!(vm.query_cache.len(), 2);
}

#[test]
fn test_query_cache_show_engine_status() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE t (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO t VALUES (1)").unwrap();
    query_rows(&mut vm, "SELECT * FROM t"); // miss
    query_rows(&mut vm, "SELECT * FROM t"); // hit

    match vm.execute_sql("SHOW ENGINE STATUS").unwrap() {
        ExecResult::Explain { plan } => {
            assert!(plan.contains("--- Query Cache ---"));
            assert!(plan.contains("Cache enabled      : true"));
            assert!(plan.contains("Cache hits         : 1"));
        }
        _ => panic!("expected Explain"),
    }
}

#[test]
fn test_query_cache_cross_table_invalidation() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE a (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("CREATE TABLE b (id INTEGER PRIMARY KEY)")
        .unwrap();
    vm.execute_sql("INSERT INTO a VALUES (1)").unwrap();
    vm.execute_sql("INSERT INTO b VALUES (2)").unwrap();

    query_rows(&mut vm, "SELECT * FROM a");
    query_rows(&mut vm, "SELECT * FROM b");
    assert_eq!(vm.query_cache.len(), 2);

    // INSERT into a should only invalidate a's cache
    vm.execute_sql("INSERT INTO a VALUES (3)").unwrap();
    assert_eq!(vm.query_cache.len(), 1); // Only b's cache remains
}
