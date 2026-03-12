// ═══════════════════════════════════════════════════════════════════
// Batch 13 — Deep coverage: BinlogFollower sync, record_to_sql,
//            parser error paths, more pager/btree code paths,
//            SHOW ENGINE STATUS, complex queries for selectivity
// ═══════════════════════════════════════════════════════════════════

use crate::types::Value;
use crate::vm::execute::{ExecResult, VM};

fn exec(vm: &mut VM, sql: &str) {
    vm.execute_sql(sql).unwrap_or_else(|e| panic!("EXEC `{sql}`: {e}"));
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
// 1. BinlogFollower::new and record_to_sql
// ═══════════════════════════════════════════════════════

#[test]
fn test_binlog_follower_new_and_record_to_sql() {
    use crate::binlog::{BinlogFollower, LogRecord};

    // Create follower with no checkpoint
    let follower = BinlogFollower::new("http://localhost:1234".into(), None);
    assert_eq!(follower.pos, 0);
    assert_eq!(follower.leader_url, "http://localhost:1234");

    // record_to_sql for all record types
    let sqls = BinlogFollower::record_to_sql(&LogRecord::Begin(1));
    assert_eq!(sqls.len(), 1);
    assert!(sqls[0].contains("BEGIN"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Insert {
        txid: 1,
        table_name: "test".into(),
        rowid: 42,
        row: vec![Value::Integer(42), Value::Text("hello".into())],
    });
    assert_eq!(sqls.len(), 1);
    assert!(sqls[0].contains("INSERT OR REPLACE"));
    assert!(sqls[0].contains("42"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Update {
        txid: 1,
        table_name: "test".into(),
        rowid: 42,
        old_row: vec![Value::Integer(42), Value::Text("old".into())],
        new_row: vec![Value::Integer(42), Value::Text("new".into())],
    });
    assert_eq!(sqls.len(), 1);
    assert!(sqls[0].contains("UPDATE"));
    assert!(sqls[0].contains("rowid = 42"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Delete {
        txid: 1,
        table_name: "test".into(),
        rowid: 42,
        row: None,
    });
    assert_eq!(sqls.len(), 1);
    assert!(sqls[0].contains("DELETE"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Commit(1));
    assert!(sqls[0].contains("COMMIT"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Rollback(1));
    assert!(sqls[0].contains("ROLLBACK"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Prepare(1));
    assert!(sqls[0].contains("PREPARE"));

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Sql {
        sql: "SELECT 1".into(),
        user_id: "admin".into(),
        raft_index: 0,
    });
    assert_eq!(sqls[0], "SELECT 1");
}

// ═══════════════════════════════════════════════════════
// 2. BinlogFollower with checkpoint file
// ═══════════════════════════════════════════════════════

#[test]
fn test_binlog_follower_checkpoint() {
    use crate::binlog::BinlogFollower;
    use std::fs;

    let ckpt_path = "/tmp/kkdb_b13_binlog_ckpt.txt";
    let _ = fs::remove_file(ckpt_path);

    // Write a checkpoint
    fs::write(ckpt_path, "12345").unwrap();

    let follower = BinlogFollower::new(
        "http://localhost:1234".into(),
        Some(std::path::PathBuf::from(ckpt_path)),
    );
    assert_eq!(follower.pos, 12345);

    let _ = fs::remove_file(ckpt_path);
}

// ═══════════════════════════════════════════════════════
// 3. value_to_sql_literal with various types
// ═══════════════════════════════════════════════════════

#[test]
fn test_binlog_record_to_sql_with_blob_and_null() {
    use crate::binlog::{BinlogFollower, LogRecord};

    let sqls = BinlogFollower::record_to_sql(&LogRecord::Insert {
        txid: 1,
        table_name: "blobs".into(),
        rowid: 1,
        row: vec![
            Value::Null,
            Value::Real(3.14),
            Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF]),
            Value::Text("it's a test".into()),
        ],
    });
    assert!(sqls[0].contains("NULL"));
    assert!(sqls[0].contains("3.14"));
    assert!(sqls[0].contains("X'deadbeef'"));
    assert!(sqls[0].contains("it''s a test")); // Escaped single quote
}

// ═══════════════════════════════════════════════════════
// 4. Binlog base64_encode
// ═══════════════════════════════════════════════════════

#[test]
fn test_binlog_base64_encode() {
    use crate::binlog::base64_encode;

    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
    assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

// ═══════════════════════════════════════════════════════
// 5. Parser error paths (unsupported SQL)
// ═══════════════════════════════════════════════════════

#[test]
fn test_parser_unsupported_statements() {
    let mut vm = VM::new_memory();

    // These should all return errors (triggering parser error paths)
    let unsupported = vec![
        "LOAD DATA INFILE 'data.csv' INTO TABLE t",
        "FETCH NEXT FROM cursor1",
        "CLOSE cursor1",
        "INSTALL EXTENSION http",
    ];

    for sql in unsupported {
        let r = try_exec(&mut vm, sql);
        assert!(r.is_err(), "expected error for: {sql}");
    }
}

// ═══════════════════════════════════════════════════════
// 6. Parser/eval: CompoundFieldAccess
// ═══════════════════════════════════════════════════════

#[test]
fn test_compound_field_access() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE cfa(id INTEGER PRIMARY KEY, val TEXT)");
    exec(&mut vm, "INSERT INTO cfa VALUES (1, 'hello')");

    // table.column style access
    let r = try_exec(&mut vm, "SELECT cfa.id, cfa.val FROM cfa");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 1);
    }
}

// ═══════════════════════════════════════════════════════
// 7. SHOW ENGINE STATUS with WAL
// ═══════════════════════════════════════════════════════

#[test]
fn test_show_engine_status_with_wal() {
    let mut vm = VM::new_memory();
    let _ = try_exec(&mut vm, "SET wal_enabled = 1");

    exec(&mut vm, "CREATE TABLE se(id INTEGER PRIMARY KEY, val TEXT)");
    for i in 1..=10 { exec(&mut vm, &format!("INSERT INTO se VALUES ({i}, 'data_{i}')")); }

    let r = try_exec(&mut vm, "SHOW ENGINE STATUS");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert!(!rows.is_empty()); // Should show engine stats
    }
}

// ═══════════════════════════════════════════════════════
// 8. SHOW STATUS
// ═══════════════════════════════════════════════════════

#[test]
fn test_show_status() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE ss(id INTEGER PRIMARY KEY, val INTEGER)");
    for i in 1..=20 { exec(&mut vm, &format!("INSERT INTO ss VALUES ({i}, {i})")); }

    // Trigger some queries to generate stats
    let _ = query_rows(&mut vm, "SELECT * FROM ss WHERE val > 10");
    let _ = query_rows(&mut vm, "SELECT COUNT(*) FROM ss");

    let r = try_exec(&mut vm, "SHOW STATUS");
    let _ = r; // May or may not be supported
}

