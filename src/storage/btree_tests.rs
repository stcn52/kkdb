use super::*;
use crate::types::Value;

fn make_pager() -> Pager {
    Pager::open_memory()
}

fn make_row(id: i64, name: &str) -> Row {
    vec![Value::Integer(id), Value::Text(name.into())]
}

/// Create a row with a large payload (~200 bytes) to trigger page splits
fn make_big_row(id: i64) -> Row {
    let big_name = "X".repeat(180);
    vec![Value::Integer(id), Value::Text(big_name.into())]
}

#[test]
fn test_create_table() {
    let mut pager = make_pager();
    let mut btree = BTree::new(&mut pager);
    let root = btree.create_table().unwrap();
    assert!(root >= 4); // page 1/2 superblock, page 3 schema root
}

#[test]
fn test_insert_and_scan() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "Alice")).unwrap();
        btree.insert(root, 2, &make_row(2, "Bob")).unwrap();
        btree.insert(root, 3, &make_row(3, "Charlie")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[2].0, 3);
}

#[test]
fn test_insert_sorted_order() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 3, &make_row(3, "C")).unwrap();
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
        btree.insert(root, 2, &make_row(2, "B")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[2].0, 3);
}

#[test]
fn test_insert_duplicate_rowid() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let result = btree.insert(root, 1, &make_row(1, "B"));
    assert!(result.is_err());
}

#[test]
fn test_find_by_rowid() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "Alice")).unwrap();
        btree.insert(root, 5, &make_row(5, "Eve")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let found = btree.find_by_rowid(root, 1).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().0, 1);

    let found = btree.find_by_rowid(root, 5).unwrap();
    assert!(found.is_some());

    let not_found = btree.find_by_rowid(root, 99).unwrap();
    assert!(not_found.is_none());
}

#[test]
fn test_delete_by_rowid() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
        btree.insert(root, 2, &make_row(2, "B")).unwrap();
        btree.insert(root, 3, &make_row(3, "C")).unwrap();
    }
    {
        let mut btree = BTree::new(&mut pager);
        let (deleted, _) = btree.delete_by_rowid(root, 2).unwrap();
        assert!(deleted);
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[1].0, 3);
}

#[test]
fn test_delete_nonexistent() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let (deleted, _) = btree.delete_by_rowid(root, 99).unwrap();
    assert!(!deleted);
}

#[test]
fn test_update_row() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "Old")).unwrap();
    }
    {
        let mut btree = BTree::new(&mut pager);
        btree.update_row(root, 1, &make_row(1, "New")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let found = btree.find_by_rowid(root, 1).unwrap().unwrap();
    assert_eq!(found.1[1], Value::Text("New".into()));
}

#[test]
fn test_max_rowid() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        let max = btree.max_rowid(root).unwrap();
        assert_eq!(max, 0); // empty table
    }
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 5, &make_row(5, "E")).unwrap();
        btree.insert(root, 10, &make_row(10, "J")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let max = btree.max_rowid(root).unwrap();
    assert_eq!(max, 10);
}

#[test]
fn test_count_rows() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        assert_eq!(btree.count_rows(root).unwrap(), 0);
    }
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
        btree.insert(root, 2, &make_row(2, "B")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    assert_eq!(btree.count_rows(root).unwrap(), 2);
}

#[test]
fn test_split_leaf_many_inserts() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Use big rows (~200 bytes each) to trigger actual page splits
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Verify all rows present
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 20);
    // Verify sorted
    for i in 0..19 {
        assert!(rows[i].0 < rows[i + 1].0);
    }
}

#[test]
fn test_scan_empty_table() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert!(rows.is_empty());
}

#[test]
fn test_header_offset() {
    assert_eq!(BTree::header_offset(1), 0);
    assert_eq!(BTree::header_offset(2), 0);
    assert_eq!(BTree::header_offset(100), 0);
}

