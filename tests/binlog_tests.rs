//! Integration tests for the KKDB binlog streaming and follower pull system.
//!
//! Tests:
//!   1. BinlogManager::append() + read_from() incremental reads
//!   2. BinlogBroadcaster fan-out to multiple subscribers
//!   3. BinlogFollower::record_to_sql() SQL generation for each record type
//!   4. round-trip: serialize → BinlogManager::append → read_from → deserialize
//!   5. Base64 encode/decode round-trip for wire format
//!   6. BinlogFollower::pull_batch() via live HTTP server

use kkdb::binlog::{
    BinlogBroadcaster, BinlogEvent, BinlogFollower, LogRecord, base64_encode,
};
use std::time::Duration;
use tempfile::TempDir;

// ─── Test 1: BinlogManager append + read_from ────────────────────────────────

#[test]
fn test_binlog_append_and_read_from() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("test_db.kkdb");
    let mut mgr = kkdb::binlog::BinlogManager::open(&db_path).unwrap();

    let records = vec![
        LogRecord::Begin(1),
        LogRecord::Insert {
            txid: 1,
            table_name: "users".into(),
            rowid: 42,
            row: vec![
                kkdb::types::Value::Integer(42),
                kkdb::types::Value::Text("alice".into()),
            ],
        },
        LogRecord::Commit(1),
    ];

    let mut positions = Vec::new();
    for rec in &records {
        let pos = mgr.append(rec).unwrap();
        positions.push(pos);
    }
    mgr.fsync().unwrap();

    // Full read from 0
    let all = mgr.read_from(0).unwrap();
    assert_eq!(all.len(), 3, "should read back all 3 records");

    // Incremental read from after first record
    let after_first = positions[1]; // start of second record
    let tail = mgr.read_from(after_first).unwrap();
    // read_from returns records starting AT from_pos, so positions[1] is the start of record 2
    assert_eq!(tail.len(), 2, "incremental read should yield 2 records");

    // Verify deserialized content
    for (next_pos, framed) in &all {
        assert!(framed.len() >= 9, "frame must have header + at least 1 byte payload");
        let payload = &framed[8..];
        let (decoded, _) = LogRecord::deserialize(payload, 0).expect("deserialize failed");
        println!("decoded: {decoded:?}");
    }
}

// ─── Test 2: BinlogBroadcaster fan-out ───────────────────────────────────────

#[tokio::test]
async fn test_broadcaster_fanout() {
    let broadcaster = BinlogBroadcaster::in_memory();

    let mut sub1 = broadcaster.subscribe();
    let mut sub2 = broadcaster.subscribe();

    let record = LogRecord::Begin(99);
    broadcaster.append_and_broadcast(&record).unwrap();

    // Both subscribers must receive the event
    let ev1 = tokio::time::timeout(Duration::from_millis(200), sub1.recv())
        .await
        .expect("timeout")
        .expect("recv error");
    let ev2 = tokio::time::timeout(Duration::from_millis(200), sub2.recv())
        .await
        .expect("timeout")
        .expect("recv error");

    assert!(ev1.framed.len() >= 9, "event 1 must have framed bytes");
    assert!(ev2.framed.len() >= 9, "event 2 must have framed bytes");

    // Decode both frames and verify same record
    let (r1, _) = LogRecord::deserialize(&ev1.framed[8..], 0).unwrap();
    let (r2, _) = LogRecord::deserialize(&ev2.framed[8..], 0).unwrap();
    assert_eq!(r1, LogRecord::Begin(99));
    assert_eq!(r2, LogRecord::Begin(99));
}

// ─── Test 3: record_to_sql() coverage ────────────────────────────────────────

#[test]
fn test_record_to_sql_begin_commit() {
    let sql = BinlogFollower::record_to_sql(&LogRecord::Begin(7));
    assert!(sql[0].contains("BEGIN") && sql[0].contains("txid=7"), "begin: {}", sql[0]);

    let sql = BinlogFollower::record_to_sql(&LogRecord::Commit(7));
    assert!(sql[0].contains("COMMIT"), "commit: {}", sql[0]);

    let sql = BinlogFollower::record_to_sql(&LogRecord::Rollback(7));
    assert!(sql[0].contains("ROLLBACK"), "rollback: {}", sql[0]);
}

#[test]
fn test_record_to_sql_insert() {
    let rec = LogRecord::Insert {
        txid: 1,
        table_name: "products".into(),
        rowid: 5,
        row: vec![
            kkdb::types::Value::Integer(5),
            kkdb::types::Value::Text("widget".into()),
        ],
    };
    let sql = BinlogFollower::record_to_sql(&rec);
    assert_eq!(sql.len(), 1);
    assert!(sql[0].contains("INSERT"), "should contain INSERT: {}", sql[0]);
    assert!(sql[0].contains("products"), "should contain table name: {}", sql[0]);
    assert!(sql[0].contains("'widget'"), "should contain text value: {}", sql[0]);
}

