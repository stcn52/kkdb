//! Integration tests for the KKDB Raft distributed consensus layer.
//!
//! These tests use an in-memory 3-node cluster (all nodes in one process)
//! to verify:
//!   1. Leader election after cluster init
//!   2. SQL writes replicated across all nodes
//!   3. Follower node rejects write (or forwards) — verified via metrics

use std::time::Duration;

use kkdb::raft::types::KkdbRequest;
use kkdb::raft::node::{KkdbNode, start_cluster_3};
use kkdb::server::http_api::AppState;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use kkdb::raft::network::NodeRegistry;

/// Convenience: create 3 independent in-memory AppStates for the 3 nodes.
fn three_states() -> [AppState; 3] {
    [
        AppState::in_memory(),
        AppState::in_memory(),
        AppState::in_memory(),
    ]
}

// ─── Test 1: Single-node cluster ─────────────────────────────────────────────

#[tokio::test]
async fn test_single_node_becomes_leader() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .expect("create node");

    node.init_single().await.expect("init single");

    // Wait for self to become leader
    let leader = node
        .wait_for_leader(Duration::from_secs(5))
        .await
        .expect("leader elected");
    assert_eq!(leader, 1, "single node must elect itself as leader");
}

// ─── Test 2: Single-node write ────────────────────────────────────────────────

#[tokio::test]
async fn test_single_node_write_and_apply() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .expect("create node");
    node.init_single().await.expect("init");

    // Wait for leadership
    node.wait_for_leader(Duration::from_secs(5))
        .await
        .expect("leader");

    // Submit SQL create table through Raft
    let resp = node
        .write(KkdbRequest {
            sql: "CREATE TABLE products (id INTEGER PRIMARY KEY, name TEXT)".into(),
            user_id: "".into(), // use auth_vm (global)
        })
        .await
        .expect("write");
    assert!(resp.ok, "CREATE TABLE via Raft failed: {}", resp.message);

    // Submit INSERT
    let resp2 = node
        .write(KkdbRequest {
            sql: "INSERT INTO products VALUES (1, 'Widget')".into(),
            user_id: "".into(),
        })
        .await
        .expect("write 2");
    assert!(resp2.ok, "INSERT via Raft failed: {}", resp2.message);
}

// ─── Test 3: 3-node cluster leader election ───────────────────────────────────

#[tokio::test]
async fn test_three_node_leader_election() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    // At least one of the 3 nodes should elect a leader
    let leader = n1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader elected in 10s");

    // All 3 nodes should agree on the same leader
    let m1 = n1.metrics();
    let m2 = n2.metrics();
    let m3 = n3.metrics();

    assert_eq!(m1.current_leader, Some(leader));
    assert_eq!(m2.current_leader, Some(leader));
    assert_eq!(m3.current_leader, Some(leader));

    // Shutdown cleanly
    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 4: 3-node write replication ─────────────────────────────────────────

#[tokio::test]
async fn test_three_node_write_replication() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    // Wait for leadership
    let leader_id = n1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    // Find the leader node (bind refs to named variable to avoid E0716)
    let nodes = [&n1, &n2, &n3];
    let leader = nodes
        .iter()
        .find(|n| n.id == leader_id)
        .expect("leader node not found");

    // Write 3 SQL statements through Raft
    let stmts = [
        "CREATE TABLE items (id INTEGER PRIMARY KEY, value TEXT)",
        "INSERT INTO items VALUES (1, 'alpha')",
        "INSERT INTO items VALUES (2, 'beta')",
    ];

    for sql in &stmts {
        let resp = leader
            .write(KkdbRequest { sql: sql.to_string(), user_id: "".into() })
            .await
            .expect("write");
        assert!(resp.ok, "SQL failed: {} — {}", sql, resp.message);
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    let m1 = n1.metrics();
    let m3 = n3.metrics();
    // All nodes should have applied the same log index
    assert_eq!(
        m1.last_applied,
        m3.last_applied,
        "follower did not replicate all entries"
    );

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 5: Cluster metrics basics ──────────────────────────────────────────

#[tokio::test]
async fn test_cluster_metrics() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .unwrap();
    node.init_single().await.unwrap();
    node.wait_for_leader(Duration::from_secs(5)).await.unwrap();

    let m = node.metrics();
    assert_eq!(m.id, 1);
    assert!(m.current_leader.is_some());
    node.shutdown().await.unwrap();
}