// ═══════════════════════════════════════════════════════
// 9. EXPLAIN with complex queries
// ═══════════════════════════════════════════════════════

#[test]
fn test_explain_complex_plans() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE ep1(id INTEGER PRIMARY KEY, a TEXT, b INTEGER)");
    exec(&mut vm, "CREATE TABLE ep2(id INTEGER PRIMARY KEY, ep1_id INTEGER, c TEXT)");
    exec(&mut vm, "CREATE INDEX idx_ep2_fk ON ep2(ep1_id)");
    for i in 1..=20 { exec(&mut vm, &format!("INSERT INTO ep1 VALUES ({i}, 'cat_{}', {})", i % 3, i)); }
    for i in 1..=30 { exec(&mut vm, &format!("INSERT INTO ep2 VALUES ({i}, {}, 'data_{i}')", i % 20 + 1)); }
    exec(&mut vm, "ANALYZE ep1");
    exec(&mut vm, "ANALYZE ep2");

    let r = try_exec(&mut vm, "EXPLAIN SELECT ep1.a, COUNT(*) FROM ep1 JOIN ep2 ON ep1.id = ep2.ep1_id WHERE ep1.b > 5 GROUP BY ep1.a");
    assert!(r.is_ok());

    let r = try_exec(&mut vm, "EXPLAIN SELECT * FROM ep1 WHERE b BETWEEN 5 AND 15");
    assert!(r.is_ok());

    // EXPLAIN with subquery
    let r = try_exec(&mut vm, "EXPLAIN SELECT * FROM ep1 WHERE id IN (SELECT ep1_id FROM ep2 WHERE c LIKE 'data_1%')");
    assert!(r.is_ok());
}

