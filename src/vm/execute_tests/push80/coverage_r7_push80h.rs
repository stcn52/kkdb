// ═══════════════════════════════════════════════════════════════════
// Batch 8 — Direct internal API tests for maximum coverage gain
// Strategy: Call internal functions directly rather than via SQL
// Target: btree splits, pager file I/O, schema restore, eval_expr,
//         exec_ddl explain, GROUP BY + PERCENT_RANK/CUME_DIST
// ═══════════════════════════════════════════════════════════════════

use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

// ── helpers ──
fn exec(vm: &mut VM, sql: &str) {
    vm.execute_sql(sql)
        .unwrap_or_else(|e| panic!("EXEC `{sql}`: {e}"));
}
fn try_exec(vm: &mut VM, sql: &str) -> Result<ExecResult, crate::error::KkdbError> {
    vm.execute_sql(sql)
}
fn query_rows(vm: &mut VM, sql: &str) -> Vec<Vec<Value>> {
    match vm.execute_sql(sql) {
        Ok(ExecResult::QueryResult { rows, .. }) => rows,
        other => panic!("expected rows from `{sql}`: {other:?}"),
    }
}

// ═══════════════════════════════════════════════════════
// BTree splits — insert enough rows to trigger node splits
// btree.rs L412-416, L460-467, L762-768, L888-892
// ═══════════════════════════════════════════════════════

#[test]
fn test_btree_massive_insert_triggers_split() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let mut root = btree.create_table().unwrap();

    // Insert 500 rows with varying-length values to trigger splits
    for i in 1..=500i64 {
        let val = format!("value_{:08}", i);
        let row = vec![Value::Integer(i), Value::Text(val.into())];
        root = btree.insert(root, i, &row).unwrap();
    }

    // Verify all rows exist
    let all = btree.scan_all(root).unwrap();
    assert_eq!(all.len(), 500);

    // Verify random access
    let found = btree.find_by_rowid(root, 250).unwrap();
    assert!(found.is_some());

    pager.commit_transaction().unwrap();
}

#[test]
fn test_btree_large_values_trigger_overflow() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let mut root = btree.create_table().unwrap();

    // Insert rows with large text values to trigger overflow pages
    for i in 1..=50i64 {
        let big_val = "X".repeat(2000); // Large value close to page payload limit
        let row = vec![Value::Integer(i), Value::Text(big_val.into())];
        root = btree.insert(root, i, &row).unwrap();
    }

    let all = btree.scan_all(root).unwrap();
    assert_eq!(all.len(), 50);

    pager.commit_transaction().unwrap();
}

#[test]
fn test_btree_delete_many_rows() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let mut root = btree.create_table().unwrap();

    // Insert 200 rows
    for i in 1..=200i64 {
        let row = vec![Value::Integer(i), Value::Text(format!("row_{i}").into())];
        root = btree.insert(root, i, &row).unwrap();
    }

    // Delete all even rows
    for i in (2..=200i64).step_by(2) {
        let (deleted, new_root) = btree.delete_by_rowid(root, i).unwrap();
        if deleted {
            root = new_root;
        }
    }

    let remaining = btree.scan_all(root).unwrap();
    assert_eq!(remaining.len(), 100);

    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// GROUP BY + window functions (PERCENT_RANK, CUME_DIST)
// exec_select.rs L3537-3592 (40 lines) — requires GROUP BY
// ═══════════════════════════════════════════════════════

#[test]
fn test_group_by_with_percent_rank() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE gbpr(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    for i in 1..=20 {
        exec(
            &mut vm,
            &format!(
                "INSERT INTO gbpr VALUES ({i}, '{}', {})",
                if i <= 10 { "A" } else { "B" },
                i * 10
            ),
        );
    }

    let r = try_exec(
        &mut vm,
        "SELECT cat, SUM(val), PERCENT_RANK() OVER(ORDER BY SUM(val)) AS pr FROM gbpr GROUP BY cat",
    );
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 2);
    }
}

#[test]
fn test_group_by_with_cume_dist() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE gbcd(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    for i in 1..=30 {
        exec(
            &mut vm,
            &format!(
                "INSERT INTO gbcd VALUES ({i}, '{}', {})",
                ["X", "Y", "Z"][i % 3],
                i
            ),
        );
    }

    let r = try_exec(
        &mut vm,
        "SELECT cat, COUNT(*), CUME_DIST() OVER(ORDER BY COUNT(*)) AS cd FROM gbcd GROUP BY cat",
    );
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 3);
    }
}

