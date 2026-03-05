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
    for i in 0..rows.len() {
        assert_eq!(rows[i].0, (i + 1) as i64);
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
    for i in 0..rows.len() {
        assert_eq!(rows[i].0, (i + 1) as i64);
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
    for i in 0..rows.len() {
        assert_eq!(rows[i].0, (i + 1) as i64);
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