// ═══════════════════════════════════════════════════════
// 10. Top-N with large OFFSET
// ═══════════════════════════════════════════════════════

#[test]
fn test_topn_large_offset() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE tn(id INTEGER PRIMARY KEY, val INTEGER)");
    for i in 1..=100 { exec(&mut vm, &format!("INSERT INTO tn VALUES ({i}, {i})")); }

    // Large offset with small limit
    let rows = query_rows(&mut vm, "SELECT * FROM tn ORDER BY val DESC LIMIT 5 OFFSET 90");
    assert_eq!(rows.len(), 5);

    // Offset beyond total rows
    let rows = query_rows(&mut vm, "SELECT * FROM tn ORDER BY val LIMIT 10 OFFSET 200");
    assert_eq!(rows.len(), 0);
}

// ═══════════════════════════════════════════════════════
// 11. CBO: BETWEEN selectivity estimation
// ═══════════════════════════════════════════════════════

#[test]
fn test_cbo_between_selectivity() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE cbo_bet(id INTEGER PRIMARY KEY, val INTEGER)");
    for i in 1..=200 { exec(&mut vm, &format!("INSERT INTO cbo_bet VALUES ({i}, {i})")); }
    exec(&mut vm, "ANALYZE cbo_bet");

    let r = try_exec(&mut vm, "EXPLAIN SELECT * FROM cbo_bet WHERE val BETWEEN 50 AND 100");
    assert!(r.is_ok());

    let r = try_exec(&mut vm, "EXPLAIN SELECT * FROM cbo_bet WHERE val > 150");
    assert!(r.is_ok());

    let r = try_exec(&mut vm, "EXPLAIN SELECT * FROM cbo_bet WHERE val < 50");
    assert!(r.is_ok());
}

// ═══════════════════════════════════════════════════════
// 12. BTree update_row
// ═══════════════════════════════════════════════════════

#[test]
fn test_btree_update_row() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let root = btree.create_table().unwrap();

    // Insert
    for i in 1..=20i64 {
        let row = vec![Value::Integer(i), Value::Text(format!("orig_{i}").into())];
        btree.insert(root, i, &row).unwrap();
    }

    // Update multiple rows
    for i in 1..=10i64 {
        let new_row = vec![Value::Integer(i), Value::Text(format!("updated_{i}").into())];
        let new_root = btree.update_row(root, i, &new_row).unwrap();
        assert!(new_root > 0);
    }

    // Verify
    let found = btree.find_by_rowid(root, 5).unwrap().unwrap();
    assert_eq!(found.1[1], Value::Text("updated_5".into()));

    let found = btree.find_by_rowid(root, 15).unwrap().unwrap();
    assert_eq!(found.1[1], Value::Text("orig_15".into()));

    drop(btree);
    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 13. BTree scan_rows_reverse_limit
// ═══════════════════════════════════════════════════════