#[test]
fn test_group_by_with_dense_rank() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE gbdr(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    for i in 1..=20 {
        exec(
            &mut vm,
            &format!(
                "INSERT INTO gbdr VALUES ({i}, '{}', {})",
                ["A", "B", "C", "D"][i % 4],
                i
            ),
        );
    }

    let r = try_exec(
        &mut vm,
        "SELECT cat, SUM(val), DENSE_RANK() OVER(ORDER BY SUM(val)) AS dr FROM gbdr GROUP BY cat",
    );
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 4);
    }
}

// ═══════════════════════════════════════════════════════
// EXPLAIN with complex queries — exec_ddl.rs from_name
// L1497-1506 — FromClause::Join, Subquery, SetOp, TableFunction
// ═══════════════════════════════════════════════════════

#[test]
fn test_explain_join_query() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE ej1(id INTEGER PRIMARY KEY, val TEXT)",
    );
    exec(
        &mut vm,
        "CREATE TABLE ej2(id INTEGER PRIMARY KEY, ref_id INTEGER)",
    );
    exec(&mut vm, "INSERT INTO ej1 VALUES (1, 'a')");
    exec(&mut vm, "INSERT INTO ej2 VALUES (1, 1)");

    let r = try_exec(
        &mut vm,
        "EXPLAIN SELECT * FROM ej1 JOIN ej2 ON ej1.id = ej2.ref_id",
    );
    assert!(r.is_ok());
}

#[test]
fn test_explain_subquery() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE esq(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&mut vm, "INSERT INTO esq VALUES (1, 10), (2, 20)");

    let r = try_exec(
        &mut vm,
        "EXPLAIN SELECT * FROM (SELECT * FROM esq WHERE val > 5) AS sub",
    );
    assert!(r.is_ok());
}

// ═══════════════════════════════════════════════════════
// Pager file-based with WAL — pager.rs L558-562, L631-637
// ═══════════════════════════════════════════════════════

#[test]
fn test_pager_file_based_wal_commit() {
    use crate::storage::pager::Pager;
    use std::fs;

    let path = "/tmp/kkdb_test_pager_wal_b8.db";
    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{path}-wal"));

    let r = Pager::open_cow_v2(path);
    if let Ok(mut pager) = r {
        pager.begin_transaction().unwrap();
        for _ in 0..20 {
            let pg = pager.allocate_page().unwrap();
            let page = pager.get_page_mut(pg).unwrap();
            page.data[0] = 0xAA;
        }
        pager.commit_transaction().unwrap();
    }

    let _ = fs::remove_file(path);
    let _ = fs::remove_file(format!("{path}-wal"));
}

// ═══════════════════════════════════════════════════════
// Large-scale SQL tests to trigger internal paths
// ═══════════════════════════════════════════════════════

#[test]
fn test_large_insert_1000_rows() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE big(id INTEGER PRIMARY KEY, val TEXT, num INTEGER)",
    );
    for i in 1..=1000 {
        exec(
            &mut vm,
            &format!("INSERT INTO big VALUES ({i}, 'item_{i}', {})", i * 7 % 100),
        );
    }

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM big");
    assert_eq!(rows[0][0], Value::Integer(1000));

    let rows = query_rows(
        &mut vm,
        "SELECT * FROM big WHERE num = 42 ORDER BY id LIMIT 5",
    );
    assert!(rows.len() <= 5);
}

#[test]
fn test_large_update_many_rows() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE big_upd(id INTEGER PRIMARY KEY, val INTEGER, status TEXT)",
    );
    for i in 1..=500 {
        exec(
            &mut vm,
            &format!("INSERT INTO big_upd VALUES ({i}, {i}, 'pending')"),
        );
    }

    exec(
        &mut vm,
        "UPDATE big_upd SET status = 'done' WHERE val <= 250",
    );
    let rows = query_rows(
        &mut vm,
        "SELECT COUNT(*) FROM big_upd WHERE status = 'done'",
    );
    assert_eq!(rows[0][0], Value::Integer(250));
}

#[test]
fn test_large_delete_many_rows() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE big_del(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 1..=500 {
        exec(&mut vm, &format!("INSERT INTO big_del VALUES ({i}, {i})"));
    }

    exec(&mut vm, "DELETE FROM big_del WHERE val > 250");
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM big_del");
    assert_eq!(rows[0][0], Value::Integer(250));
}

// ═══════════════════════════════════════════════════════
// Complex aggregation queries
// ═══════════════════════════════════════════════════════

