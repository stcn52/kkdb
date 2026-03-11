//! WAL compaction tests.
//!
//! Verifies that compact() reduces file size, preserves live entries,
//! and that compaction_stats() reports correct counts.

use kkdb::raft::log_store::KkdbLogStore;
use kkdb::raft::types::KkdbTypeConfig;
use openraft::{Entry, EntryPayload, LogId};

fn blank_entry(term: u64, index: u64) -> Entry<KkdbTypeConfig> {
    Entry {
        log_id: LogId {
            leader_id: openraft::LeaderId::new(term, 1),
            index,
        },
        payload: EntryPayload::Blank,
    }
}

// ─── Test 1: compact() on empty WAL (nothing to do) ──────────────────────────

#[test]
fn test_compact_empty_wal() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();
    let eliminated = store.compact().unwrap();
    assert_eq!(eliminated, 0, "compact on empty WAL eliminates 0 records");
}

// ─── Test 2: compact() reduces file size after purge ─────────────────────────

#[test]
fn test_compact_reduces_file_size() {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("raft").join("wal.log");

    let store = KkdbLogStore::open(dir.path()).unwrap();

    // Append 100 entries
    let entries: Vec<_> = (1..=100).map(|i| blank_entry(1, i)).collect();
    store.append_direct(entries).unwrap();

    let size_before = std::fs::metadata(&wal_path).unwrap().len();

    // Purge entries 1-90 (only 10 live entries remain)
    let purge_id = LogId {
        leader_id: openraft::LeaderId::new(1, 1),
        index: 90,
    };
    store.purge_direct(purge_id).unwrap();

    // Compact
    let eliminated = store.compact().unwrap();
    assert!(
        eliminated > 0,
        "should have eliminated dead records, got {}",
        eliminated
    );

    let size_after = std::fs::metadata(&wal_path).unwrap().len();
    assert!(
        size_after < size_before,
        "WAL file size should decrease after compaction: before={} after={}",
        size_before,
        size_after
    );
}

// ─── Test 3: live entries preserved after compact ────────────────────────────

#[test]
fn test_compact_preserves_live_entries() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    // Append 20 entries
    let entries: Vec<_> = (1..=20).map(|i| blank_entry(1, i)).collect();
    store.append_direct(entries).unwrap();

    // Purge first 15
    let purge_id = LogId {
        leader_id: openraft::LeaderId::new(1, 1),
        index: 15,
    };
    store.purge_direct(purge_id).unwrap();

    // Compact
    let eliminated = store.compact().unwrap();
    assert!(eliminated >= 15, "at least 15 dead records eliminated");

    // Verify live entries 16-20 are still in memory
    let last = store.last_index();
    assert_eq!(last, Some(20), "last live entry index must still be 20");
    assert_eq!(
        store.inner.lock().unwrap().log.len(),
        5,
        "5 live entries remain"
    );
}

// ─── Test 4: data correct after compact + reopen ─────────────────────────────

#[test]
fn test_compact_then_reopen() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        let entries: Vec<_> = (1..=10).map(|i| blank_entry(1, i)).collect();
        store.append_direct(entries).unwrap();

        // Purge 1-7, keep 8-10
        store
            .purge_direct(LogId {
                leader_id: openraft::LeaderId::new(1, 1),
                index: 7,
            })
            .unwrap();

        // Manual compact
        let eliminated = store.compact().unwrap();
        assert!(eliminated > 0);
    }

    // Reopen: only 8,9,10 should be present
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        let inner = store.inner.lock().unwrap();
        assert_eq!(
            inner.log.len(),
            3,
            "3 entries (8,9,10) after compact+reopen"
        );
        assert!(inner.log.contains_key(&8));
        assert!(inner.log.contains_key(&9));
        assert!(inner.log.contains_key(&10));
        assert!(!inner.log.contains_key(&7), "purged entry 7 must be gone");
    }
}

// ─── Test 5: compaction_stats() accuracy ─────────────────────────────────────

#[test]
fn test_compaction_stats() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    // Start: all zeros
    let (live, total, dead) = store.compaction_stats();
    assert_eq!((live, total, dead), (0, 0, 0));

    // Append 5 entries
    let entries: Vec<_> = (1..=5).map(|i| blank_entry(1, i)).collect();
    store.append_direct(entries).unwrap();

    let (live, total, dead) = store.compaction_stats();
    assert_eq!(live, 5, "5 live entries");
    assert_eq!(total, 5, "5 total records");
    assert_eq!(dead, 0, "0 dead records");

    // Purge 3 entries
    store
        .purge_direct(LogId {
            leader_id: openraft::LeaderId::new(1, 1),
            index: 3,
        })
        .unwrap();

    let (live, _total, dead) = store.compaction_stats();
    assert_eq!(live, 2, "2 live entries (4,5)");
    assert!(dead > 0, "some dead records expected");

    // After compaction: dead should be 0
    store.compact().unwrap();
    let (live, total, dead) = store.compaction_stats();
    assert_eq!(live, 2);
    assert_eq!(total, live, "total resets to live after compact");
    assert_eq!(dead, 0);
}

// ─── Test 6: compact on in-memory store does nothing ─────────────────────────

#[test]
fn test_compact_in_memory_store_noop() {
    let store = KkdbLogStore::default(); // in-memory
                                         // Should succeed silently
    let eliminated = store.compact().unwrap();
    assert_eq!(eliminated, 0);
}