#[test]
fn test_find_delete_with_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    // Use big rows to trigger actual page splits
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Find in interior tree
    {
        let mut btree = BTree::new(&mut pager);
        let found = btree.find_by_rowid(current_root, 15).unwrap();
        assert!(found.is_some());
        let not_found = btree.find_by_rowid(current_root, 999).unwrap();
        assert!(not_found.is_none());
    }
    // Delete from interior tree
    {
        let mut btree = BTree::new(&mut pager);
        let (deleted, _) = btree.delete_by_rowid(current_root, 15).unwrap();
        assert!(deleted);
    }
    {
        let mut btree = BTree::new(&mut pager);
        let rows = btree.scan_all(current_root).unwrap();
        assert_eq!(rows.len(), 19);
    }
}

#[test]
fn test_count_rows_with_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    assert_eq!(btree.count_rows(current_root).unwrap(), 20);
}

#[test]
fn test_max_rowid_with_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    assert_eq!(btree.max_rowid(current_root).unwrap(), 20);
}

#[test]
fn test_update_row_with_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    {
        let mut btree = BTree::new(&mut pager);
        btree
            .update_row(current_root, 15, &make_row(15, "updated"))
            .unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let found = btree.find_by_rowid(current_root, 15).unwrap().unwrap();
    assert_eq!(found.1[1], Value::Text("updated".into()));
}

#[test]
fn test_find_by_rowid_not_found_early_exit() {
    // Test the early exit path: rowid > target means target doesn't exist
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 5, &make_row(5, "E")).unwrap();
        btree.insert(root, 10, &make_row(10, "J")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    // rowid 3 < 5 so the loop hits rowid > target_rowid early
    let not_found = btree.find_by_rowid(root, 3).unwrap();
    assert!(not_found.is_none());
}

#[test]
fn test_delete_nonexistent_in_split_tree() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let (deleted, _) = btree.delete_by_rowid(current_root, 999).unwrap();
    assert!(!deleted);
}

#[test]
fn test_insert_reverse_order_with_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    // Insert in reverse order with big rows to trigger splits
    for i in (1..=20).rev() {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 20);
    // Verify sorted order
    for i in 0..19 {
        assert!(rows[i].0 < rows[i + 1].0);
    }
}

#[test]
fn test_find_by_rowid_at_boundary_in_split() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Find first, last, and boundary rows in a split tree
    let mut btree = BTree::new(&mut pager);
    assert!(btree.find_by_rowid(current_root, 1).unwrap().is_some());
    assert!(btree.find_by_rowid(current_root, 20).unwrap().is_some());
    assert!(btree.find_by_rowid(current_root, 0).unwrap().is_none());
    assert!(btree.find_by_rowid(current_root, 21).unwrap().is_none());
}

// ==============================================================
// Recursive split tests — three-level trees, large scale
// ==============================================================

#[test]
fn test_three_level_tree_large_insert() {
    // Insert enough big rows to trigger interior page splits (three-level tree)
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 100;
    for i in 1..=n {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Verify all rows present and sorted
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), n as usize);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.0, (i + 1) as i64);
    }
}

#[test]
fn test_three_level_tree_reverse_insert() {
    // Insert in reverse order to stress different split paths
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 80;
    for i in (1..=n).rev() {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), n as usize);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.0, (i + 1) as i64);
    }
}

#[test]
fn test_three_level_tree_interleaved_insert() {
    // Insert in interleaved order: evens then odds
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 60i64;
    // Insert even numbers first
    for i in (2..=n).step_by(2) {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Then odd numbers
    for i in (1..=n).step_by(2) {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), n as usize);
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row.0, (i + 1) as i64);
    }
}

