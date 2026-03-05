use super::*;
use crate::storage::btree::BTree;
use crate::types::Value;

fn make_row(id: i64, name: &str) -> Vec<Value> {
    vec![Value::Integer(id), Value::Text(name.into())]
}

fn make_big_row(id: i64) -> Vec<Value> {
    let big_name = "X".repeat(180);
    vec![Value::Integer(id), Value::Text(big_name.into())]
}

#[test]
fn test_cursor_empty_table() {
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let cursor = Cursor::table_start(&mut pager, root).unwrap();
    assert!(cursor.end_of_table);
}

#[test]
fn test_cursor_single_row() {
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    {
        let mut btree = BTree::new(&mut pager);
        btree.insert(root, 1, &make_row(1, "Alice")).unwrap();
    }
    let mut cursor = Cursor::table_start(&mut pager, root).unwrap();
    assert!(!cursor.end_of_table);

    let (rowid, row) = cursor.current(&mut pager).unwrap();
    assert_eq!(rowid, 1);
    assert_eq!(row[1], Value::Text("Alice".into()));

    cursor.advance(&mut pager).unwrap();
    assert!(cursor.end_of_table);
}

#[test]
fn test_cursor_multiple_rows() {
    let mut pager = Pager::open_memory();
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

    let mut cursor = Cursor::table_start(&mut pager, root).unwrap();
    let mut collected = Vec::new();
    while !cursor.end_of_table {
        let (rowid, _row) = cursor.current(&mut pager).unwrap();
        collected.push(rowid);
        cursor.advance(&mut pager).unwrap();
    }
    assert_eq!(collected, vec![1, 2, 3]);
}

#[test]
fn test_cursor_current_past_end() {
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let cursor = Cursor::table_start(&mut pager, root).unwrap();
    assert!(cursor.end_of_table);
    assert!(cursor.current(&mut pager).is_err());
}

#[test]
fn test_cursor_advance_past_end_noop() {
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut cursor = Cursor::table_start(&mut pager, root).unwrap();
    assert!(cursor.end_of_table);
    // Advancing past end should be a no-op
    cursor.advance(&mut pager).unwrap();
    assert!(cursor.end_of_table);
}

#[test]
fn test_cursor_with_split_tree() {
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    // Use big rows to trigger actual page splits
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }

    let mut cursor = Cursor::table_start(&mut pager, current_root).unwrap();
    let mut count = 0;
    let mut prev_rowid = 0;
    while !cursor.end_of_table {
        let (rowid, _) = cursor.current(&mut pager).unwrap();
        assert!(rowid > prev_rowid, "rows should be in order");
        prev_rowid = rowid;
        count += 1;
        cursor.advance(&mut pager).unwrap();
    }
    assert_eq!(count, 20);
}

#[test]
fn test_header_offset() {
    assert_eq!(Cursor::header_offset(1), DB_HEADER_SIZE);
    assert_eq!(Cursor::header_offset(2), 0);
}

#[test]
fn test_cursor_large_split_tree() {
    // Use big rows to trigger multiple page splits
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=20 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }

    let mut cursor = Cursor::table_start(&mut pager, current_root).unwrap();
    let mut count = 0;
    let mut prev_rowid = 0;
    while !cursor.end_of_table {
        let (rowid, row) = cursor.current(&mut pager).unwrap();
        assert!(rowid > prev_rowid, "rows should be in ascending order");
        assert_eq!(row[0], Value::Integer(rowid));
        prev_rowid = rowid;
        count += 1;
        cursor.advance(&mut pager).unwrap();
    }
    assert_eq!(count, 20);
}

#[test]
fn test_cursor_reverse_insert_order() {
    // Insert in reverse order with big rows to trigger splits
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in (1..=20).rev() {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_big_row(i)).unwrap();
    }

    let mut cursor = Cursor::table_start(&mut pager, current_root).unwrap();
    let mut count = 0;
    let mut prev_rowid = 0;
    while !cursor.end_of_table {
        let (rowid, _) = cursor.current(&mut pager).unwrap();
        assert!(rowid > prev_rowid);
        prev_rowid = rowid;
        count += 1;
        cursor.advance(&mut pager).unwrap();
    }
    assert_eq!(count, 20);
}

#[test]
fn test_cursor_after_delete() {
    // Cursor should work correctly on a tree with deleted rows
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        btree.create_table().unwrap()
    };
    let mut current_root = root;
    for i in 1..=10 {
        let mut btree = BTree::new(&mut pager);
        current_root = btree.insert(current_root, i, &make_row(i, "x")).unwrap();
    }
    // Delete some rows
    {
        let mut btree = BTree::new(&mut pager);
        btree.delete_by_rowid(current_root, 3).unwrap();
        let _ = btree.delete_by_rowid(current_root, 7).unwrap();
    }

    let mut cursor = Cursor::table_start(&mut pager, current_root).unwrap();
    let mut collected = Vec::new();
    while !cursor.end_of_table {
        let (rowid, _) = cursor.current(&mut pager).unwrap();
        collected.push(rowid);
        cursor.advance(&mut pager).unwrap();
    }
    assert_eq!(collected.len(), 8);
    assert!(!collected.contains(&3));
    assert!(!collected.contains(&7));
}
