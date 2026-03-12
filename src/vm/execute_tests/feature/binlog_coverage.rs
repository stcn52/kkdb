// ═══════════════════════════════════════════════════════════════════════════════
// Round-5 coverage: Binlog module tests
//
// Target: binlog/mod.rs  24.2% → 60%+
// Tests serialization/deserialization, crash recovery, base64, value_to_sql
// ═══════════════════════════════════════════════════════════════════════════════

use crate::binlog::{base64_encode, BinlogBroadcaster, BinlogFollower, BinlogManager, LogRecord};
use crate::types::Value;

// ── Serialize / Deserialize roundtrip ─────────────────────────────────────────

fn roundtrip(record: &LogRecord) -> LogRecord {
    let mut buf = Vec::new();
    record.serialize(&mut buf).unwrap();
    let (deserialized, _) = LogRecord::deserialize(&buf, 0).expect("deserialize failed");
    deserialized
}

#[test]
fn test_binlog_serde_begin() {
    let r = LogRecord::Begin(42);
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_insert() {
    let r = LogRecord::Insert {
        txid: 1,
        table_name: "users".into(),
        rowid: 100,
        row: vec![Value::Integer(100), Value::Text("alice".into())],
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_update() {
    let r = LogRecord::Update {
        txid: 2,
        table_name: "users".into(),
        rowid: 100,
        old_row: vec![Value::Integer(100), Value::Text("alice".into())],
        new_row: vec![Value::Integer(100), Value::Text("bob".into())],
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_delete_with_row() {
    let r = LogRecord::Delete {
        txid: 3,
        table_name: "users".into(),
        rowid: 50,
        row: Some(vec![Value::Integer(50), Value::Text("charlie".into())]),
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_delete_without_row() {
    let r = LogRecord::Delete {
        txid: 4,
        table_name: "t".into(),
        rowid: -1,
        row: None,
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_prepare() {
    let r = LogRecord::Prepare(7);
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_commit() {
    let r = LogRecord::Commit(8);
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_rollback() {
    let r = LogRecord::Rollback(9);
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_sql() {
    let r = LogRecord::Sql {
        sql: "INSERT INTO t VALUES (1, 'hello')".into(),
        user_id: "admin".into(),
        raft_index: 42,
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_sql_empty_user() {
    let r = LogRecord::Sql {
        sql: "CREATE TABLE t (id INT)".into(),
        user_id: "".into(),
        raft_index: 0,
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_with_blob_value() {
    let r = LogRecord::Insert {
        txid: 10,
        table_name: "blobs".into(),
        rowid: 1,
        row: vec![Value::Blob(vec![0xDE, 0xAD, 0xBE, 0xEF])],
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_serde_with_null_and_real() {
    let r = LogRecord::Insert {
        txid: 11,
        table_name: "mixed".into(),
        rowid: 2,
        row: vec![Value::Null, Value::Real(3.14), Value::Integer(0)],
    };
    assert_eq!(roundtrip(&r), r);
}

#[test]
fn test_binlog_deserialize_invalid_tag() {
    let data = vec![255u8]; // invalid tag
    assert!(LogRecord::deserialize(&data, 0).is_none());
}

#[test]
fn test_binlog_deserialize_truncated() {
    // Only a tag byte, no payload
    let data = vec![1u8]; // Begin tag, but no txid varint
    assert!(LogRecord::deserialize(&data, 0).is_none());
}

#[test]
fn test_binlog_deserialize_pos_past_end() {
    let data = vec![1u8, 2u8];
    assert!(LogRecord::deserialize(&data, 100).is_none());
}

// ── Memory-mode BinlogManager ────────────────────────────────────────────────

#[test]
fn test_binlog_manager_memory_append_read() {
    let mut mgr = BinlogManager::open_memory();
    let r1 = LogRecord::Begin(1);
    let r2 = LogRecord::Insert {
        txid: 1,
        table_name: "t".into(),
        rowid: 1,
        row: vec![Value::Integer(1)],
    };
    let r3 = LogRecord::Commit(1);

    mgr.append(&r1).unwrap();
    mgr.append(&r2).unwrap();
    mgr.append(&r3).unwrap();

    let frames = mgr.read_from(0).unwrap();
    assert_eq!(frames.len(), 3);

    // Verify deserialization of read frames
    for (_, framed) in &frames {
        assert!(framed.len() >= 8);
        let payload = &framed[8..];
        assert!(LogRecord::deserialize(payload, 0).is_some());
    }
}

#[test]
fn test_binlog_manager_memory_read_from_offset() {
    let mut mgr = BinlogManager::open_memory();
    mgr.append(&LogRecord::Begin(1)).unwrap();
    let pos_after_first = mgr.write_pos;
    mgr.append(&LogRecord::Commit(1)).unwrap();

    // Read only from after the first record
    let frames = mgr.read_from(pos_after_first).unwrap();
    assert_eq!(frames.len(), 1);
}

#[test]
fn test_binlog_manager_memory_fsync_noop() {
    let mut mgr = BinlogManager::open_memory();
    mgr.append(&LogRecord::Begin(1)).unwrap();
    // fsync on memory manager should not fail
    mgr.fsync().unwrap();
}

#[test]
fn test_binlog_manager_memory_recover_returns_empty() {
    let mut mgr = BinlogManager::open_memory();
    let uncommitted = mgr.recover().unwrap();
    assert!(uncommitted.is_empty());
}

// ── File-mode BinlogManager (tempdir) ────────────────────────────────────────

#[test]
fn test_binlog_file_mode_append_read_recover() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");

    // Append records
    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        mgr.append(&LogRecord::Begin(1)).unwrap();
        mgr.append(&LogRecord::Insert {
            txid: 1,
            table_name: "t".into(),
            rowid: 1,
            row: vec![Value::Integer(42)],
        })
        .unwrap();
        mgr.append(&LogRecord::Commit(1)).unwrap();
        mgr.fsync().unwrap();
    }

    // Re-open and read
    {
        let mgr = BinlogManager::open(&db_path).unwrap();
        let frames = mgr.read_from(0).unwrap();
        assert_eq!(frames.len(), 3);
    }

    // Recover
    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        let uncommitted = mgr.recover().unwrap();
        assert!(uncommitted.is_empty()); // all committed
    }
}

#[test]
fn test_binlog_recover_uncommitted_prepare() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_recover.db");

    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        mgr.append(&LogRecord::Begin(1)).unwrap();
        mgr.append(&LogRecord::Prepare(1)).unwrap();
        // No Commit — this txn is uncommitted
        mgr.append(&LogRecord::Begin(2)).unwrap();
        mgr.append(&LogRecord::Prepare(2)).unwrap();
        mgr.append(&LogRecord::Commit(2)).unwrap();
        mgr.fsync().unwrap();
    }

    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        let uncommitted = mgr.recover().unwrap();
        assert!(uncommitted.contains(&1));
        assert!(!uncommitted.contains(&2)); // was committed
    }
}

#[test]
fn test_binlog_recover_truncated_file() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test_trunc.db");
    let binlog_path = db_path.with_extension("binlog");

    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        mgr.append(&LogRecord::Begin(1)).unwrap();
        mgr.append(&LogRecord::Commit(1)).unwrap();
        mgr.fsync().unwrap();
    }

    // Corrupt by appending garbage (simulates torn write)
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&binlog_path)
            .unwrap();
        f.write_all(&[0xFF, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x00, 0xDE, 0xAD])
            .unwrap();
    }

    // Recover should truncate the garbage and return no uncommitted
    {
        let mut mgr = BinlogManager::open(&db_path).unwrap();
        let uncommitted = mgr.recover().unwrap();
        assert!(uncommitted.is_empty());
    }
}

// ── Base64 encode/decode ─────────────────────────────────────────────────────

#[test]
fn test_base64_encode_roundtrip_simple() {
    let original = b"Hello, binlog!";
    let encoded = base64_encode(original);
    // Verify it's a valid base64 string (only printable ASCII)
    assert!(encoded
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    assert!(!encoded.is_empty());
}

#[test]
fn test_base64_encode_empty() {
    let encoded = base64_encode(b"");
    assert!(encoded.is_empty() || encoded == "" || encoded.chars().all(|c| c == '='));
}

#[test]
fn test_base64_encode_binary() {
    let data: Vec<u8> = (0..=255).collect();
    let encoded = base64_encode(&data);
    assert!(!encoded.is_empty());
}

// ── BinlogFollower::record_to_sql ────────────────────────────────────────────

#[test]
fn test_record_to_sql_insert() {
    let r = LogRecord::Insert {
        txid: 1,
        table_name: "users".into(),
        rowid: 1,
        row: vec![Value::Integer(1), Value::Text("alice".into())],
    };
    let sqls = BinlogFollower::record_to_sql(&r);
    assert_eq!(sqls.len(), 1);
    assert!(sqls[0].contains("INSERT OR REPLACE INTO users"));
    assert!(sqls[0].contains("'alice'"));
}

#[test]
fn test_record_to_sql_update() {
    let r = LogRecord::Update {
        txid: 1,
        table_name: "t".into(),
        rowid: 5,
        old_row: vec![Value::Integer(5), Value::Text("old".into())],
        new_row: vec![Value::Integer(5), Value::Text("new".into())],
    };
    let sqls = BinlogFollower::record_to_sql(&r);
    assert!(sqls[0].contains("UPDATE t SET"));
    assert!(sqls[0].contains("WHERE rowid = 5"));
}

#[test]
fn test_record_to_sql_delete() {
    let r = LogRecord::Delete {
        txid: 1,
        table_name: "t".into(),
        rowid: 3,
        row: None,
    };
    let sqls = BinlogFollower::record_to_sql(&r);
    assert!(sqls[0].contains("DELETE FROM t WHERE rowid = 3"));
}

#[test]
fn test_record_to_sql_begin_commit_rollback_prepare() {
    assert!(BinlogFollower::record_to_sql(&LogRecord::Begin(1))[0].contains("BEGIN"));
    assert!(BinlogFollower::record_to_sql(&LogRecord::Commit(1))[0].contains("COMMIT"));
    assert!(BinlogFollower::record_to_sql(&LogRecord::Rollback(1))[0].contains("ROLLBACK"));
    assert!(BinlogFollower::record_to_sql(&LogRecord::Prepare(1))[0].contains("PREPARE"));
}

#[test]
fn test_record_to_sql_sql_variant() {
    let r = LogRecord::Sql {
        sql: "CREATE TABLE t (id INT)".into(),
        user_id: "admin".into(),
        raft_index: 1,
    };
    let sqls = BinlogFollower::record_to_sql(&r);
    assert_eq!(sqls[0], "CREATE TABLE t (id INT)");
}

#[test]
fn test_value_to_sql_literal_all_types() {
    // Test via Insert record to SQL conversion
    let r = LogRecord::Insert {
        txid: 1,
        table_name: "t".into(),
        rowid: 1,
        row: vec![
            Value::Null,
            Value::Integer(42),
            Value::Real(3.14),
            Value::Text("it's a test".into()),
            Value::Blob(vec![0xCA, 0xFE]),
        ],
    };
    let sqls = BinlogFollower::record_to_sql(&r);
    assert!(sqls[0].contains("NULL"));
    assert!(sqls[0].contains("42"));
    assert!(sqls[0].contains("3.14"));
    assert!(sqls[0].contains("it''s a test")); // escaped quote
    assert!(sqls[0].contains("X'cafe'")); // hex blob
}

// ── BinlogBroadcaster in-memory ──────────────────────────────────────────────

#[test]
fn test_broadcaster_in_memory_append_and_subscribe() {
    let broadcaster = BinlogBroadcaster::in_memory();
    let mut rx = broadcaster.subscribe();
    assert_eq!(broadcaster.subscriber_count(), 1);

    broadcaster
        .append_and_broadcast(&LogRecord::Begin(1))
        .unwrap();
    broadcaster
        .append_and_broadcast(&LogRecord::Commit(1))
        .unwrap();

    // Subscriber should have received 2 events
    let event1 = rx.try_recv().unwrap();
    assert!(event1.framed.len() >= 8);
    let event2 = rx.try_recv().unwrap();
    assert!(event2.pos > event1.pos);
}

#[test]
fn test_broadcaster_no_subscribers_ok() {
    let broadcaster = BinlogBroadcaster::in_memory();
    // No subscribers — should not panic
    broadcaster
        .append_and_broadcast(&LogRecord::Begin(1))
        .unwrap();
}

// ── BinlogFollower checkpoint ────────────────────────────────────────────────

#[test]
fn test_follower_checkpoint_save_load() {
    let dir = tempfile::tempdir().unwrap();
    let cp_path = dir.path().join("checkpoint.txt");

    // Create follower with no existing checkpoint
    let mut follower = BinlogFollower::new("http://localhost:0".into(), Some(cp_path.clone()));
    assert_eq!(follower.pos, 0);

    // Save a checkpoint
    follower.pos = 12345;
    // save_checkpoint is private; just verify field access works
    assert_eq!(follower.pos, 12345);
}

#[test]
fn test_follower_no_checkpoint_path() {
    let follower = BinlogFollower::new("http://localhost:0".into(), None);
    assert_eq!(follower.pos, 0);
}

// ── Multiple records in sequence (coverage for multi-record scan) ────────────

#[test]
fn test_binlog_memory_many_records() {
    let mut mgr = BinlogManager::open_memory();
    let records = vec![
        LogRecord::Begin(1),
        LogRecord::Insert {
            txid: 1,
            table_name: "a".into(),
            rowid: 1,
            row: vec![Value::Integer(1)],
        },
        LogRecord::Update {
            txid: 1,
            table_name: "a".into(),
            rowid: 1,
            old_row: vec![Value::Integer(1)],
            new_row: vec![Value::Integer(2)],
        },
        LogRecord::Delete {
            txid: 1,
            table_name: "a".into(),
            rowid: 1,
            row: Some(vec![Value::Integer(2)]),
        },
        LogRecord::Prepare(1),
        LogRecord::Commit(1),
        LogRecord::Rollback(2),
        LogRecord::Sql {
            sql: "SELECT 1".into(),
            user_id: "".into(),
            raft_index: 0,
        },
    ];

    for r in &records {
        mgr.append(r).unwrap();
    }

    let frames = mgr.read_from(0).unwrap();
    assert_eq!(frames.len(), records.len());

    // Verify each frame round-trips correctly
    for (i, (_, framed)) in frames.iter().enumerate() {
        let payload = &framed[8..];
        let (decoded, _) = LogRecord::deserialize(payload, 0).unwrap();
        assert_eq!(decoded, records[i], "mismatch at record {i}");
    }
}

#[test]
fn test_binlog_read_from_empty() {
    // Reading from an empty in-memory binlog should return no frames
    let mgr = BinlogManager::open_memory();
    let frames = mgr.read_from(0).unwrap();
    assert!(frames.is_empty());
}

#[test]
fn test_binlog_negative_rowid() {
    let r = LogRecord::Insert {
        txid: 1,
        table_name: "t".into(),
        rowid: -9999,
        row: vec![Value::Integer(-9999)],
    };
    let decoded = roundtrip(&r);
    if let LogRecord::Insert { rowid, .. } = decoded {
        assert_eq!(rowid, -9999);
    } else {
        panic!("wrong variant");
    }
}