#[test]
fn test_three_level_tree_find_all() {
    // Build a large tree and verify every rowid is findable
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 100;
    for i in 1..=n {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    for i in 1..=n {
        let found = btree.find_by_rowid(current_root, i).unwrap();
        assert!(found.is_some(), "rowid {} not found", i);
    }
    // Verify non-existent rows
    assert!(btree.find_by_rowid(current_root, 0).unwrap().is_none());
    assert!(btree.find_by_rowid(current_root, n + 1).unwrap().is_none());
}

#[test]
fn test_three_level_tree_delete_and_verify() {
    // Build large tree, delete some rows, verify remaining
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 60;
    for i in 1..=n {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Delete every 3rd row
    for i in (3..=n).step_by(3) {
        let mut btree = BTree::new(&mut pager);
        let (deleted, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        assert!(deleted);
        current_root = new_root;
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    let expected_count = n as usize - (n as usize / 3);
    assert_eq!(rows.len(), expected_count);
    // Verify deleted rows are gone, others present
    for i in 1..=n {
        let found = btree.find_by_rowid(current_root, i).unwrap();
        if i % 3 == 0 {
            assert!(found.is_none(), "rowid {} should be deleted", i);
        } else {
            assert!(found.is_some(), "rowid {} should exist", i);
        }
    }
}

#[test]
fn test_three_level_tree_count_rows() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    let n = 80;
    for i in 1..=n {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let count = btree.count_rows(current_root).unwrap();
    assert_eq!(count, n as u64);
}

#[test]
fn test_insert_after_delete_in_split_tree() {
    // Delete some rows then insert new ones — verify tree integrity
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=30 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Delete rows 10-20
    for i in 10..=20 {
        let mut btree = BTree::new(&mut pager);
        let (deleted, new_root) = btree.delete_by_rowid(current_root, i).unwrap();
        assert!(deleted);
        current_root = new_root;
    }
    // Insert new rows with higher IDs
    for i in 31..=50 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    // 30 - 11 (deleted 10..=20) + 20 (inserted 31..=50) = 39
    assert_eq!(rows.len(), 39);
    // Verify sorted
    for i in 0..rows.len() - 1 {
        assert!(rows[i].0 < rows[i + 1].0);
    }
}

#[test]
fn test_right_edge_append_efficiency() {
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    // Insert 200 big rows sequentially
    for i in 1..=200 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }

    // Scan and verify
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 200);
    for i in 0..199 {
        assert!(rows[i].0 < rows[i + 1].0);
    }
}

// ── Overflow page tests (S1) ──────────────────────────────────────────────

/// Generate a row with a TEXT field of `n` bytes to trigger overflow.
fn make_overflow_row(id: i64, n: usize) -> Row {
    vec![Value::Integer(id), Value::Text("A".repeat(n).into())]
}

#[test]
fn test_overflow_insert_and_scan() {
    // 5 KB TEXT — well over MAX_INLINE_PAYLOAD (~2016 bytes)
    let mut pager = make_pager();
    let mut root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };

    let big_row = make_overflow_row(1, 5000);
    {
        let mut btree = BTree::new(&mut pager);
        root = btree.insert(root, 1, &big_row).unwrap();
    }

    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 1);
    // Verify the large TEXT value came back intact
    if let Value::Text(s) = &rows[0].1[1] {
        assert_eq!(s.len(), 5000);
        assert!(s.chars().all(|c| c == 'A'));
    } else {
        panic!("Expected Text value");
    }
}

#[test]
fn test_overflow_multi_chunk() {
    // 16 KB TEXT — spans multiple overflow pages (each page holds PAGE_SIZE-4 = 4092 bytes)
    let mut pager = make_pager();
    let mut root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };

    let big_row = make_overflow_row(42, 16384);
    {
        let mut btree = BTree::new(&mut pager);
        root = btree.insert(root, 42, &big_row).unwrap();
    }

    let mut btree = BTree::new(&mut pager);
    let found = btree.find_by_rowid(root, 42).unwrap();
    assert!(found.is_some());
    let (rid, row) = found.unwrap();
    assert_eq!(rid, 42);
    if let Value::Text(s) = &row[1] {
        assert_eq!(s.len(), 16384);
    } else {
        panic!("Expected Text value");
    }
}