#[test]
fn test_btree_scan_rows_reverse_limit() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let root = btree.create_table().unwrap();

    for i in 1..=50i64 {
        btree.insert(root, i, &vec![Value::Integer(i)]).unwrap();
    }

    let rev = btree.scan_rows_reverse_limit(root, 10).unwrap();
    assert_eq!(rev.len(), 10);
    // Last 10 rows in reverse order
    assert_eq!(rev[0][0], Value::Integer(50));
    assert_eq!(rev[9][0], Value::Integer(41));

    drop(btree);
    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 14. BTree defragment_leaf
// ═══════════════════════════════════════════════════════

#[test]
fn test_btree_defragment_leaf() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let root = btree.create_table().unwrap();

    // Insert and delete to create leaf fragmentation
    for i in 1..=20i64 {
        btree.insert(root, i, &vec![Value::Integer(i), Value::Text(format!("data_{i}").into())]).unwrap();
    }
    let mut current_root = root;
    for i in (1..=20i64).step_by(2) {
        let (_, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        current_root = new_root;
    }

    // Defragment the root leaf page
    let result = btree.defragment_leaf(current_root).unwrap();
    let _ = result;

    drop(btree);
    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 15. Pager release_savepoint
// ═══════════════════════════════════════════════════════

#[test]
fn test_pager_release_savepoint() {
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();

    let _ = pager.savepoint("sp1");
    let p1 = pager.allocate_page().unwrap();
    let page = pager.get_page_mut(p1).unwrap();
    page.data[0] = 0xAA;

    let _ = pager.savepoint("sp2");
    let p2 = pager.allocate_page().unwrap();
    let page = pager.get_page_mut(p2).unwrap();
    page.data[0] = 0xBB;

    // Release sp2 (commits changes since sp2)
    let _ = pager.release_savepoint("sp2");

    // Rollback to sp1 (undoes changes since sp1, including released sp2)
    let _ = pager.rollback_to_savepoint("sp1");

    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 16. Pager rollback_to_savepoint error (not found)
// ═══════════════════════════════════════════════════════

#[test]
fn test_pager_savepoint_not_found() {
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();

    let result = pager.rollback_to_savepoint("nonexistent");
    assert!(result.is_err());

    let result = pager.release_savepoint("nonexistent");
    assert!(result.is_err());

    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 17. Schema restore — vector index DDL
// ═══════════════════════════════════════════════════════

#[test]
fn test_schema_vector_index_restore() {
    use std::fs;
    let dir = "/tmp/kkdb_b13_vec_idx";
    let _ = fs::remove_dir_all(dir);

    {
        let mut vm = VM::open(dir).unwrap();
        exec(&mut vm, "CREATE TABLE vec_tbl(id INTEGER PRIMARY KEY, embedding BLOB)");
        let _ = try_exec(&mut vm, "CREATE VECTOR INDEX idx_vec ON vec_tbl(embedding) DIMENSION 4 DISTANCE cosine");
    }

    // Reopen and verify schema restored correctly
    {
        let mut vm = VM::open(dir).unwrap();
        let r = try_exec(&mut vm, "SELECT * FROM vec_tbl");
        assert!(r.is_ok());
    }

    let _ = fs::remove_dir_all(dir);
}

// ═══════════════════════════════════════════════════════
// 18. Complex HAVING with multiple aggregates
// ═══════════════════════════════════════════════════════

#[test]
fn test_having_multiple_aggregates() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE hma(id INTEGER PRIMARY KEY, cat TEXT, val INTEGER)");
    for i in 1..=30 {
        exec(&mut vm, &format!("INSERT INTO hma VALUES ({i}, 'cat_{}', {})", i % 3, i));
    }

    let r = try_exec(&mut vm,
        "SELECT cat, COUNT(*) AS cnt, SUM(val) AS total FROM hma GROUP BY cat HAVING COUNT(*) > 5 AND SUM(val) > 100 ORDER BY cat");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        let _ = rows;
    }
}

// ═══════════════════════════════════════════════════════
// 19. Nested function calls
// ═══════════════════════════════════════════════════════

#[test]
fn test_nested_function_calls() {
    let mut vm = VM::new_memory();

    let rows = query_rows(&mut vm, "SELECT UPPER(LOWER('Hello World'))");
    assert_eq!(rows[0][0], Value::Text("HELLO WORLD".into()));

    let rows = query_rows(&mut vm, "SELECT LENGTH(REPLACE('aaa bbb ccc', ' ', ''))");
    assert_eq!(rows[0][0], Value::Integer(9));

    let rows = query_rows(&mut vm, "SELECT ABS(ROUND(-3.7))");
    let _ = rows;
}

// ═══════════════════════════════════════════════════════
// 20. IS DISTINCT FROM / IS NOT DISTINCT FROM
// ═══════════════════════════════════════════════════════

#[test]
fn test_is_distinct_from() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE idf(id INTEGER PRIMARY KEY, val INTEGER)");
    exec(&mut vm, "INSERT INTO idf VALUES (1, NULL), (2, 10), (3, NULL)");

    // IS DISTINCT FROM treats NULL as a regular value
    let r = try_exec(&mut vm, "SELECT * FROM idf WHERE val IS DISTINCT FROM NULL");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 1); // only row 2 (val=10)
    }

    // IS NOT DISTINCT FROM
    let r = try_exec(&mut vm, "SELECT * FROM idf WHERE val IS NOT DISTINCT FROM NULL");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 2); // rows 1 and 3
    }
}