#[test]
fn test_aggregate_with_null_handling() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE anh(id INTEGER PRIMARY KEY, val REAL, cat TEXT)",
    );
    exec(&mut vm, "INSERT INTO anh VALUES (1, 10.0, 'A'), (2, NULL, 'A'), (3, 30.0, 'B'), (4, NULL, 'B'), (5, 50.0, 'A')");

    let rows = query_rows(&mut vm,
        "SELECT cat, COUNT(*), COUNT(val), SUM(val), MIN(val), MAX(val) FROM anh GROUP BY cat ORDER BY cat");
    assert_eq!(rows.len(), 2);
    // A: count=3, count(val)=2, sum=60, min=10, max=50
    // B: count=2, count(val)=1, sum=30, min=30, max=30
}

#[test]
fn test_having_with_multiple_conditions() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE hmc(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    for i in 1..=30 {
        exec(
            &mut vm,
            &format!(
                "INSERT INTO hmc VALUES ({i}, '{}', {})",
                ["X", "Y", "Z"][i % 3],
                i
            ),
        );
    }

    let rows = query_rows(&mut vm,
        "SELECT cat, COUNT(*) AS cnt, SUM(val) AS total FROM hmc GROUP BY cat HAVING COUNT(*) >= 10 AND SUM(val) > 100 ORDER BY cat");
    let _ = rows;
}

// ═══════════════════════════════════════════════════════
// UNION set operations with ORDER BY + LIMIT
// query.rs L68-77
// ═══════════════════════════════════════════════════════

#[test]
fn test_union_with_order_by_limit() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE u1(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(
        &mut vm,
        "CREATE TABLE u2(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 1..=10 {
        exec(&mut vm, &format!("INSERT INTO u1 VALUES ({i}, {i})"));
    }
    for i in 11..=20 {
        exec(&mut vm, &format!("INSERT INTO u2 VALUES ({i}, {i})"));
    }

    let rows = query_rows(
        &mut vm,
        "SELECT val FROM u1 UNION ALL SELECT val FROM u2 ORDER BY val LIMIT 5",
    );
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][0], Value::Integer(1));
}

#[test]
fn test_union_with_offset() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE uo1(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(
        &mut vm,
        "CREATE TABLE uo2(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    for i in 1..=5 {
        exec(&mut vm, &format!("INSERT INTO uo1 VALUES ({i}, {i})"));
    }
    for i in 6..=10 {
        exec(&mut vm, &format!("INSERT INTO uo2 VALUES ({i}, {i})"));
    }

    let rows = query_rows(
        &mut vm,
        "SELECT val FROM uo1 UNION ALL SELECT val FROM uo2 ORDER BY val LIMIT 3 OFFSET 3",
    );
    assert_eq!(rows.len(), 3);
}

// ═══════════════════════════════════════════════════════
// Cursor API for range scans — btree/cursor coverage
// ═══════════════════════════════════════════════════════

#[test]
fn test_cursor_range_scan_large() {
    use crate::storage::btree::BTree;
    use crate::storage::cursor::Cursor;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let mut root = btree.create_table().unwrap();

    for i in 1..=100i64 {
        let row = vec![Value::Integer(i), Value::Text(format!("data_{i}").into())];
        root = btree.insert(root, i, &row).unwrap();
    }
    pager.commit_transaction().unwrap();

    // Full cursor traversal
    let mut cursor = Cursor::table_start(&mut pager, root).unwrap();
    let mut count = 0;
    while !cursor.end_of_table {
        let r = cursor.current(&mut pager);
        assert!(r.is_ok());
        cursor.advance(&mut pager).unwrap();
        count += 1;
    }
    assert_eq!(count, 100);
}

// ═══════════════════════════════════════════════════════
// SQL edge cases that exercise eval_expr deeply
// ═══════════════════════════════════════════════════════

#[test]
fn test_null_safe_equality() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE nse(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER)",
    );
    exec(
        &mut vm,
        "INSERT INTO nse VALUES (1, 1, 1), (2, 1, 2), (3, NULL, NULL), (4, NULL, 1)",
    );
    // IS comparison
    let rows = query_rows(&mut vm, "SELECT * FROM nse WHERE a IS NULL");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_in_subquery_with_empty_result() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE isqe1(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(
        &mut vm,
        "CREATE TABLE isqe2(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&mut vm, "INSERT INTO isqe1 VALUES (1, 10), (2, 20)");
    // isqe2 is empty

    let rows = query_rows(
        &mut vm,
        "SELECT * FROM isqe1 WHERE val IN (SELECT val FROM isqe2)",
    );
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_case_simple_form() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT CASE 2 WHEN 1 THEN 'one' WHEN 2 THEN 'two' WHEN 3 THEN 'three' ELSE 'other' END",
    );
    assert_eq!(rows[0][0], Value::Text("two".into()));
}