#[test]
fn test_overflow_mixed_normal_and_overflow() {
    // Mix of normal and large rows; verify scan order is correct
    let mut pager = make_pager();
    let mut root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };

    {
        let mut btree = BTree::new(&mut pager);
        root = btree.insert(root, 1, &make_row(1, "small")).unwrap();
        root = btree.insert(root, 2, &make_overflow_row(2, 6000)).unwrap();
        root = btree.insert(root, 3, &make_row(3, "also small")).unwrap();
        root = btree.insert(root, 4, &make_overflow_row(4, 8000)).unwrap();
    }

    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[1].0, 2);
    assert_eq!(rows[2].0, 3);
    assert_eq!(rows[3].0, 4);

    if let Value::Text(s) = &rows[1].1[1] {
        assert_eq!(s.len(), 6000);
    } else {
        panic!("row 2 should be big TEXT");
    }
}

#[test]
fn test_overflow_delete_frees_chain() {
    // After deleting an overflow row, insert a normal row — should succeed (no crash)
    let mut pager = make_pager();
    let mut root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };

    {
        let mut btree = BTree::new(&mut pager);
        root = btree
            .insert(root, 10, &make_overflow_row(10, 5000))
            .unwrap();
    }

    // Delete the overflow row
    {
        let mut btree = BTree::new(&mut pager);
        let (deleted, new_root) = btree.delete_by_rowid(root, 10).unwrap();
        assert!(deleted);
        root = new_root;
    }

    // Verify it's gone
    {
        let mut btree = BTree::new(&mut pager);
        assert!(btree.find_by_rowid(root, 10).unwrap().is_none());
    }

    // Insert a normal row at the same rowid — should work cleanly
    {
        let mut btree = BTree::new(&mut pager);
        root = btree.insert(root, 10, &make_row(10, "normal")).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 10);
}

// ── F1: Prefix-compressed index scan tests ────────────────────────────────

fn make_index_row(key: &str, rowid_val: i64) -> Row {
    vec![Value::Text(key.into()), Value::Integer(rowid_val)]
}

#[test]
fn test_f1_scan_all_compressed_basic_roundtrip() {
    let mut pager = make_pager();
    let root = {
        let mut b = BTree::new(&mut pager);
        b.create_table().unwrap()
    };
    let keys = [
        "user_0001",
        "user_0002",
        "user_0003",
        "user_0010",
        "user_0099",
    ];
    let mut cur_root = root;
    let mut prev: Vec<u8> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let row = make_index_row(key, i as i64 + 1);
        let mut b = BTree::new(&mut pager);
        let (nr, np) = b
            .insert_compressed(cur_root, i as i64 + 1, &row, &prev)
            .unwrap();
        cur_root = nr;
        prev = np;
    }
    let mut b = BTree::new(&mut pager);
    let res = b.scan_all_compressed(cur_root).unwrap();
    assert_eq!(res.len(), keys.len());
    for (i, (rowid, row)) in res.iter().enumerate() {
        assert_eq!(*rowid, i as i64 + 1);
        assert_eq!(row[0], Value::Text(keys[i].into()), "key[{}] mismatch", i);
    }
}

#[test]
fn test_f1_scan_all_compressed_no_shared_prefix() {
    // Keys with no common prefix — still round-trips correctly
    let mut pager = make_pager();
    let root = {
        let mut b = BTree::new(&mut pager);
        b.create_table().unwrap()
    };
    let keys = ["apple", "banana", "cherry", "date", "elderberry"];
    let mut cur_root = root;
    let mut prev: Vec<u8> = Vec::new();
    for (i, key) in keys.iter().enumerate() {
        let row = make_index_row(key, i as i64 + 10);
        let mut b = BTree::new(&mut pager);
        let (nr, np) = b
            .insert_compressed(cur_root, i as i64 + 10, &row, &prev)
            .unwrap();
        cur_root = nr;
        prev = np;
    }
    let mut b = BTree::new(&mut pager);
    let res = b.scan_all_compressed(cur_root).unwrap();
    assert_eq!(res.len(), 5);
    for (i, (_, row)) in res.iter().enumerate() {
        assert_eq!(row[0], Value::Text(keys[i].into()));
    }
}