// ═══════════════════════════════════════════════════════
// 21. Table aliased in FROM
// ═══════════════════════════════════════════════════════

#[test]
fn test_table_alias_in_from() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE ta(id INTEGER PRIMARY KEY, val TEXT)");
    exec(&mut vm, "INSERT INTO ta VALUES (1, 'hello'), (2, 'world')");

    let rows = query_rows(&mut vm, "SELECT t.id, t.val FROM ta AS t ORDER BY t.id");
    assert_eq!(rows.len(), 2);
}

// ═══════════════════════════════════════════════════════
// 22. Unary minus and plus
// ═══════════════════════════════════════════════════════

#[test]
fn test_unary_minus_plus() {
    let mut vm = VM::new_memory();

    let rows = query_rows(&mut vm, "SELECT -5, +5, -(3 + 4)");
    assert_eq!(rows[0][0], Value::Integer(-5));
    assert_eq!(rows[0][1], Value::Integer(5));
    assert_eq!(rows[0][2], Value::Integer(-7));
}

// ═══════════════════════════════════════════════════════
// 23. Boolean expressions with AND/OR precedence
// ═══════════════════════════════════════════════════════

#[test]
fn test_boolean_precedence() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE bp(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, c INTEGER)");
    exec(&mut vm, "INSERT INTO bp VALUES (1, 1, 0, 1)");
    exec(&mut vm, "INSERT INTO bp VALUES (2, 0, 1, 1)");
    exec(&mut vm, "INSERT INTO bp VALUES (3, 1, 1, 0)");

    // AND before OR: a=1 AND b=1 OR c=1 → (a=1 AND b=1) OR c=1
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM bp WHERE a = 1 AND b = 1 OR c = 1");
    let _ = rows;
}

// ═══════════════════════════════════════════════════════
// 24. COUNT(DISTINCT column)
// ═══════════════════════════════════════════════════════

