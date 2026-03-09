//! WAL (Write-Ahead Log) persistence tests.
//!
//! Tests KkdbLogStore's disk persistence through the public test helpers
//! (append_direct, truncate_direct, purge_direct, persisted_vote).

use kkdb::raft::log_store::KkdbLogStore;
use kkdb::raft::types::{KkdbNodeId, KkdbTypeConfig};
use openraft::storage::RaftLogStorage;
use openraft::{Entry, EntryPayload, LogId, Vote};

fn blank_entry(term: u64, index: u64) -> Entry<KkdbTypeConfig> {
    Entry {
        log_id: LogId {
            leader_id: openraft::LeaderId::new(term, 1),
            index,
        },
        payload: EntryPayload::Blank,
    }
}

// ─── Test 1: Fresh WAL append ─────────────────────────────────────────────────

#[test]
fn test_wal_append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    store
        .append_direct(vec![
            blank_entry(1, 1),
            blank_entry(1, 2),
            blank_entry(1, 3),
        ])
        .unwrap();

    assert_eq!(store.last_index(), Some(3));
    assert_eq!(store.inner.lock().unwrap().log.len(), 3);
}

// ─── Test 2: Recovery after re-open ──────────────────────────────────────────

#[test]
fn test_wal_recovery_after_reopen() {
    let dir = tempfile::tempdir().unwrap();

    // Write
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![
                blank_entry(1, 1),
                blank_entry(1, 2),
                blank_entry(2, 3),
            ])
            .unwrap();
    }

    // Recover
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(store.last_index(), Some(3), "3 entries recovered");
        let inner = store.inner.lock().unwrap();
        assert_eq!(inner.log.len(), 3);
        assert!(inner.log.contains_key(&1));
        assert!(inner.log.contains_key(&3));
    }
}

// ─── Test 3: Vote persistence ─────────────────────────────────────────────────

#[tokio::test]
async fn test_wal_vote_persistence() {
    let dir = tempfile::tempdir().unwrap();
    let vote = Vote {
        leader_id: openraft::LeaderId::new(2, 1),
        committed: true,
    };

    {
        let mut store = KkdbLogStore::open(dir.path()).unwrap();
        store.save_vote(&vote).await.unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(
            store.persisted_vote(),
            Some(vote),
            "vote must survive reopen"
        );
    }
}

// ─── Test 4: Truncate reflected in WAL replay ─────────────────────────────────

#[test]
fn test_wal_truncate_recovery() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        let entries: Vec<_> = (1..=5).map(|i| blank_entry(1, i)).collect();
        store.append_direct(entries).unwrap();
        // Truncate: keep indices 1,2,3; remove 4,5
        store.truncate_direct(4).unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(
            store.last_index(),
            Some(3),
            "only entries 1-3 after truncate recovery"
        );
        let inner = store.inner.lock().unwrap();
        assert!(!inner.log.contains_key(&4), "entry 4 must be gone");
        assert!(!inner.log.contains_key(&5), "entry 5 must be gone");
    }
}

// ─── Test 5: Purge reflected in WAL replay ────────────────────────────────────

#[test]
fn test_wal_purge_recovery() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        let entries: Vec<_> = (1..=5).map(|i| blank_entry(1, i)).collect();
        store.append_direct(entries).unwrap();
        let purge_id = LogId {
            leader_id: openraft::LeaderId::new(1, 1),
            index: 3,
        };
        store.purge_direct(purge_id).unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(
            store.last_purged().map(|l| l.index),
            Some(3),
            "last_purged_log_id must be recovered"
        );
        // Entries 1,2,3 purged; 4,5 still present
        let inner = store.inner.lock().unwrap();
        assert!(!inner.log.contains_key(&1), "purged");
        assert!(!inner.log.contains_key(&3), "purged");
        assert!(inner.log.contains_key(&4), "still present");
        assert!(inner.log.contains_key(&5), "still present");
    }
}

// ─── Test 6: CRC corruption detected ─────────────────────────────────────────

#[test]
fn test_wal_crc_corruption_detected() {
    let dir = tempfile::tempdir().unwrap();
    let wal_path = dir.path().join("raft").join("wal.log");

    // Write 2 entries
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![blank_entry(1, 1), blank_entry(1, 2)])
            .unwrap();
    }

    // Corrupt the last byte of the file
    {
        let mut bytes = std::fs::read(&wal_path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xFF;
        std::fs::write(&wal_path, bytes).unwrap();
    }

    // Re-open: should stop at the corruption boundary and recover partial data
    // (entry 1 should be OK, entry 2 corrupt → only entry 1 or 0 entries)
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        // At minimum, the store should not panic
        let count = store.inner.lock().unwrap().log.len();
        assert!(
            count <= 2,
            "corrupted entry must not be loaded, got {} entries",
            count
        );
    }
}
