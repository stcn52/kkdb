use super::*;
use std::io::Write;
use std::process::Command;

// ---- Page ----

#[test]
fn test_page_new() {
    let page = Page::new();
    assert!(!page.dirty);
    assert_eq!(page.data[0], 0);
    assert_eq!(page.data[PAGE_SIZE - 1], 0);
}

// ---- SuperblockV2 ----

#[test]
fn test_superblock_v2_serialize_deserialize() {
    let sb = SuperblockV2 {
        format_version: 2,
        page_size: PAGE_SIZE as u16,
        flags: 7,
        generation: 42,
        db_uuid: [0xAB; 16],
        schema_root: 9,
        free_root: 10,
        pending_free_root: 11,
        page_count: 128,
        checksum: 0,
    };
    let mut page = [0u8; PAGE_SIZE];
    sb.serialize(&mut page).unwrap();
    let decoded = SuperblockV2::deserialize(&page).unwrap();
    assert_eq!(decoded.format_version, 2);
    assert_eq!(decoded.page_size, PAGE_SIZE as u16);
    assert_eq!(decoded.flags, 7);
    assert_eq!(decoded.generation, 42);
    assert_eq!(decoded.db_uuid, [0xAB; 16]);
    assert_eq!(decoded.schema_root, 9);
    assert_eq!(decoded.free_root, 10);
    assert_eq!(decoded.pending_free_root, 11);
    assert_eq!(decoded.page_count, 128);
}

#[test]
fn test_superblock_v2_checksum_mismatch() {
    let mut page = [0u8; PAGE_SIZE];
    SuperblockV2::new([0x11; 16]).serialize(&mut page).unwrap();
    page[24] ^= 0x5A; // generation byte
    match SuperblockV2::deserialize(&page) {
        Err(KkdbError::CorruptDatabase(msg)) => assert!(msg.contains("checksum")),
        Err(other) => panic!("expected checksum mismatch, got {}", other),
        Ok(_) => panic!("expected checksum mismatch"),
    }
}

fn failpoint_name(failpoint: PagerFailpoint) -> &'static str {
    match failpoint {
        PagerFailpoint::AfterDataPagesWrite => "after_data_pages_write",
        PagerFailpoint::AfterDataPagesSync => "after_data_pages_sync",
        PagerFailpoint::AfterSuperblockWrite => "after_superblock_write",
        PagerFailpoint::AfterSuperblockSync => "after_superblock_sync",
    }
}

fn parse_failpoint_name(name: &str) -> PagerFailpoint {
    match name {
        "after_data_pages_write" => PagerFailpoint::AfterDataPagesWrite,
        "after_data_pages_sync" => PagerFailpoint::AfterDataPagesSync,
        "after_superblock_write" => PagerFailpoint::AfterSuperblockWrite,
        "after_superblock_sync" => PagerFailpoint::AfterSuperblockSync,
        other => panic!("unknown failpoint name: {}", other),
    }
}

// ---- Pager (in-memory) ----

#[test]
fn test_pager_open_memory() {
    let pager = Pager::open_memory();
    assert!(pager.is_memory);
    assert_eq!(pager.format(), PagerFormat::V2);
    assert_eq!(pager.schema_root_page(), 3);
    assert_eq!(pager.header.total_pages, 3);
}