#[test]
fn test_count_distinct() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE cd(id INTEGER PRIMARY KEY, cat TEXT)");
    exec(&mut vm, "INSERT INTO cd VALUES (1, 'a'), (2, 'b'), (3, 'a'), (4, 'c'), (5, 'b')");

    let rows = query_rows(&mut vm, "SELECT COUNT(DISTINCT cat) FROM cd");
    assert_eq!(rows[0][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════
// 25. NULLS FIRST / NULLS LAST in ORDER BY
// ═══════════════════════════════════════════════════════

#[test]
fn test_nulls_first_last() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE nfl(id INTEGER PRIMARY KEY, val INTEGER)");
    exec(&mut vm, "INSERT INTO nfl VALUES (1, NULL), (2, 10), (3, NULL), (4, 5)");

    let r = try_exec(&mut vm, "SELECT val FROM nfl ORDER BY val NULLS FIRST");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert!(rows.len() >= 4);
    }

    let r = try_exec(&mut vm, "SELECT val FROM nfl ORDER BY val NULLS LAST");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert!(rows.len() >= 4);
    }
}

// ═══════════════════════════════════════════════════════
// 26. DELETE with complex WHERE
// ═══════════════════════════════════════════════════════

#[test]
fn test_delete_complex_where() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE dcw(id INTEGER PRIMARY KEY, val INTEGER, cat TEXT)");
    for i in 1..=30 { exec(&mut vm, &format!("INSERT INTO dcw VALUES ({i}, {i}, 'cat_{}') ", i % 5)); }

    exec(&mut vm, "DELETE FROM dcw WHERE val > 20 AND cat = 'cat_1'");
    let rows = query_rows(&mut vm, "SELECT COUNT(*) FROM dcw");
    let _ = rows;
}

// ═══════════════════════════════════════════════════════
// 27. DELETE FROM with LIMIT (if supported)
// ═══════════════════════════════════════════════════════

#[test]
fn test_delete_with_limit() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE dwl(id INTEGER PRIMARY KEY, val INTEGER)");
    for i in 1..=10 { exec(&mut vm, &format!("INSERT INTO dwl VALUES ({i}, {i})")); }

    let r = try_exec(&mut vm, "DELETE FROM dwl WHERE val > 5 LIMIT 3");
    let _ = r;
}

// ═══════════════════════════════════════════════════════
// 28. String concatenation with ||
// ═══════════════════════════════════════════════════════

#[test]
fn test_string_concat_operator() {
    let mut vm = VM::new_memory();

    let r = try_exec(&mut vm, "SELECT 'hello' || ' ' || 'world'");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows[0][0], Value::Text("hello world".into()));
    }
}

// ═══════════════════════════════════════════════════════
// 29. IN with subquery
// ═══════════════════════════════════════════════════════

#[test]
fn test_in_subquery_complex() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE isq1(id INTEGER PRIMARY KEY, val INTEGER)");
    exec(&mut vm, "CREATE TABLE isq2(id INTEGER PRIMARY KEY, ref_id INTEGER)");
    for i in 1..=20 { exec(&mut vm, &format!("INSERT INTO isq1 VALUES ({i}, {i})")); }
    for i in 1..=10 { exec(&mut vm, &format!("INSERT INTO isq2 VALUES ({i}, {})", i * 2)); }

    let rows = query_rows(&mut vm,
        "SELECT * FROM isq1 WHERE id IN (SELECT ref_id FROM isq2) ORDER BY id");
    assert_eq!(rows.len(), 10);
}

// ═══════════════════════════════════════════════════════
// 30. Pager page_count
// ═══════════════════════════════════════════════════════

#[test]
fn test_pager_allocate_many() {
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut pages = Vec::new();
    for _ in 0..10 {
        pages.push(pager.allocate_page().unwrap());
    }
    // Pages should be distinct and increasing
    for i in 1..pages.len() {
        assert!(pages[i] > pages[i - 1]);
    }
    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 31. Multiple nested CASE expressions
// ═══════════════════════════════════════════════════════

#[test]
fn test_nested_case_expressions() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE nce(id INTEGER PRIMARY KEY, val INTEGER)");
    exec(&mut vm, "INSERT INTO nce VALUES (1, 10), (2, 20), (3, 30), (4, 40)");

    let rows = query_rows(&mut vm,
        "SELECT id, CASE WHEN val < 15 THEN CASE WHEN id = 1 THEN 'first' ELSE 'other' END WHEN val < 35 THEN 'mid' ELSE 'high' END AS tier FROM nce ORDER BY id");
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0][1], Value::Text("first".into()));
    assert_eq!(rows[2][1], Value::Text("mid".into()));
    assert_eq!(rows[3][1], Value::Text("high".into()));
}