#[test]
fn test_f1_scan_all_compressed_with_page_split() {
    // Enough rows to cause leaf page splits; prefix decoder resets at page boundaries
    let mut pager = make_pager();
    let root = {
        let mut b = BTree::new(&mut pager);
        b.create_table().unwrap()
    };
    let n = 60usize;
    let mut cur_root = root;
    let mut prev: Vec<u8> = Vec::new();
    for i in 0..n {
        let key = format!("record_{:06}", i);
        let row = make_index_row(&key, i as i64);
        let mut b = BTree::new(&mut pager);
        let (nr, np) = b
            .insert_compressed(cur_root, i as i64, &row, &prev)
            .unwrap();
        cur_root = nr;
        prev = np;
    }
    let mut b = BTree::new(&mut pager);
    let res = b.scan_all_compressed(cur_root).unwrap();
    assert_eq!(res.len(), n, "post-split row count wrong");
    for (i, (rowid, row)) in res.iter().enumerate() {
        assert_eq!(*rowid, i as i64);
        let expected = format!("record_{:06}", i);
        assert_eq!(
            row[0],
            Value::Text(expected.as_str().into()),
            "split key[{}] wrong",
            i
        );
    }
}

#[test]
fn test_f1_scan_all_compressed_identical_keys() {
    // Identical consecutive keys: suffix_len=0, shared=full key; must decode correctly
    let mut pager = make_pager();
    let root = {
        let mut b = BTree::new(&mut pager);
        b.create_table().unwrap()
    };
    let mut cur_root = root;
    let mut prev: Vec<u8> = Vec::new();
    for i in 0..5i64 {
        let row = make_index_row("same_key", i);
        let mut b = BTree::new(&mut pager);
        let (nr, np) = b.insert_compressed(cur_root, i, &row, &prev).unwrap();
        cur_root = nr;
        prev = np;
    }
    let mut b = BTree::new(&mut pager);
    let res = b.scan_all_compressed(cur_root).unwrap();
    assert_eq!(res.len(), 5);
    for (_, row) in &res {
        assert_eq!(row[0], Value::Text("same_key".into()));
    }
}

#[test]
fn test_f1_insert_compressed_integer_key_passthrough() {
    // Integer first column bypasses prefix compression; scan_all_compressed handles it
    let mut pager = make_pager();
    let root = {
        let mut b = BTree::new(&mut pager);
        b.create_table().unwrap()
    };
    let mut cur_root = root;
    let mut prev: Vec<u8> = Vec::new();
    for i in 0..5i64 {
        let row = vec![Value::Integer(i * 100), Value::Text("v".into())];
        let mut b = BTree::new(&mut pager);
        let (nr, np) = b.insert_compressed(cur_root, i, &row, &prev).unwrap();
        cur_root = nr;
        prev = np;
    }
    let mut b = BTree::new(&mut pager);
    let res = b.scan_all_compressed(cur_root).unwrap();
    assert_eq!(res.len(), 5);
    for (i, (rowid, row)) in res.iter().enumerate() {
        assert_eq!(*rowid, i as i64);
        assert_eq!(row[0], Value::Integer(i as i64 * 100));
    }
}

// ── Corruption error path tests ──────────────────────────────────────────

#[test]
fn test_count_rows_corrupt_page_type() {
    // Set the page_type byte to an unknown value.
    // count_rows treats unknown page types as 0 rows (the _ => Ok(0) arm).
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Insert a row so the page has valid data first
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
    }
    // Verify normal count works
    {
        let mut btree = BTree::new(&mut pager);
        assert_eq!(btree.count_rows(root).unwrap(), 1);
    }
    // Corrupt the page_type byte to 0xFF (unknown type)
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0xFF;
    }
    // count_rows should return 0 for unknown page type
    let mut btree = BTree::new(&mut pager);
    let count = btree.count_rows(root).unwrap();
    assert_eq!(count, 0, "unknown page type should return 0 rows");
}

