use super::*;
use std::io::Write;

// ---- Page ----

#[test]
fn test_page_new() {
    let page = Page::new();
    assert!(!page.dirty);
    assert_eq!(page.data[0], 0);
    assert_eq!(page.data[PAGE_SIZE - 1], 0);
}

// ---- DbHeader ----

#[test]
fn test_db_header_new() {
    let hdr = DbHeader::new();
    assert_eq!(hdr.page_size, PAGE_SIZE as u16);
    assert_eq!(hdr.total_pages, 1);
    assert_eq!(hdr.first_freelist_page, 0);
    assert_eq!(hdr.freelist_count, 0);
    assert_eq!(hdr.schema_version, 0);
}

#[test]
fn test_db_header_serialize_deserialize() {
    let hdr = DbHeader {
        page_size: 4096,
        total_pages: 10,
        first_freelist_page: 3,
        freelist_count: 2,
        schema_version: 5,
    };
    let mut buf = [0u8; DB_HEADER_SIZE];
    hdr.serialize(&mut buf);
    let hdr2 = DbHeader::deserialize(&buf).unwrap();
    assert_eq!(hdr2.page_size, 4096);
    assert_eq!(hdr2.total_pages, 10);
    assert_eq!(hdr2.first_freelist_page, 3);
    assert_eq!(hdr2.freelist_count, 2);
    assert_eq!(hdr2.schema_version, 5);
}

#[test]
fn test_db_header_deserialize_too_short() {
    let buf = [0u8; 10];
    assert!(DbHeader::deserialize(&buf).is_err());
}

#[test]
fn test_db_header_deserialize_bad_magic() {
    let mut buf = [0u8; DB_HEADER_SIZE];
    buf[0..16].copy_from_slice(b"WRONG MAGIC\0\0\0\0\0");
    assert!(DbHeader::deserialize(&buf).is_err());
}

// ---- Pager (in-memory) ----

#[test]
fn test_pager_open_memory() {
    let pager = Pager::open_memory();
    assert!(pager.is_memory);
    assert_eq!(pager.header.total_pages, 1);
}

#[test]
fn test_pager_get_page() {
    let mut pager = Pager::open_memory();
    let page = pager.get_page(1).unwrap();
    // Page 1 should have magic header
    assert_eq!(&page.data[0..16], MAGIC);
}

#[test]
fn test_pager_get_page_out_of_range_zero() {
    let mut pager = Pager::open_memory();
    assert!(pager.get_page(0).is_err());
}

#[test]
fn test_pager_get_page_out_of_range_high() {
    let mut pager = Pager::open_memory();
    assert!(pager.get_page(999).is_err());
}

#[test]
fn test_pager_get_page_mut() {
    let mut pager = Pager::open_memory();
    {
        let page = pager.get_page_mut(1).unwrap();
        assert!(page.dirty);
        page.data[200] = 0xAB;
    }
    let data = pager.get_page_data(1).unwrap();
    assert_eq!(data[200], 0xAB);
}

#[test]
fn test_pager_get_page_mut_out_of_range() {
    let mut pager = Pager::open_memory();
    assert!(pager.get_page_mut(0).is_err());
    assert!(pager.get_page_mut(999).is_err());
}

#[test]
fn test_pager_allocate_page() {
    let mut pager = Pager::open_memory();
    assert_eq!(pager.header.total_pages, 1);
    let page_num = pager.allocate_page().unwrap();
    assert_eq!(page_num, 2);
    assert_eq!(pager.header.total_pages, 2);
    // Should be able to read the new page
    let page = pager.get_page(2).unwrap();
    assert_eq!(page.data[0], 0);
}

#[test]
fn test_pager_allocate_page_limit() {
    let mut pager = Pager::open_memory();
    pager.header.total_pages = MAX_PAGES;
    assert!(pager.allocate_page().is_err());
}