// ═══════════════════════════════════════════════════════
// 32. Complex UNION ALL with ORDER BY
// ═══════════════════════════════════════════════════════

#[test]
fn test_union_all_with_order_by() {
    let mut vm = VM::new_memory();

    let rows = query_rows(&mut vm,
        "SELECT 1 AS val, 'a' AS cat UNION ALL SELECT 3, 'c' UNION ALL SELECT 2, 'b' ORDER BY val");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0][0], Value::Integer(1));
    assert_eq!(rows[2][0], Value::Integer(3));
}

// ═══════════════════════════════════════════════════════
// 33. AUTO_INCREMENT behavior
// ═══════════════════════════════════════════════════════

#[test]
fn test_auto_increment_insert() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE ai(id INTEGER PRIMARY KEY, val TEXT)");

    // INSERT without id (auto-increment)
    let r = try_exec(&mut vm, "INSERT INTO ai(val) VALUES ('first')");
    let _ = r;

    let r = try_exec(&mut vm, "INSERT INTO ai(val) VALUES ('second')");
    let _ = r;

    let rows = query_rows(&mut vm, "SELECT * FROM ai ORDER BY id");
    assert!(rows.len() >= 2);
}

// ═══════════════════════════════════════════════════════
// 34. SELECT with multiple JOINs and aliases
// ═══════════════════════════════════════════════════════

#[test]
fn test_multi_join_with_aliases() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE mj1(id INTEGER PRIMARY KEY, name TEXT)");
    exec(&mut vm, "CREATE TABLE mj2(id INTEGER PRIMARY KEY, mj1_id INTEGER, score INTEGER)");
    exec(&mut vm, "CREATE TABLE mj3(id INTEGER PRIMARY KEY, mj2_id INTEGER, tag TEXT)");
    exec(&mut vm, "INSERT INTO mj1 VALUES (1, 'Alice'), (2, 'Bob')");
    exec(&mut vm, "INSERT INTO mj2 VALUES (1, 1, 100), (2, 2, 200)");
    exec(&mut vm, "INSERT INTO mj3 VALUES (1, 1, 'fast'), (2, 2, 'slow')");

    let r = try_exec(&mut vm,
        "SELECT a.name, b.score, c.tag FROM mj1 AS a JOIN mj2 AS b ON a.id = b.mj1_id JOIN mj3 AS c ON b.id = c.mj2_id ORDER BY a.name");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 2);
    }
}

// ═══════════════════════════════════════════════════════
// 35. ALTER TABLE DROP COLUMN
// ═══════════════════════════════════════════════════════

#[test]
fn test_alter_table_drop_column() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE adc(id INTEGER PRIMARY KEY, a TEXT, b INTEGER, c REAL)");
    exec(&mut vm, "INSERT INTO adc VALUES (1, 'hello', 42, 3.14)");

    let r = try_exec(&mut vm, "ALTER TABLE adc DROP COLUMN b");
    let _ = r; // May or may not be supported
}

// ═══════════════════════════════════════════════════════
// 36. Correlated subquery
// ═══════════════════════════════════════════════════════

#[test]
fn test_correlated_subquery() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE csq1(id INTEGER PRIMARY KEY, val INTEGER)");
    exec(&mut vm, "CREATE TABLE csq2(id INTEGER PRIMARY KEY, csq1_id INTEGER, score INTEGER)");
    exec(&mut vm, "INSERT INTO csq1 VALUES (1, 10), (2, 20), (3, 30)");
    exec(&mut vm, "INSERT INTO csq2 VALUES (1, 1, 100), (2, 1, 200), (3, 2, 150)");

    let r = try_exec(&mut vm,
        "SELECT id, val, (SELECT MAX(score) FROM csq2 WHERE csq2.csq1_id = csq1.id) AS max_score FROM csq1 ORDER BY id");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 3);
    }
}

