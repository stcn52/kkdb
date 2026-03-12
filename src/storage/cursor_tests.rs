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
    assert_eq!(Cursor::header_offset(1), 0);
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

// ── Corruption error path tests ──────────────────────────────────────────

#[test]
fn test_cursor_table_start_invalid_page_header() {
    // Create a valid table, then corrupt the root page header so that
    // Cursor::table_start cannot parse the cell_count field.
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        let r = btree.create_table().unwrap();
        // Insert one row so the leaf has a cell.
        btree.insert(r, 1, &make_row(1, "A")).unwrap();
        r
    };

    // Corrupt the page: set page_type byte to an invalid value (0xFF)
    // and zero out the first 10 bytes so the slice conversions still have
    // well-defined length but the page_type doesn't match any known type.
    // Cursor::move_to_leftmost reads data[off] to decide INTERIOR_TABLE
    // or fall through; with a non-interior page it treats it as a leaf and
    // tries to read cell_count. We make the page too short for the header
    // by setting page type to INTERIOR_TABLE (0x05) but providing a valid
    // cell_count that points to an out-of-bounds cell pointer.
    {
        let page = pager.get_page_mut(root).unwrap();
        // Set as interior page
        page.data[0] = 0x05; // INTERIOR_TABLE
                             // cell_count = 1
        page.data[1..3].copy_from_slice(&1u16.to_le_bytes());
        // cell_content_offset left as is
        // right_child = 0 (invalid page)
        page.data[6..10].copy_from_slice(&0u32.to_le_bytes());
        // First cell pointer at offset INTERIOR_HEADER_SIZE (10) = points to offset 9999
        // which is beyond the page, so reading child page pointer will use bad data.
        page.data[10..12].copy_from_slice(&9999u16.to_le_bytes());
    }

    // Cursor::table_start → move_to_leftmost on this corrupt interior page
    // should attempt to read a child page pointer from offset 9999,
    // which is out of bounds, which may panic or return corrupt data.
    // Since PAGE_SIZE is typically 4096, offset 9999 is out of bounds.
    // The try_into().map_err() won't trigger because the slice indexing
    // itself will panic. But with cell pointer pointing within page bounds
    // to a region with corrupt data we can trigger an error when it tries
    // to get_page on the bogus child page number.
    {
        let page = pager.get_page_mut(root).unwrap();
        // Point the cell pointer to offset 20 (within page bounds)
        page.data[10..12].copy_from_slice(&20u16.to_le_bytes());
        // Write a bogus child page number at offset 20: page 0 (invalid)
        page.data[20..24].copy_from_slice(&0u32.to_le_bytes());
    }

    // get_page(0) should fail
    let result = Cursor::table_start(&mut pager, root);
    assert!(
        result.is_err(),
        "expected error for corrupt interior page pointing to page 0"
    );
}

#[test]
fn test_cursor_current_corrupt_cell_pointer() {
    // Create a valid table with one row, then corrupt the cell so that
    // `current()` reads garbage for payload data and fails during deserialization.
    let mut pager = Pager::open_memory();
    let root = {
        let mut btree = BTree::new(&mut pager);
        let r = btree.create_table().unwrap();
        btree.insert(r, 1, &make_row(1, "Hello")).unwrap();
        r
    };

    // First, verify cursor works normally
    {
        let cursor = Cursor::table_start(&mut pager, root).unwrap();
        let (rid, _) = cursor.current(&mut pager).unwrap();
        assert_eq!(rid, 1);
    }

    // Corrupt the cell pointer to point to offset 100, and set up the cell data
    // there with the OVERFLOW_FLAG in payload_size. This makes current() try
    // to follow an overflow chain with a bogus page number, causing an error.
    {
        let page = pager.get_page_mut(root).unwrap();
        // LEAF_HEADER_SIZE = 14, first cell pointer at offset 14.
        // Point it to offset 100 where we craft a corrupt cell.
        page.data[14..16].copy_from_slice(&100u16.to_le_bytes());
        // At offset 100, write raw_payload_size with OVERFLOW_FLAG set
        // (bit 31 set, inline len = 0)
        page.data[100..104].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        // rowid at offset 104..112 — write rowid=1
        page.data[104..112].copy_from_slice(&1i64.to_le_bytes());
        // inline_start = 112
        // overflow total_len at offset 112..116 — write 1000
        page.data[112..116].copy_from_slice(&1000u32.to_le_bytes());
        // overflow first page at offset 116..120 — write page 9999 (invalid)
        page.data[116..120].copy_from_slice(&9999u32.to_le_bytes());
    }

    let cursor = Cursor::table_start(&mut pager, root).unwrap();
    let result = cursor.current(&mut pager);
    // Reading overflow page 9999 should fail since it doesn't exist
    assert!(
        result.is_err(),
        "expected error for corrupt overflow page in current()"
    );
}