#[test]
fn test_pager_get_page() {
    let mut pager = Pager::open_memory();
    let page = pager.get_page(1).unwrap();
    // Page 1 should contain v2 superblock magic.
    assert_eq!(&page.data[0..16], COW_MAGIC);
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
        let page = pager.get_page_mut(3).unwrap();
        assert!(page.dirty);
        page.data[200] = 0xAB;
    }
    let data = pager.get_page_data(3).unwrap();
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
    assert_eq!(pager.header.total_pages, 3);
    let page_num = pager.allocate_page().unwrap();
    assert_eq!(page_num, 4);
    assert_eq!(pager.header.total_pages, 4);
    // Should be able to read the new page
    let page = pager.get_page(4).unwrap();
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
    assert_eq!(&data[0..16], COW_MAGIC);
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
        assert_eq!(pager.format(), PagerFormat::V2);
        assert_eq!(pager.schema_root_page(), 3);
        assert_eq!(pager.header.total_pages, 3);
        let p4 = pager.allocate_page().unwrap();
        assert_eq!(p4, 4);
        {
            let page = pager.get_page_mut(4).unwrap();
            page.data[0] = 0x42;
        }
        pager.flush().unwrap();
    }

    // Reopen existing
    {
        let pager = Pager::open(&path).unwrap();
        assert_eq!(pager.format(), PagerFormat::V2);
        assert_eq!(pager.header.total_pages, 4);
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
        let page = pager.get_page_mut(3).unwrap();
        assert!(page.dirty);
    }
    pager.flush().unwrap();
    // After flush, dirty should be cleared
    let page = pager.get_page(3).unwrap();
    assert!(!page.dirty);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_pager_read_page_from_disk_memory() {
    let mut pager = Pager::open_memory();
    // Allocate a page – for in-memory, pages are already loaded
    pager.allocate_page().unwrap();
    let page = pager.get_page(4).unwrap();
    assert_eq!(page.data[0], 0); // empty page for in-memory
}

