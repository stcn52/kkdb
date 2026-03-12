// ═══════════════════════════════════════════════════════════════════════════════
// Round-5 coverage: Raft log_store.rs
//
// Target: raft/log_store.rs coverage via synchronous test helpers.
// Tests: open, append_direct, truncate_direct, purge_direct, compact,
//        compaction_stats, WAL recovery, vote persistence, purge persistence.
// ═══════════════════════════════════════════════════════════════════════════════

use crate::raft::log_store::KkdbLogStore;
use crate::raft::types::{KkdbRequest, KkdbTypeConfig};
use openraft::{Entry, EntryPayload, LogId};

/// Build a dummy log entry at the given index.
fn make_entry(index: u64) -> Entry<KkdbTypeConfig> {
    Entry {
        log_id: LogId::new(openraft::CommittedLeaderId::new(1, 0), index),
        payload: EntryPayload::Normal(KkdbRequest {
            sql: format!("INSERT INTO t VALUES ({})", index),
            user_id: "test".into(),
        }),
    }
}

// ── Open / fresh ──────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_open_fresh() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();
    assert_eq!(store.entry_count(), 0);
    assert!(store.last_index().is_none());
    assert!(store.last_purged().is_none());
    assert!(store.persisted_vote().is_none());
}

// ── Append / read ─────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_append_and_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    let entries = vec![make_entry(1), make_entry(2), make_entry(3)];
    store.append_direct(entries).unwrap();

    assert_eq!(store.entry_count(), 3);
    assert_eq!(store.last_index(), Some(3));

    let all = store.all_entries();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].log_id.index, 1);
    assert_eq!(all[2].log_id.index, 3);
}

// ── Truncate ──────────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_truncate() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    store
        .append_direct(vec![make_entry(1), make_entry(2), make_entry(3)])
        .unwrap();
    assert_eq!(store.entry_count(), 3);

    // Truncate from index 2 → keeps only index 1
    store.truncate_direct(2).unwrap();
    assert_eq!(store.entry_count(), 1);
    assert_eq!(store.last_index(), Some(1));
}

// ── Purge ─────────────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_purge() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    store
        .append_direct(vec![make_entry(1), make_entry(2), make_entry(3)])
        .unwrap();

    let purge_id = LogId::new(openraft::CommittedLeaderId::new(1, 0), 2);
    store.purge_direct(purge_id).unwrap();

    assert_eq!(store.entry_count(), 1); // only index 3 remains
    assert_eq!(store.last_index(), Some(3));
    assert_eq!(store.last_purged().unwrap().index, 2);
}

// ── Compaction ────────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_compact() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    // Append 5 entries, truncate last 2 → creates dead records
    store
        .append_direct(vec![
            make_entry(1),
            make_entry(2),
            make_entry(3),
            make_entry(4),
            make_entry(5),
        ])
        .unwrap();
    store.truncate_direct(4).unwrap(); // drop 4,5

    let (live, total, dead) = store.compaction_stats();
    assert_eq!(live, 3);
    assert!(dead > 0 || total > live);

    let eliminated = store.compact().unwrap();
    assert!(eliminated > 0);

    // After compaction: total should equal live
    let (live2, total2, dead2) = store.compaction_stats();
    assert_eq!(live2, 3);
    assert_eq!(total2, live2);
    assert_eq!(dead2, 0);
}

// ── WAL recovery ──────────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_wal_recovery() {
    let dir = tempfile::tempdir().unwrap();

    // Phase 1: append entries
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![make_entry(1), make_entry(2), make_entry(3)])
            .unwrap();
        assert_eq!(store.entry_count(), 3);
    }

    // Phase 2: reopen → should recover all 3 entries from WAL
    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(store.entry_count(), 3);
        assert_eq!(store.last_index(), Some(3));
    }
}

#[test]
fn test_raft_log_store_wal_recovery_with_truncate() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![make_entry(1), make_entry(2), make_entry(3)])
            .unwrap();
        store.truncate_direct(3).unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(store.entry_count(), 2); // index 1, 2
        assert_eq!(store.last_index(), Some(2));
    }
}

#[test]
fn test_raft_log_store_wal_recovery_with_purge() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![make_entry(1), make_entry(2), make_entry(3)])
            .unwrap();
        let purge_id = LogId::new(openraft::CommittedLeaderId::new(1, 0), 1);
        store.purge_direct(purge_id).unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(store.entry_count(), 2); // index 2, 3
        assert_eq!(store.last_purged().unwrap().index, 1);
    }
}

// ── Compact + reopen ──────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_compact_then_reopen() {
    let dir = tempfile::tempdir().unwrap();

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        store
            .append_direct(vec![
                make_entry(1),
                make_entry(2),
                make_entry(3),
                make_entry(4),
            ])
            .unwrap();
        store.truncate_direct(3).unwrap();
        store.compact().unwrap();
    }

    {
        let store = KkdbLogStore::open(dir.path()).unwrap();
        assert_eq!(store.entry_count(), 2); // index 1, 2
        assert_eq!(store.last_index(), Some(2));
    }
}

// ── Edge: empty compact ──────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_compact_empty() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();
    // Compact on empty store should be no-op
    let eliminated = store.compact().unwrap();
    assert_eq!(eliminated, 0);
}

// ── Default (in-memory, no WAL) ──────────────────────────────────────────────

#[test]
fn test_raft_log_store_default_in_memory() {
    let store = KkdbLogStore::default();
    assert_eq!(store.entry_count(), 0);

    store.append_direct(vec![make_entry(1)]).unwrap();
    assert_eq!(store.entry_count(), 1);

    // compact on in-memory store → 0
    let eliminated = store.compact().unwrap();
    assert_eq!(eliminated, 0);
}

// ── Compaction stats ─────────────────────────────────────────────────────────

#[test]
fn test_raft_log_store_compaction_stats() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    store
        .append_direct(vec![make_entry(1), make_entry(2)])
        .unwrap();

    let (live, total, dead) = store.compaction_stats();
    assert_eq!(live, 2);
    assert_eq!(total, 2);
    assert_eq!(dead, 0);
}

// ── Multiple append batches ──────────────────────────────────────────────────

#[test]
fn test_raft_log_store_multiple_appends() {
    let dir = tempfile::tempdir().unwrap();
    let store = KkdbLogStore::open(dir.path()).unwrap();

    store
        .append_direct(vec![make_entry(1), make_entry(2)])
        .unwrap();
    store
        .append_direct(vec![make_entry(3), make_entry(4)])
        .unwrap();

    assert_eq!(store.entry_count(), 4);
    assert_eq!(store.last_index(), Some(4));

    // Verify all entries are correct
    let entries = store.all_entries();
    for (i, e) in entries.iter().enumerate() {
        assert_eq!(e.log_id.index, (i + 1) as u64);
    }
}