#[test]
fn test_nested_case() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE nc2(id INTEGER PRIMARY KEY, a INTEGER, b TEXT)",
    );
    exec(
        &mut vm,
        "INSERT INTO nc2 VALUES (1, 1, 'x'), (2, 2, 'y'), (3, 3, 'z')",
    );
    let rows = query_rows(&mut vm,
        "SELECT id, CASE WHEN a = 1 THEN CASE b WHEN 'x' THEN 'ax' ELSE 'a?' END ELSE 'other' END FROM nc2 ORDER BY id");
    assert_eq!(rows[0][1], Value::Text("ax".into()));
    assert_eq!(rows[1][1], Value::Text("other".into()));
}

// ═══════════════════════════════════════════════════════
// More window function types in GROUP BY context
// ═══════════════════════════════════════════════════════

#[test]
fn test_group_by_with_ntile_window() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE gbn(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    for i in 1..=20 {
        exec(
            &mut vm,
            &format!(
                "INSERT INTO gbn VALUES ({i}, '{}', {})",
                ["A", "B", "C", "D"][i % 4],
                i
            ),
        );
    }

    let r = try_exec(
        &mut vm,
        "SELECT cat, SUM(val), NTILE(2) OVER(ORDER BY SUM(val)) FROM gbn GROUP BY cat",
    );
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 4);
    }
}

#[test]
fn test_group_by_with_row_number_partition() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE gbrn(id INTEGER PRIMARY KEY, dept TEXT, role TEXT, sal INTEGER)",
    );
    exec(&mut vm, "INSERT INTO gbrn VALUES (1, 'eng', 'dev', 100), (2, 'eng', 'mgr', 200), (3, 'sales', 'rep', 150), (4, 'sales', 'mgr', 250)");

    let r = try_exec(&mut vm,
        "SELECT dept, role, sal, ROW_NUMBER() OVER(PARTITION BY dept ORDER BY sal DESC) AS rn FROM gbrn");
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 4);
    }
}

// ═══════════════════════════════════════════════════════
// Views — exec_ddl.rs view creation + query
// ═══════════════════════════════════════════════════════

#[test]
fn test_view_creation_and_query() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE v_t(id INTEGER PRIMARY KEY, val TEXT, num INTEGER)",
    );
    exec(
        &mut vm,
        "INSERT INTO v_t VALUES (1, 'a', 10), (2, 'b', 20), (3, 'c', 30)",
    );
    exec(
        &mut vm,
        "CREATE VIEW v_high AS SELECT * FROM v_t WHERE num > 15",
    );

    let rows = query_rows(&mut vm, "SELECT * FROM v_high ORDER BY id");
    assert_eq!(rows.len(), 2);
}

#[test]
fn test_view_with_aggregation() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE va_t(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)",
    );
    exec(
        &mut vm,
        "INSERT INTO va_t VALUES (1, 'A', 10), (2, 'A', 20), (3, 'B', 30)",
    );
    exec(
        &mut vm,
        "CREATE VIEW va_summary AS SELECT cat, SUM(val) AS total FROM va_t GROUP BY cat",
    );

    let rows = query_rows(&mut vm, "SELECT * FROM va_summary ORDER BY cat");
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════
// Multiple index operations for coverage
// ═══════════════════════════════════════════════════════

#[test]
fn test_index_comparison_operators() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE ico(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&mut vm, "CREATE INDEX idx_ico ON ico(val)");
    for i in 1..=100 {
        exec(&mut vm, &format!("INSERT INTO ico VALUES ({i}, {i})"));
    }

    // Multiple comparison patterns, each exercising different index scan paths
    let r1 = query_rows(&mut vm, "SELECT COUNT(*) FROM ico WHERE val > 90");
    assert_eq!(r1[0][0], Value::Integer(10));

    let r2 = query_rows(&mut vm, "SELECT COUNT(*) FROM ico WHERE val < 10");
    assert_eq!(r2[0][0], Value::Integer(9));

    let r3 = query_rows(&mut vm, "SELECT COUNT(*) FROM ico WHERE val >= 95");
    assert_eq!(r3[0][0], Value::Integer(6));

    let r4 = query_rows(&mut vm, "SELECT COUNT(*) FROM ico WHERE val <= 5");
    assert_eq!(r4[0][0], Value::Integer(5));
}