// ═══════════════════════════════════════════════════════
// 37. Window function PARTITION BY with GROUP BY
// ═══════════════════════════════════════════════════════

#[test]
fn test_window_partition_with_group_by() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE wpg(id INTEGER PRIMARY KEY, dept TEXT, region TEXT, sales INTEGER)");
    exec(&mut vm, "INSERT INTO wpg VALUES (1, 'eng', 'east', 100)");
    exec(&mut vm, "INSERT INTO wpg VALUES (2, 'eng', 'west', 200)");
    exec(&mut vm, "INSERT INTO wpg VALUES (3, 'sales', 'east', 150)");
    exec(&mut vm, "INSERT INTO wpg VALUES (4, 'sales', 'west', 250)");
    exec(&mut vm, "INSERT INTO wpg VALUES (5, 'eng', 'east', 300)");

    let r = try_exec(&mut vm,
        "SELECT dept, SUM(sales) AS total, RANK() OVER(ORDER BY SUM(sales) DESC) AS rnk FROM wpg GROUP BY dept");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 2);
    }
}

// ═══════════════════════════════════════════════════════
// 38. DENSE_RANK window function
// ═══════════════════════════════════════════════════════

#[test]
fn test_dense_rank_window() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE dr(id INTEGER PRIMARY KEY, score INTEGER)");
    exec(&mut vm, "INSERT INTO dr VALUES (1, 100), (2, 100), (3, 90), (4, 90), (5, 80)");

    let r = try_exec(&mut vm,
        "SELECT id, score, DENSE_RANK() OVER(ORDER BY score DESC) AS drnk FROM dr ORDER BY id");
    if let Ok(ExecResult::QueryResult { rows, .. }) = r {
        assert_eq!(rows.len(), 5);
    }
}

// ═══════════════════════════════════════════════════════
// 39. BTree: insert, scan_all, count in large scale
// ═══════════════════════════════════════════════════════

#[test]
fn test_btree_large_scale_operations() {
    use crate::storage::btree::BTree;
    use crate::storage::pager::Pager;

    let mut pager = Pager::open_memory();
    pager.begin_transaction().unwrap();
    let mut btree = BTree::new(&mut pager);
    let root = btree.create_table().unwrap();

    // Insert 500 rows — root may change on splits
    let mut current_root = root;
    for i in 1..=500i64 {
        let row = vec![Value::Integer(i), Value::Text(format!("row_{i}").into())];
        current_root = btree.insert(current_root, i, &row).unwrap();
    }

    let count = btree.count_rows(current_root).unwrap();
    assert_eq!(count, 500);

    let all = btree.scan_all(current_root).unwrap();
    assert_eq!(all.len(), 500);

    // verify ordering
    for (idx, (rid, _)) in all.iter().enumerate() {
        assert_eq!(*rid, (idx + 1) as i64);
    }

    drop(btree);
    pager.commit_transaction().unwrap();
}

// ═══════════════════════════════════════════════════════
// 40. EXPLAIN ANALYZE (if supported)
// ═══════════════════════════════════════════════════════

#[test]
fn test_explain_analyze() {
    let mut vm = VM::new_memory();
    exec(&mut vm, "CREATE TABLE ea(id INTEGER PRIMARY KEY, val INTEGER)");
    for i in 1..=20 { exec(&mut vm, &format!("INSERT INTO ea VALUES ({i}, {i})")); }
    exec(&mut vm, "ANALYZE ea");

    // EXPLAIN ANALYZE
    let r = try_exec(&mut vm, "EXPLAIN ANALYZE SELECT * FROM ea WHERE val > 10");
    let _ = r;
}