#[test]
fn test_count_rows_corrupt_interior_cell_pointer() {
    // Build a tree large enough to have interior pages, then corrupt
    // a cell pointer in the root (interior) page.
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    // Insert enough big rows to force interior pages
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Verify count works before corruption
    {
        let mut btree = BTree::new(&mut pager);
        assert_eq!(btree.count_rows(current_root).unwrap(), 20);
    }
    // Check if root is an interior page
    let is_interior = {
        let page = pager.get_page(current_root).unwrap();
        page.data[0] == INTERIOR_TABLE
    };
    if !is_interior {
        // Root is still a leaf (all rows fit in one page).
        // We can still test count_rows with a corrupt page type.
        let page = pager.get_page_mut(current_root).unwrap();
        page.data[0] = 0xFF; // unknown type → count_rows returns 0
        let mut btree = BTree::new(&mut pager);
        assert_eq!(btree.count_rows(current_root).unwrap(), 0);
        return;
    }

    // Corrupt the first cell pointer (at INTERIOR_HEADER_SIZE offset)
    // to point to a child page number that doesn't exist (e.g. page 9999)
    {
        let page = pager.get_page_mut(current_root).unwrap();
        let ptr_base = INTERIOR_HEADER_SIZE; // 10
        // Read the cell pointer to get cell offset
        let cell_off = u16::from_le_bytes(page.data[ptr_base..ptr_base + 2].try_into().unwrap()) as usize;
        // Overwrite the child page number at that cell with an invalid page
        page.data[cell_off..cell_off + 4].copy_from_slice(&9999u32.to_le_bytes());
    }
    // count_rows will recurse into the bogus page and fail
    let mut btree = BTree::new(&mut pager);
    let result = btree.count_rows(current_root);
    assert!(result.is_err(), "expected error for corrupt child page pointer");
}

#[test]
fn test_find_by_rowid_corrupt_interior_page() {
    use crate::error::KkdbError;
    // Build a tree with interior pages, then corrupt root page data.
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }
    // Verify find works before corruption
    {
        let mut btree = BTree::new(&mut pager);
        assert!(btree.find_by_rowid(current_root, 10).unwrap().is_some());
    }

    // Corrupt the page: set page_type to an invalid value
    {
        let page = pager.get_page_mut(current_root).unwrap();
        page.data[0] = 0xAB; // unknown page type
    }
    let mut btree = BTree::new(&mut pager);
    let result = btree.find_by_rowid(current_root, 10);
    match result {
        Err(KkdbError::BTreeError(msg)) => {
            assert!(msg.contains("unexpected page type"), "got: {}", msg);
        }
        Err(other) => panic!("expected BTreeError(unexpected page type), got {}", other),
        Ok(_) => panic!("expected error for corrupt page type in find_by_rowid"),
    }
}

#[test]
fn test_find_by_rowid_corrupt_cell_content() {
    // Build a tree, then corrupt the payload data so deserialization fails.
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
        btree.insert(root, 2, &make_row(2, "B")).unwrap();
        btree.insert(root, 3, &make_row(3, "C")).unwrap();
    }
    // Verify normal find works
    {
        let mut btree = BTree::new(&mut pager);
        assert!(btree.find_by_rowid(root, 2).unwrap().is_some());
    }

    // Corrupt: find which cell has rowid 2, then corrupt its payload so
    // deserialization fails. We write an invalid Value tag byte (0xFF)
    // at the start of the payload data.
    {
        let page = pager.get_page_mut(root).unwrap();
        let ptr_base = LEAF_HEADER_SIZE; // 14
        let cell_count = u16::from_le_bytes(page.data[1..3].try_into().unwrap()) as usize;
        // Find the cell with rowid 2
        for i in 0..cell_count {
            let ptr_off = ptr_base + i * 2;
            let cell_off = u16::from_le_bytes(page.data[ptr_off..ptr_off + 2].try_into().unwrap()) as usize;
            let rowid = i64::from_le_bytes(page.data[cell_off + 4..cell_off + 12].try_into().unwrap());
            if rowid == 2 {
                // payload_size is at cell_off..cell_off+4, inline payload starts at cell_off+12
                let payload_size = u32::from_le_bytes(page.data[cell_off..cell_off + 4].try_into().unwrap()) as usize;
                // Write column count varint = 1, then invalid tag byte 0xFF
                if payload_size >= 2 {
                    page.data[cell_off + 12] = 0x01; // column count = 1
                    page.data[cell_off + 13] = 0xFF; // invalid Value tag
                }
                break;
            }
        }
    }

    // find_by_rowid for rowid 2 should fail during deserialization
    let mut btree = BTree::new(&mut pager);
    let result = btree.find_by_rowid(root, 2);
    assert!(result.is_err(), "expected deserialization error for corrupt payload");
}