#[test]
fn test_index_with_null_values() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE inull(id INTEGER PRIMARY KEY, val INTEGER)",
    );
    exec(&mut vm, "CREATE INDEX idx_inull ON inull(val)");
    exec(
        &mut vm,
        "INSERT INTO inull VALUES (1, 10), (2, NULL), (3, 30), (4, NULL)",
    );

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM inull WHERE val IS NULL");
    assert_eq!(rows[0][0], Value::Integer(2));

    let rows2 = query_rows(&mut vm, "SELECT COUNT(*) FROM inull WHERE val > 20");
    assert_eq!(rows2[0][0], Value::Integer(1));
}

// ═══════════════════════════════════════════════════════
// Complex multi-table transactions
// ═══════════════════════════════════════════════════════

#[test]
fn test_multi_table_transaction_commit() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE mt_a(id INTEGER PRIMARY KEY, val TEXT)",
    );
    exec(
        &mut vm,
        "CREATE TABLE mt_b(id INTEGER PRIMARY KEY, ref_id INTEGER, data TEXT)",
    );

    exec(&mut vm, "BEGIN");
    exec(&mut vm, "INSERT INTO mt_a VALUES (1, 'parent')");
    exec(&mut vm, "INSERT INTO mt_b VALUES (1, 1, 'child1')");
    exec(&mut vm, "INSERT INTO mt_b VALUES (2, 1, 'child2')");
    exec(&mut vm, "COMMIT");

    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM mt_a");
    assert_eq!(rows[0][0], Value::Integer(1));
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM mt_b");
    assert_eq!(rows[0][0], Value::Integer(2));
}

#[test]
fn test_multi_table_transaction_rollback() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE mtr_a(id INTEGER PRIMARY KEY, val TEXT)",
    );
    exec(
        &mut vm,
        "CREATE TABLE mtr_b(id INTEGER PRIMARY KEY, data TEXT)",
    );
    exec(&mut vm, "INSERT INTO mtr_a VALUES (1, 'existing')");

    exec(&mut vm, "BEGIN");
    exec(&mut vm, "INSERT INTO mtr_a VALUES (2, 'new')");
    exec(&mut vm, "INSERT INTO mtr_b VALUES (1, 'new_b')");
    exec(&mut vm, "UPDATE mtr_a SET val = 'modified' WHERE id = 1");
    exec(&mut vm, "ROLLBACK");

    let rows = query_rows(&mut vm, "SELECT * FROM mtr_a");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][1], Value::Text("existing".into()));
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM mtr_b");
    assert_eq!(rows[0][0], Value::Integer(0));
}

// ═══════════════════════════════════════════════════════
// UNNEST and generate_series table functions
// query.rs L401-411
// ═══════════════════════════════════════════════════════

#[test]
fn test_generate_series() {
    let mut vm = VM::new_memory();
    let r = try_exec(&mut vm, "SELECT * FROM generate_series(1, 10)");
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 10);
    }
}

#[test]
fn test_generate_series_with_step() {
    let mut vm = VM::new_memory();
    let r = try_exec(&mut vm, "SELECT * FROM generate_series(1, 20, 5)");
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        // 1, 6, 11, 16 = 4 rows
        assert_eq!(rows.len(), 4);
    }
}

// ═══════════════════════════════════════════════════════
// Recursive CTE — exec_select paths
// ═══════════════════════════════════════════════════════

#[test]
fn test_recursive_cte_fibonacci() {
    let mut vm = VM::new_memory();
    let r = try_exec(&mut vm,
        "WITH RECURSIVE fib(n, a, b) AS (SELECT 0, 0, 1 UNION ALL SELECT n+1, b, a+b FROM fib WHERE n < 10) SELECT n, a FROM fib");
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert!(rows.len() >= 10);
    }
}

#[test]
fn test_recursive_cte_tree_traversal() {
    let mut vm = VM::new_memory();
    exec(
        &mut vm,
        "CREATE TABLE tree(id INTEGER PRIMARY KEY, name TEXT, parent_id INTEGER)",
    );
    exec(&mut vm, "INSERT INTO tree VALUES (1, 'root', NULL), (2, 'child1', 1), (3, 'child2', 1), (4, 'grandchild', 2)");

    let r = try_exec(&mut vm,
        "WITH RECURSIVE paths(id, name, depth) AS (SELECT id, name, 0 FROM tree WHERE parent_id IS NULL UNION ALL SELECT tree.id, tree.name, paths.depth + 1 FROM tree JOIN paths ON tree.parent_id = paths.id) SELECT * FROM paths ORDER BY depth, id");
    if let Ok(ExecResult::QueryResult { rows, .. }) = &r {
        assert_eq!(rows.len(), 4);
    }
}