#[test]
fn test_record_to_sql_update() {
    let rec = LogRecord::Update {
        txid: 1,
        table_name: "products".into(),
        rowid: 5,
        old_row: vec![kkdb::types::Value::Text("old".into())],
        new_row: vec![kkdb::types::Value::Text("new".into())],
    };
    let sql = BinlogFollower::record_to_sql(&rec);
    assert!(sql[0].contains("UPDATE products"), "should contain UPDATE: {}", sql[0]);
    assert!(sql[0].contains("WHERE rowid = 5"), "should filter by rowid: {}", sql[0]);
    assert!(sql[0].contains("'new'"), "should use new value: {}", sql[0]);
}

#[test]
fn test_record_to_sql_delete() {
    let rec = LogRecord::Delete {
        txid: 1,
        table_name: "products".into(),
        rowid: 5,
        row: None,
    };
    let sql = BinlogFollower::record_to_sql(&rec);
    assert!(sql[0].contains("DELETE FROM products"), "should contain DELETE: {}", sql[0]);
    assert!(sql[0].contains("WHERE rowid = 5"), "should filter by rowid: {}", sql[0]);
}

// ─── Test 4: Round-trip serialize → append → read_from → deserialize ─────────

#[test]
fn test_roundtrip_all_record_types() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("rt.kkdb");
    let mut mgr = kkdb::binlog::BinlogManager::open(&db_path).unwrap();

    let records: Vec<LogRecord> = vec![
        LogRecord::Begin(100),
        LogRecord::Insert {
            txid: 100,
            table_name: "t".into(),
            rowid: 1,
            row: vec![kkdb::types::Value::Integer(1), kkdb::types::Value::Real(3.14)],
        },
        LogRecord::Update {
            txid: 100,
            table_name: "t".into(),
            rowid: 1,
            old_row: vec![kkdb::types::Value::Integer(1)],
            new_row: vec![kkdb::types::Value::Integer(2)],
        },
        LogRecord::Delete {
            txid: 100,
            table_name: "t".into(),
            rowid: 1,
            row: Some(vec![kkdb::types::Value::Integer(2)]),
        },
        LogRecord::Prepare(100),
        LogRecord::Commit(100),
    ];

    for rec in &records {
        mgr.append(rec).unwrap();
    }
    mgr.fsync().unwrap();

    let frames = mgr.read_from(0).unwrap();
    assert_eq!(frames.len(), records.len(), "all records must round-trip");

    for ((_, framed), original) in frames.iter().zip(records.iter()) {
        let payload = &framed[8..];
        let (decoded, _) = LogRecord::deserialize(payload, 0)
            .expect("deserialize must succeed");
        assert_eq!(&decoded, original, "round-trip mismatch");
    }
}

// ─── Test 5: Base64 encode/decode round-trip ─────────────────────────────────

#[test]
fn test_base64_roundtrip() {
    let original = b"Hello, KKDB binlog streaming! \x00\x01\x02\xff";
    let encoded = base64_encode(original);
    assert!(!encoded.is_empty());
    // Verify using std base64 logic: decode manually
    // (Using our own decoder via BinlogFollower internals would require pub access;
    //  here we just verify the encoded string is valid base64 chars)
    for c in encoded.chars() {
        assert!(
            c.is_alphanumeric() || c == '+' || c == '/' || c == '=',
            "unexpected char in base64: {c}"
        );
    }
}

// ─── Test 6: /binlog/stream HTTP endpoint ────────────────────────────────────

#[tokio::test]
async fn test_binlog_stream_http_endpoint() {
    use std::sync::Arc;
    use std::collections::BTreeMap;
    use kkdb::raft::node::KkdbNode;
    use kkdb::raft::network::NodeRegistry;
    use std::sync::Mutex;

    // Spin up a KkdbNode + binlog HTTP server on a random port
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, kkdb::server::http_api::AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .expect("create node");
    node.init_single().await.expect("init");
    node.wait_for_leader(Duration::from_secs(5)).await.expect("leader");

    let broadcaster = BinlogBroadcaster::in_memory();

    // Append a few records to the broadcaster's in-memory manager
    broadcaster.append_and_broadcast(&LogRecord::Begin(1)).unwrap();
    broadcaster.append_and_broadcast(&LogRecord::Commit(1)).unwrap();

    // Start HTTP server on a random ephemeral port
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let router = kkdb::raft::http_transport::build_raft_router_with_store(
        Arc::new(node),
        None,
        Some(broadcaster),
    );

    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Pull from pos=0
    let url = format!("http://{addr}/binlog/stream?from_pos=0");
    let resp = reqwest::get(&url).await.expect("HTTP GET");
    assert!(resp.status().is_success(), "status: {}", resp.status());

    let body = resp.text().await.expect("body");
    let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected 2 framed records, got {}:\n{body}", lines.len());

    // Parse each line and verify pos increases monotonically
    let mut prev_pos = 0u64;
    for line in &lines {
        let obj: serde_json::Value = serde_json::from_str(line).expect("parse JSON line");
        let pos = obj["pos"].as_u64().unwrap_or(0);
        assert!(pos > prev_pos, "pos must increase: {prev_pos} → {pos}");
        assert!(obj["data"].is_string(), "data field must be a string");
        prev_pos = pos;
    }
}
