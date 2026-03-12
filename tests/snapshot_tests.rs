//! Snapshot persistence tests for KkdbStateMachine.
//!
//! Verifies: build_snapshot writes to disk, get_current_snapshot reads it back,
//! install_snapshot persists and replays, state survives restart.

use kkdb::raft::state_machine::{KkdbSnapshotData, KkdbStateMachine};
use kkdb::raft::types::{KkdbNodeId, KkdbRequest};
use kkdb::server::http_api::AppState;
use openraft::{
    storage::RaftStateMachine, LogId, RaftSnapshotBuilder, SnapshotMeta, StoredMembership,
};
use std::io::Cursor;

// ─── helpers ─────────────────────────────────────────────────────────────────

fn in_memory_sm(dir: &std::path::Path) -> KkdbStateMachine {
    KkdbStateMachine::open(AppState::in_memory(), dir).expect("open SM")
}

fn dummy_log_id(term: u64, index: u64) -> LogId<KkdbNodeId> {
    LogId {
        leader_id: openraft::LeaderId::new(term, 1),
        index,
    }
}

// ─── Test 1: build_snapshot writes a JSON file to disk ───────────────────────

#[tokio::test]
async fn test_snapshot_written_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let mut sm = in_memory_sm(dir.path());
    sm.last_applied_log = Some(dummy_log_id(1, 5));

    sm.build_snapshot().await.unwrap();

    let snap_path = dir.path().join("raft").join("snapshot.json");
    assert!(
        snap_path.exists(),
        "snapshot.json must exist after build_snapshot"
    );
}

// ─── Test 2: get_current_snapshot returns what was built ─────────────────────

#[tokio::test]
async fn test_get_current_snapshot_returns_built() {
    let dir = tempfile::tempdir().unwrap();
    let mut sm = in_memory_sm(dir.path());
    sm.last_applied_log = Some(dummy_log_id(1, 3));

    sm.build_snapshot().await.unwrap();

    let snap = sm.get_current_snapshot().await.unwrap();
    assert!(
        snap.is_some(),
        "get_current_snapshot must return Some after build"
    );
    let s = snap.unwrap();
    assert_eq!(s.meta.last_log_id.map(|l| l.index), Some(3));
}

// ─── Test 3: snapshot survives reopen (get_current_snapshot reads from disk) ──

#[tokio::test]
async fn test_snapshot_survives_reopen() {
    let dir = tempfile::tempdir().unwrap();

    // Build snapshot in first SM instance
    {
        let mut sm = in_memory_sm(dir.path());
        sm.last_applied_log = Some(dummy_log_id(2, 10));
        sm.build_snapshot().await.unwrap();
    }

    // New SM instance — in-memory cache is empty, must load from disk
    {
        let mut sm = in_memory_sm(dir.path());
        // current_snapshot should be populated by open() reading the file
        assert!(
            sm.current_snapshot.is_some(),
            "snapshot loaded from disk on open()"
        );
        let snap = sm.get_current_snapshot().await.unwrap();
        assert!(snap.is_some(), "get_current_snapshot reads from disk");
        let s = snap.unwrap();
        assert_eq!(
            s.meta.last_log_id.map(|l| l.index),
            Some(10),
            "correct log index recovered"
        );
    }
}

// ─── Test 4: install_snapshot persists to disk and replays SQL ───────────────

#[tokio::test]
async fn test_install_snapshot_persists() {
    let dir = tempfile::tempdir().unwrap();

    let log_id = dummy_log_id(1, 7);
    let snap_data = KkdbSnapshotData {
        entries: vec![KkdbRequest {
            sql: "CREATE TABLE t (id INT)".into(),
            user_id: String::new(),
        }],
        last_applied: Some(log_id),
        last_membership: StoredMembership::default(),
    };

    let payload = serde_json::to_vec(&snap_data).unwrap();
    let meta = SnapshotMeta::<KkdbNodeId, openraft::BasicNode> {
        last_log_id: Some(log_id),
        last_membership: StoredMembership::default(),
        snapshot_id: "test-snap-1".to_string(),
    };

    {
        let mut sm = in_memory_sm(dir.path());
        sm.install_snapshot(&meta, Box::new(Cursor::new(payload)))
            .await
            .unwrap();

        // Snapshot file should now exist
        let snap_path = dir.path().join("raft").join("snapshot.json");
        assert!(snap_path.exists(), "install_snapshot must write to disk");
    }

    // Re-open: snapshot must be loaded and applied_entries restored
    {
        let sm = KkdbStateMachine::open(AppState::in_memory(), dir.path()).unwrap();
        assert_eq!(
            sm.applied_entries.len(),
            1,
            "applied_entries restored from snapshot"
        );
        assert_eq!(
            sm.last_applied_log.map(|l| l.index),
            Some(7),
            "last_applied_log restored"
        );
    }
}

// ─── Test 5: snapshot overwrite (newer snapshot replaces older) ───────────────

#[tokio::test]
async fn test_snapshot_overwrite() {
    let dir = tempfile::tempdir().unwrap();

    // Build snapshot at index 5
    {
        let mut sm = in_memory_sm(dir.path());
        sm.last_applied_log = Some(dummy_log_id(1, 5));
        sm.build_snapshot().await.unwrap();
    }

    // Build snapshot at index 9
    {
        let mut sm = in_memory_sm(dir.path());
        sm.last_applied_log = Some(dummy_log_id(1, 9));
        sm.build_snapshot().await.unwrap();
        // Get current should return index 9
        let snap = sm.get_current_snapshot().await.unwrap().unwrap();
        assert_eq!(snap.meta.last_log_id.map(|l| l.index), Some(9));
    }

    // Re-open: should get the latest (index 9)
    {
        let mut sm = in_memory_sm(dir.path());
        let snap = sm.get_current_snapshot().await.unwrap().unwrap();
        assert_eq!(
            snap.meta.last_log_id.map(|l| l.index),
            Some(9),
            "snapshot was overwritten by newer one"
        );
    }
}