#[test]
fn test_pager_flush_memory() {
    let mut pager = Pager::open_memory();
    // Flush on in-memory DB should succeed (no file to write)
    assert!(pager.flush().is_ok());
}

#[test]
fn test_pager_get_page_data() {
    let mut pager = Pager::open_memory();
    let data = pager.get_page_data(1).unwrap();
    assert_eq!(&data[0..16], MAGIC);
}

// ---- Pager (file-based) ----

#[test]
fn test_pager_open_file_create_and_reopen() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_create_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // Create new
    {
        let mut pager = Pager::open(&path).unwrap();
        assert!(!pager.is_memory);
        assert_eq!(pager.header.total_pages, 1);
        // Load page 1 into cache so flush can serialize the header
        let _ = pager.get_page(1).unwrap();
        let p2 = pager.allocate_page().unwrap();
        assert_eq!(p2, 2);
        {
            let page = pager.get_page_mut(2).unwrap();
            page.data[0] = 0x42;
        }
        pager.flush().unwrap();
    }

    // Reopen existing
    {
        let pager = Pager::open(&path).unwrap();
        assert_eq!(pager.header.total_pages, 2);
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_pager_flush_clears_dirty() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_flush_dirty_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::open(&path).unwrap();
    {
        let page = pager.get_page_mut(1).unwrap();
        assert!(page.dirty);
    }
    pager.flush().unwrap();
    // After flush, dirty should be cleared
    let page = pager.get_page(1).unwrap();
    assert!(!page.dirty);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_pager_read_page_from_disk_memory() {
    let mut pager = Pager::open_memory();
    // Allocate a page — for in-memory, pages are already loaded
    pager.allocate_page().unwrap();
    let page = pager.get_page(2).unwrap();
    assert_eq!(page.data[0], 0); // empty page for in-memory
}

#[test]
fn test_pager_short_read_returns_corrupt_database() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_short_read_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // File says total_pages=2 but contains only page 1.
    {
        let mut file = std::fs::File::create(&path).unwrap();
        let hdr = DbHeader {
            page_size: PAGE_SIZE as u16,
            total_pages: 2,
            first_freelist_page: 0,
            freelist_count: 0,
            schema_version: 0,
        };
        let mut page = [0u8; PAGE_SIZE];
        hdr.serialize(&mut page);
        let offset = DB_HEADER_SIZE;
        page[offset] = 0x0D;
        page[offset + 1..offset + 3].copy_from_slice(&0u16.to_le_bytes());
        page[offset + 3..offset + 5].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        page[offset + 5] = 0;
        file.write_all(&page).unwrap();
        file.flush().unwrap();
    }

    let mut pager = Pager::open(&path).unwrap();
    match pager.get_page(2) {
        Err(KkdbError::CorruptDatabase(_)) => {}
        Err(other) => panic!("expected CorruptDatabase, got {}", other),
        Ok(_) => panic!("expected error for short read"),
    }

    let _ = std::fs::remove_file(&path);
}

// ---- Transaction semantics ----

#[test]
fn test_commit_failure_keeps_snapshot_for_rollback() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_commit_fail_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::open(&path).unwrap();
    pager.begin_transaction().unwrap();

    // Make sure we have dirty data to flush on COMMIT.
    {
        let page = pager.get_page_mut(1).unwrap();
        page.data[200] = 0x7F;
    }

    // Replace RW handle with RO handle so write during flush fails.
    let ro_file = std::fs::OpenOptions::new().read(true).open(&path).unwrap();
    pager.file = Some(ro_file);

    match pager.commit_transaction() {
        Err(KkdbError::Io(_)) => {}
        Err(other) => panic!("expected IO error, got {}", other),
        Ok(_) => panic!("expected commit failure"),
    }

    // Snapshot must remain so caller can still rollback.
    assert!(pager.txn_snapshot.is_some());
    pager.rollback_transaction().unwrap();
    assert!(pager.txn_snapshot.is_none());

    drop(pager);
    let _ = std::fs::remove_file(&path);
}