#[test]
fn test_scan_leaf_chain_corrupt_cell_pointer_offset() {
    // Create table with rows, then corrupt a cell pointer to point
    // to in-bounds but corrupt payload data.
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
        btree.insert(root, 2, &make_row(2, "B")).unwrap();
    }
    // Corrupt cell pointer 0 to point to offset 200 (within page)
    // and put invalid data there so deserialization fails.
    {
        let page = pager.get_page_mut(root).unwrap();
        let ptr_base = LEAF_HEADER_SIZE; // 14
        // Point cell 0 to offset 200 (well within page)
        page.data[ptr_base..ptr_base + 2].copy_from_slice(&200u16.to_le_bytes());
        // At offset 200, write a normal payload_size of 20 (no overflow)
        page.data[200..204].copy_from_slice(&20u32.to_le_bytes());
        // rowid at offset 204..212
        page.data[204..212].copy_from_slice(&1i64.to_le_bytes());
        // payload at offset 212..232: write col_count=1, then invalid tag 0xFF
        page.data[212] = 0x01; // varint col_count=1
        page.data[213] = 0xFF; // invalid Value tag
    }
    let mut btree = BTree::new(&mut pager);
    let result = btree.scan_all(root);
    assert!(result.is_err(), "expected error for corrupt cell payload data");
}

#[test]
fn test_delete_from_corrupt_page_type() {
    use crate::error::KkdbError;
    // Create a table, insert a row, then corrupt the page type.
    // delete_by_rowid should return an error.
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "A")).unwrap();
    }
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0xFE; // unknown page type
    }
    let mut btree = BTree::new(&mut pager);
    let result = btree.delete_by_rowid(root, 1);
    match result {
        Err(KkdbError::BTreeError(msg)) => {
            assert!(msg.contains("unexpected page type"), "got: {}", msg);
        }
        Err(other) => panic!("expected BTreeError, got {}", other),
        Ok(_) => panic!("expected error for corrupt page type in delete_by_rowid"),
    }
}

#[test]
fn test_max_rowid_corrupt_page_type() {
    use crate::error::KkdbError;
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 42, &make_row(42, "X")).unwrap();
    }
    {
        let page = pager.get_page_mut(root).unwrap();
        page.data[0] = 0xBB; // unknown page type
    }
    let mut btree = BTree::new(&mut pager);
    let result = btree.max_rowid(root);
    match result {
        Err(KkdbError::BTreeError(msg)) => {
            assert!(msg.contains("unexpected page type"), "got: {}", msg);
        }
        Err(other) => panic!("expected BTreeError, got {}", other),
        Ok(_) => panic!("expected error for corrupt page type in max_rowid"),
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Overflow chain tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_overflow_insert_and_retrieve() {
    // Insert a row large enough to trigger overflow chain
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Payload larger than MAX_INLINE_PAYLOAD (~2016 bytes)
    let big_text = "A".repeat(3000);
    let big_row = vec![Value::Integer(1), Value::Text(big_text.clone().into())];
    let current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &big_row).unwrap()
    };
    // Retrieve and verify
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 1);
    match &rows[0].1[1] {
        Value::Text(s) => assert_eq!(s.len(), 3000),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn test_overflow_multiple_pages() {
    // Even larger payload requiring multiple overflow pages
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let huge_text = "B".repeat(10000);
    let big_row = vec![Value::Integer(1), Value::Text(huge_text.clone().into())];
    let current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &big_row).unwrap()
    };
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 1);
    match &rows[0].1[1] {
        Value::Text(s) => assert_eq!(s.len(), 10000),
        other => panic!("expected Text, got {:?}", other),
    }
}