#[test]
fn test_pager_short_read_returns_corrupt_database() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_short_read_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    // Superblock claims page_count=4 while file has only 3 pages.
    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        let mut page2 = [0u8; PAGE_SIZE];
        let mut page3 = [0u8; PAGE_SIZE];
        let mut sb1 = SuperblockV2::new([0x44; 16]);
        sb1.page_count = 4;
        sb1.serialize(&mut page1).unwrap();
        let mut sb2 = sb1.clone();
        sb2.generation = 0;
        sb2.serialize(&mut page2).unwrap();
        page3[0] = 0x0D;
        page3[3..5].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        file.write_all(&page1).unwrap();
        file.write_all(&page2).unwrap();
        file.write_all(&page3).unwrap();
        file.flush().unwrap();
    }

    match Pager::open(&path) {
        Err(KkdbError::CorruptDatabase(msg)) => {
            assert!(msg.contains("does not fit file len") || msg.contains("openable superblock"))
        }
        Err(other) => panic!("expected CorruptDatabase, got {}", other),
        Ok(_) => panic!("expected open error for short v2 file"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_pager_open_v2_format_supported() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_pager_open_v2_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        let mut page2 = [0u8; PAGE_SIZE];
        let mut page3 = [0u8; PAGE_SIZE];
        let mut sb1 = SuperblockV2::new([0x22; 16]);
        sb1.generation = 3;
        sb1.page_count = 3;
        sb1.serialize(&mut page1).unwrap();
        let mut sb2 = sb1.clone();
        sb2.generation = 2;
        sb2.serialize(&mut page2).unwrap();
        page3[0] = 0x0D;
        page3[3..5].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        file.write_all(&page1).unwrap();
        file.write_all(&page2).unwrap();
        file.write_all(&page3).unwrap();
        file.flush().unwrap();
    }

    match Pager::open(&path) {
        Ok(pager) => {
            assert_eq!(pager.format(), PagerFormat::V2);
            assert_eq!(pager.schema_root_page(), 3);
        }
        Err(other) => panic!("expected v2 format to be accepted, got {}", other),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_pager_open_rejects_legacy_v1_magic() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_pager_open_rejects_legacy_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        page1[0..16].copy_from_slice(b"KKDB not v2 fmt!");
        file.write_all(&page1).unwrap();
        file.flush().unwrap();
    }

    match Pager::open(&path) {
        Err(KkdbError::RuntimeError(msg)) => {
            assert!(msg.contains("only format v2 is supported"))
        }
        Err(other) => panic!("expected RuntimeError, got {}", other),
        Ok(_) => panic!("expected non-v2 open to be rejected"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_read_active_superblock_v2_prefers_higher_generation() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_active_superblock_choice_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        let mut page2 = [0u8; PAGE_SIZE];
        let mut sb1 = SuperblockV2::new([0x33; 16]);
        sb1.generation = 8;
        sb1.serialize(&mut page1).unwrap();
        let mut sb2 = sb1.clone();
        sb2.generation = 9;
        sb2.serialize(&mut page2).unwrap();
        file.write_all(&page1).unwrap();
        file.write_all(&page2).unwrap();
        file.flush().unwrap();
    }

    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let (active, _) = Pager::read_active_superblock_v2_with_slot(&mut file).unwrap();
    assert_eq!(active.generation, 9);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_open_cow_v2_falls_back_when_higher_generation_does_not_fit_file() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_open_cow_v2_fallback_slot_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        let mut page2 = [0u8; PAGE_SIZE];
        let mut page3 = [0u8; PAGE_SIZE];

        let mut sb_a = SuperblockV2::new([0x66; 16]);
        sb_a.generation = 5;
        sb_a.page_count = 3;
        sb_a.serialize(&mut page1).unwrap();

        let mut sb_b = sb_a.clone();
        sb_b.generation = 6;
        sb_b.page_count = 9;
        sb_b.serialize(&mut page2).unwrap();

        page3[0] = 0x0D;
        page3[3..5].copy_from_slice(&(PAGE_SIZE as u16).to_le_bytes());
        file.write_all(&page1).unwrap();
        file.write_all(&page2).unwrap();
        file.write_all(&page3).unwrap();
        file.flush().unwrap();
    }

    let pager = Pager::open_cow_v2(&path).unwrap();
    let state = pager.cow_state.as_ref().unwrap();
    assert_eq!(state.active_slot, SuperblockSlot::A);
    assert_eq!(state.active_superblock.generation, 5);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_open_cow_v2_rejects_superblock_uuid_mismatch() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_open_cow_v2_uuid_mismatch_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    {
        let mut file = std::fs::File::create(&path).unwrap();
        let mut page1 = [0u8; PAGE_SIZE];
        let mut page2 = [0u8; PAGE_SIZE];
        let mut sb1 = SuperblockV2::new([0xAA; 16]);
        sb1.generation = 10;
        sb1.page_count = 3;
        sb1.serialize(&mut page1).unwrap();

        let mut sb2 = SuperblockV2::new([0xBB; 16]);
        sb2.generation = 11;
        sb2.page_count = 3;
        sb2.serialize(&mut page2).unwrap();

        file.write_all(&page1).unwrap();
        file.write_all(&page2).unwrap();
        file.flush().unwrap();
    }

    match Pager::open_cow_v2(&path) {
        Err(KkdbError::CorruptDatabase(msg)) => assert!(msg.contains("UUID mismatch")),
        Err(other) => panic!("expected CorruptDatabase, got {}", other),
        Ok(_) => panic!("expected UUID mismatch to be rejected"),
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_create_and_open_cow_v2_roundtrip() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_create_open_cow_v2_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    {
        let pager = Pager::create_cow_v2(&path).unwrap();
        assert_eq!(pager.format(), PagerFormat::V2);
        assert_eq!(pager.header.total_pages, 3);
        let state = pager.cow_state.as_ref().unwrap();
        assert_eq!(state.active_slot, SuperblockSlot::A);
        assert_eq!(state.active_superblock.generation, 1);
        assert_eq!(state.active_superblock.page_count, 3);
    }

    {
        let mut pager = Pager::open_cow_v2(&path).unwrap();
        assert_eq!(pager.format(), PagerFormat::V2);
        let state = pager.cow_state.as_ref().unwrap();
        assert_eq!(state.active_slot, SuperblockSlot::A);
        assert_eq!(state.active_superblock.generation, 1);
        assert_eq!(state.active_superblock.page_count, 3);
        let schema_page = pager.get_page(3).unwrap();
        assert_eq!(schema_page.data[0], 0x0D);
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_begin_transaction_sets_txid_and_generation() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_cow_v2_begin_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::create_cow_v2(&path).unwrap();
    pager.begin_transaction().unwrap();
    let state = pager.cow_state.as_ref().unwrap();
    let tx = state.active_tx.as_ref().unwrap();
    assert_eq!(tx.txid, 2);
    assert_eq!(tx.base_generation, 1);
    assert_eq!(tx.target_generation, 2);
    assert!(pager.txn_snapshot.is_some());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_commit_switches_slot_and_persists_data() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_cow_v2_commit_switch_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    {
        let mut pager = Pager::create_cow_v2(&path).unwrap();
        pager.begin_transaction().unwrap();
        {
            let page = pager.get_page_mut(3).unwrap();
            page.data[300] = 0x5A;
        }
        pager.commit_transaction().unwrap();

        let state = pager.cow_state.as_ref().unwrap();
        assert_eq!(state.active_slot, SuperblockSlot::B);
        assert_eq!(state.active_superblock.generation, 2);
        assert!(state.active_tx.is_none());
        assert!(pager.txn_snapshot.is_none());
    }

    {
        let mut reopened = Pager::open_cow_v2(&path).unwrap();
        let state = reopened.cow_state.as_ref().unwrap();
        assert_eq!(state.active_slot, SuperblockSlot::B);
        assert_eq!(state.active_superblock.generation, 2);
        let page = reopened.get_page(3).unwrap();
        assert_eq!(page.data[300], 0x5A);
    }

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_commit_failure_keeps_state_for_rollback() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_cow_v2_commit_fail_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::create_cow_v2(&path).unwrap();
    pager.begin_transaction().unwrap();
    {
        let page = pager.get_page_mut(3).unwrap();
        page.data[128] = 0x7F;
    }

    let ro_file = std::fs::OpenOptions::new().read(true).open(&path).unwrap();
    pager.file = Some(ro_file);

    match pager.commit_transaction() {
        Err(KkdbError::Io(_)) => {}
        Err(other) => panic!("expected IO error, got {}", other),
        Ok(_) => panic!("expected commit failure"),
    }

    assert!(pager.txn_snapshot.is_some());
    assert!(pager
        .cow_state
        .as_ref()
        .and_then(|s| s.active_tx.as_ref())
        .is_some());
    pager.rollback_transaction().unwrap();
    assert!(pager.txn_snapshot.is_none());
    assert!(pager
        .cow_state
        .as_ref()
        .and_then(|s| s.active_tx.as_ref())
        .is_none());

    drop(pager);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_rollback_restores_page_snapshot() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!("test_cow_v2_rollback_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::create_cow_v2(&path).unwrap();
    let original = {
        let page = pager.get_page(3).unwrap();
        page.data[256]
    };

    pager.begin_transaction().unwrap();
    {
        let page = pager.get_page_mut(3).unwrap();
        page.data[256] = original ^ 0x5A;
    }
    pager.rollback_transaction().unwrap();

    let page = pager.get_page(3).unwrap();
    assert_eq!(page.data[256], original);
    assert!(pager
        .cow_state
        .as_ref()
        .and_then(|s| s.active_tx.as_ref())
        .is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_commit_rejects_dirty_superblock_pages() {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "test_cow_v2_dirty_superblock_{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);

    let mut pager = Pager::create_cow_v2(&path).unwrap();
    pager.begin_transaction().unwrap();
    {
        let page = pager.get_page_mut(1).unwrap();
        page.data[80] ^= 0x01;
    }

    match pager.commit_transaction() {
        Err(KkdbError::RuntimeError(msg)) => assert!(msg.contains("reserves page 1/2")),
        Err(other) => panic!("expected RuntimeError, got {}", other),
        Ok(_) => panic!("expected commit to reject dirty superblock pages"),
    }

    assert!(pager.txn_snapshot.is_some());
    pager.rollback_transaction().unwrap();
    assert!(pager.txn_snapshot.is_none());

    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_cow_v2_memory_commit_increments_generation() {
    let mut pager = Pager::open_memory();
    assert_eq!(pager.format(), PagerFormat::V2);

    pager.begin_transaction().unwrap();
    {
        let page = pager.get_page_mut(3).unwrap();
        page.data[42] = 0xA5;
    }
    pager.commit_transaction().unwrap();

    let state = pager.cow_state.as_ref().unwrap();
    assert_eq!(state.active_superblock.generation, 2);
    assert!(state.active_tx.is_none());
    let page = pager.get_page(3).unwrap();
    assert_eq!(page.data[42], 0xA5);
}

#[test]
fn test_cow_v2_failpoints_keep_state_for_rollback() {
    let failpoints = [
        PagerFailpoint::AfterDataPagesWrite,
        PagerFailpoint::AfterDataPagesSync,
        PagerFailpoint::AfterSuperblockWrite,
        PagerFailpoint::AfterSuperblockSync,
    ];

    for failpoint in failpoints {
        let mut pager = Pager::open_memory();
        let original = {
            let page = pager.get_page(3).unwrap();
            page.data[512]
        };

        pager.begin_transaction().unwrap();
        {
            let page = pager.get_page_mut(3).unwrap();
            page.data[512] = original ^ 0x7A;
        }

        pager.set_failpoint(Some(failpoint));
        match pager.commit_transaction() {
            Err(KkdbError::RuntimeError(msg)) => {
                assert!(msg.contains("injected pager failpoint"), "{:?}", failpoint)
            }
            Err(other) => panic!("expected RuntimeError, got {} ({:?})", other, failpoint),
            Ok(_) => panic!("expected injected failure for {:?}", failpoint),
        }

        assert!(pager.txn_snapshot.is_some(), "{:?}", failpoint);
        assert!(pager
            .cow_state
            .as_ref()
            .and_then(|s| s.active_tx.as_ref())
            .is_some());

        pager.rollback_transaction().unwrap();
        assert!(pager.txn_snapshot.is_none(), "{:?}", failpoint);
        assert!(pager
            .cow_state
            .as_ref()
            .and_then(|s| s.active_tx.as_ref())
            .is_none());

        let page = pager.get_page(3).unwrap();
        assert_eq!(page.data[512], original, "{:?}", failpoint);
    }
}

#[test]
fn test_cow_v2_file_failpoints_reopen_and_continue() {
    let failpoints = [
        PagerFailpoint::AfterDataPagesWrite,
        PagerFailpoint::AfterDataPagesSync,
        PagerFailpoint::AfterSuperblockWrite,
        PagerFailpoint::AfterSuperblockSync,
    ];

    for (idx, failpoint) in failpoints.iter().enumerate() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_cow_v2_file_failpoint_{}_{}.db",
            std::process::id(),
            idx
        ));
        let _ = std::fs::remove_file(&path);

        {
            let mut pager = Pager::create_cow_v2(&path).unwrap();
            let old = {
                let page = pager.get_page(3).unwrap();
                page.data[640]
            };
            let newv = old ^ 0x5D;

            pager.begin_transaction().unwrap();
            {
                let page = pager.get_page_mut(3).unwrap();
                page.data[640] = newv;
            }
            pager.set_failpoint(Some(*failpoint));
            match pager.commit_transaction() {
                Err(KkdbError::RuntimeError(msg)) => {
                    assert!(msg.contains("injected pager failpoint"), "{:?}", failpoint)
                }
                Err(other) => panic!("expected RuntimeError, got {} ({:?})", other, failpoint),
                Ok(_) => panic!("expected injected failure for {:?}", failpoint),
            }
            // Intentionally skip rollback to simulate crash/restart flow.
        }

        {
            let mut reopened = Pager::open_cow_v2(&path).unwrap();
            let gen = reopened
                .cow_state
                .as_ref()
                .map(|s| s.active_superblock.generation)
                .unwrap_or(0);
            assert!(gen == 1 || gen == 2, "{:?}, gen={}", failpoint, gen);

            // Restarted DB remains readable and can continue committing.
            reopened.begin_transaction().unwrap();
            {
                let page = reopened.get_page_mut(3).unwrap();
                page.data[641] ^= 0xA3;
            }
            reopened.commit_transaction().unwrap();
            let gen2 = reopened
                .cow_state
                .as_ref()
                .map(|s| s.active_superblock.generation)
                .unwrap_or(0);
            assert!(gen2 >= gen, "{:?}, {} -> {}", failpoint, gen, gen2);
        }

        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn test_cow_v2_abort_child_only() {
    if std::env::var("KKDB_ABORT_CHILD").ok().as_deref() != Some("1") {
        return;
    }

    let db_path =
        std::env::var("KKDB_ABORT_DB_PATH").expect("KKDB_ABORT_DB_PATH must be set in child");
    let failpoint = parse_failpoint_name(
        &std::env::var("KKDB_ABORT_FAILPOINT").expect("KKDB_ABORT_FAILPOINT must be set in child"),
    );

    let mut pager = Pager::open_cow_v2(&db_path).unwrap();
    pager.begin_transaction().unwrap();
    {
        let page = pager.get_page_mut(3).unwrap();
        page.data[777] ^= 0x3C;
    }
    pager.set_failpoint(Some(failpoint));
    pager.set_failpoint_action(PagerFailAction::AbortProcess);

    let _ = pager.commit_transaction();
    panic!("child should have aborted at {:?}", failpoint);
}

#[test]
fn test_cow_v2_abort_failpoints_reopen_and_continue() {
    let failpoints = [
        PagerFailpoint::AfterDataPagesWrite,
        PagerFailpoint::AfterDataPagesSync,
        PagerFailpoint::AfterSuperblockWrite,
        PagerFailpoint::AfterSuperblockSync,
    ];

    let exe = std::env::current_exe().expect("current_exe");
    let child_test_name = "storage::pager::tests::test_cow_v2_abort_child_only";

    for (idx, failpoint) in failpoints.iter().enumerate() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "test_cow_v2_abort_failpoint_{}_{}.db",
            std::process::id(),
            idx
        ));
        let _ = std::fs::remove_file(&path);

        {
            let _pager = Pager::create_cow_v2(&path).unwrap();
        }

        let status = Command::new(&exe)
            .arg("--nocapture")
            .arg("--test-threads=1")
            .arg("--exact")
            .arg(child_test_name)
            .env("KKDB_ABORT_CHILD", "1")
            .env("KKDB_ABORT_DB_PATH", path.to_string_lossy().to_string())
            .env("KKDB_ABORT_FAILPOINT", failpoint_name(*failpoint))
            .status()
            .expect("spawn child test process");
        assert!(
            !status.success(),
            "child should crash for {:?}, status={:?}",
            failpoint,
            status
        );

        let mut reopened = Pager::open_cow_v2(&path).unwrap();
        let gen = reopened
            .cow_state
            .as_ref()
            .map(|s| s.active_superblock.generation)
            .unwrap_or(0);
        assert!(gen == 1 || gen == 2, "{:?}, gen={}", failpoint, gen);

        reopened.begin_transaction().unwrap();
        {
            let page = reopened.get_page_mut(3).unwrap();
            page.data[778] ^= 0xA7;
        }
        reopened.commit_transaction().unwrap();
        let gen2 = reopened
            .cow_state
            .as_ref()
            .map(|s| s.active_superblock.generation)
            .unwrap_or(0);
        assert!(gen2 >= gen, "{:?}, {} -> {}", failpoint, gen, gen2);

        let _ = std::fs::remove_file(&path);
    }
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
        let page = pager.get_page_mut(3).unwrap();
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