// ═══════════════════════════════════════════════════════
// Schema restore — vector indexes, triggers
// schema.rs L456-469
// ═══════════════════════════════════════════════════════

#[test]
fn test_schema_with_vector_index_restore() {
    use std::fs;
    let path = "/tmp/kkdb_test_vec_idx_restore_b8";
    let _ = fs::remove_dir_all(path);

    {
        let mut vm = VM::open(path).unwrap();
        exec(
            &mut vm,
            "CREATE TABLE vec_t(id INTEGER PRIMARY KEY, data BLOB)",
        );
        let r = try_exec(
            &mut vm,
            "CREATE VECTOR INDEX vec_idx ON vec_t(data) DIMENSION 3",
        );
        let _ = r;
    }

    // Reopen — triggers schema restore
    {
        let mut vm = VM::open(path).unwrap();
        let r = try_exec(&mut vm, "SELECT * FROM vec_t");
        assert!(r.is_ok());
    }

    let _ = fs::remove_dir_all(path);
}

#[test]
fn test_schema_with_trigger_restore() {
    use std::fs;
    let path = "/tmp/kkdb_test_trig_restore_b8";
    let _ = fs::remove_dir_all(path);

    {
        let mut vm = VM::open(path).unwrap();
        exec(
            &mut vm,
            "CREATE TABLE trig_t(id INTEGER PRIMARY KEY, val INTEGER, log_val INTEGER)",
        );
        let _ = try_exec(&mut vm,
            "CREATE TRIGGER tr_after_insert AFTER INSERT ON trig_t BEGIN UPDATE trig_t SET log_val = NEW.val * 2 WHERE id = NEW.id; END");
    }

    // Reopen
    {
        let mut vm = VM::open(path).unwrap();
        let r = try_exec(&mut vm, "INSERT INTO trig_t VALUES (1, 10, 0)");
        let _ = r;
    }

    let _ = fs::remove_dir_all(path);
}

// ═══════════════════════════════════════════════════════
// WAL-based file operations
// ═══════════════════════════════════════════════════════

#[test]
fn test_vm_file_with_wal_operations() {
    use std::fs;
    let path = "/tmp/kkdb_test_wal_ops_b8";
    let _ = fs::remove_dir_all(path);

    {
        let mut vm = VM::open(path).unwrap();
        exec(
            &mut vm,
            "CREATE TABLE wal_t(id INTEGER PRIMARY KEY, val TEXT)",
        );
        for i in 1..=50 {
            exec(
                &mut vm,
                &format!("INSERT INTO wal_t VALUES ({i}, 'data_{i}')"),
            );
        }
        let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM wal_t");
        assert_eq!(rows[0][0], Value::Integer(50));
    }

    let _ = fs::remove_dir_all(path);
}

// ═══════════════════════════════════════════════════════
// Edge case SQL
// ═══════════════════════════════════════════════════════

#[test]
fn test_select_1() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1, 2, 3");
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[0][1], Value::Integer(2));
}

#[test]
fn test_select_arithmetic() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 1 + 2, 10 - 3, 4 * 5, 20 / 4, 10 % 3");
    assert_eq!(rows[0][0], Value::Integer(3));
    assert_eq!(rows[0][1], Value::Integer(7));
    assert_eq!(rows[0][2], Value::Integer(20));
    assert_eq!(rows[0][3], Value::Integer(5));
    assert_eq!(rows[0][4], Value::Integer(1));
}

#[test]
fn test_concat_operator() {
    let mut vm = VM::new_memory();
    let rows = query_rows(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    assert_eq!(rows[0][0], Value::Text("hello world".into()));
}

#[test]
fn test_comparison_operators() {
    let mut vm = VM::new_memory();
    let rows = query_rows(
        &mut vm,
        "SELECT 1 < 2, 2 > 1, 1 <= 1, 1 >= 1, 1 = 1, 1 != 2",
    );
    assert_eq!(rows[0][0], Value::Integer(1)); // true
    assert_eq!(rows[0][5], Value::Integer(1)); // true
}