#[test]
fn test_overflow_delete() {
    // Insert overflow row, then delete it
    let mut pager = make_pager();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let big_row = vec![Value::Integer(1), Value::Text("C".repeat(3000).into())];
    let current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &big_row).unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.delete_by_rowid(current_root, 1).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 0);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Page split stress tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_many_inserts_trigger_multiple_splits() {
    let mut pager = make_pager();
    let mut current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Insert 500 rows with moderately large payload to force many splits
    for i in 1..=500_i64 {
        let row = vec![Value::Integer(i), Value::Text(format!("row_{:04}", i).into())];
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &row).unwrap();
    }
    // Scan and verify order
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 500);
    for (idx, (rowid, _)) in rows.iter().enumerate() {
        assert_eq!(*rowid, (idx + 1) as i64, "row {} out of order", idx);
    }
}

#[test]
fn test_reverse_order_inserts() {
    // Insert in reverse order to stress interior page building
    let mut pager = make_pager();
    let mut current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    for i in (1..=200_i64).rev() {
        let row = make_row(i, &format!("rev_{}", i));
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &row).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 200);
    // Verify sorted order
    for (idx, (rowid, _)) in rows.iter().enumerate() {
        assert_eq!(*rowid, (idx + 1) as i64);
    }
}

#[test]
fn test_random_order_inserts() {
    // Insert in pseudo-random order
    let mut pager = make_pager();
    let mut current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let order: Vec<i64> = {
        let mut v: Vec<i64> = (1..=100).collect();
        // Simple deterministic shuffle using XOR
        for i in (1..v.len()).rev() {
            let j = (i * 7 + 3) % (i + 1);
            v.swap(i, j);
        }
        v
    };
    for &id in &order {
        let row = make_row(id, &format!("rand_{}", id));
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, id, &row).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 100);
    for (idx, (rowid, _)) in rows.iter().enumerate() {
        assert_eq!(*rowid, (idx + 1) as i64);
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Delete + scan boundary tests
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_delete_all_then_scan() {
    let mut pager = make_pager();
    let mut current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    for i in 1..=10_i64 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_row(i, &format!("d_{}", i))).unwrap();
    }
    for i in 1..=10 {
        let mut btree = BTree::new(&mut pager);
        btree.delete_by_rowid(current_root, i).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 0);
}

#[test]
fn test_delete_every_other_row() {
    let mut pager = make_pager();
    let mut current_root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    for i in 1..=20_i64 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_row(i, &format!("e_{}", i))).unwrap();
    }
    // Delete even rows
    for i in (2..=20).step_by(2) {
        let mut btree = BTree::new(&mut pager);
        btree.delete_by_rowid(current_root, i).unwrap();
    }
    let mut btree = BTree::new(&mut pager);
    let rows = btree.scan_all(current_root).unwrap();
    assert_eq!(rows.len(), 10);
    for (idx, (rowid, _)) in rows.iter().enumerate() {
        assert_eq!(*rowid, (idx as i64 * 2) + 1); // 1,3,5,...,19
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Pager edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn test_pager_memory_allocate_many_pages() {
    let mut pager = Pager::open_memory();
    // Allocate many pages
    for _ in 0..100 {
        let _ = pager.allocate_page().unwrap();
    }
}

#[test]
fn test_page_data_persistence_memory() {
    let mut pager = Pager::open_memory();
    let pg = pager.allocate_page().unwrap();
    {
        let page = pager.get_page_mut(pg).unwrap();
        page.data[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
    }
    {
        let page = pager.get_page(pg).unwrap();
        assert_eq!(&page.data[0..4], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }
}

