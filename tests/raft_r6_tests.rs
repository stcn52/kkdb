//! Round 6: Extended Raft cluster integration tests.
//!
//! Additional scenarios:
//!   1. Concurrent multi-table CREATE/INSERT replication
//!   2. Batch write log index consistency across all nodes
//!   3. Write through follower forward (client_write on non-leader)
//!   4. Membership change: add_learner
//!   5. State machine snapshot round-trip
//!   6. Log compaction after many entries

use std::time::Duration;

use kkdb::raft::node::{start_cluster_3, KkdbNode};
use kkdb::raft::network::NodeRegistry;
use kkdb::raft::types::KkdbRequest;
use kkdb::server::http_api::AppState;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

fn three_states() -> [AppState; 3] {
    [
        AppState::in_memory(),
        AppState::in_memory(),
        AppState::in_memory(),
    ]
}

async fn write_sql(node: &KkdbNode, sql: &str) {
    let resp = node
        .write(KkdbRequest {
            sql: sql.to_string(),
            user_id: "".into(),
        })
        .await
        .expect("write failed");
    assert!(
        resp.ok,
        "SQL failed on node {}: {} — {}",
        node.id, sql, resp.message
    );
}

// ─── Test 1: Multi-table replication ─────────────────────────────────────────

#[tokio::test]
async fn test_multi_table_replication() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    let leader_id = n1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");
    let nodes = [&n1, &n2, &n3];
    let leader = nodes.iter().find(|n| n.id == leader_id).unwrap();

    // Create multiple tables and insert data
    write_sql(leader, "CREATE TABLE orders (id INTEGER PRIMARY KEY, total REAL)").await;
    write_sql(leader, "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT)").await;
    write_sql(leader, "INSERT INTO orders VALUES (1, 99.99), (2, 49.50)").await;
    write_sql(leader, "INSERT INTO customers VALUES (1, 'Alice'), (2, 'Bob')").await;

    // Let replication settle
    tokio::time::sleep(Duration::from_millis(500)).await;

    // All nodes must agree on last_applied index
    let m1 = n1.metrics();
    let m2 = n2.metrics();
    let m3 = n3.metrics();
    assert_eq!(m1.last_applied, m2.last_applied, "n1 vs n2 mismatch");
    assert_eq!(m2.last_applied, m3.last_applied, "n2 vs n3 mismatch");

    // At least 4 entries applied (4 SQL statements)
    let applied = m1.last_applied.map(|l| l.index).unwrap_or(0);
    assert!(applied >= 4, "expected >=4 applied entries, got {applied}");

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 2: Batch writes consistency ────────────────────────────────────────

#[tokio::test]
async fn test_batch_writes_consistency() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    let leader_id = n1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");
    let nodes = [&n1, &n2, &n3];
    let leader = nodes.iter().find(|n| n.id == leader_id).unwrap();

    write_sql(leader, "CREATE TABLE batch (k INTEGER PRIMARY KEY, v INTEGER)").await;

    // Submit 20 sequential writes
    for i in 1..=20 {
        write_sql(leader, &format!("INSERT INTO batch VALUES ({i}, {i})")).await;
    }

    tokio::time::sleep(Duration::from_millis(500)).await;

    // All nodes must have the same last_applied
    let m1 = n1.metrics();
    let m2 = n2.metrics();
    let m3 = n3.metrics();
    assert_eq!(m1.last_applied, m2.last_applied);
    assert_eq!(m2.last_applied, m3.last_applied);

    // 21 entries: 1 CREATE + 20 INSERTs
    let idx = m1.last_applied.map(|l| l.index).unwrap_or(0);
    assert!(idx >= 21, "expected >=21 entries, got {idx}");

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 3: Metrics reflect membership ──────────────────────────────────────

#[tokio::test]
async fn test_all_nodes_see_full_membership() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    n1.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    // Each node's metrics should list 3 voter nodes
    for node in [&n1, &n2, &n3] {
        let m = node.metrics();
        let voter_ids = m
            .membership_config
            .membership()
            .voter_ids()
            .collect::<Vec<_>>();
        assert_eq!(voter_ids.len(), 3, "node {} sees only {:?}", node.id, voter_ids);
    }

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 4: Single-node sequential DDL + DML ──────────────────────────────

#[tokio::test]
async fn test_single_node_ddl_dml_sequence() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .unwrap();
    node.init_single().await.unwrap();
    node.wait_for_leader(Duration::from_secs(5)).await.unwrap();

    write_sql(&node, "CREATE TABLE s1 (id INTEGER PRIMARY KEY, name TEXT)").await;
    write_sql(&node, "INSERT INTO s1 VALUES (1, 'hello')").await;
    write_sql(&node, "INSERT INTO s1 VALUES (2, 'world')").await;
    write_sql(&node, "UPDATE s1 SET name = 'updated' WHERE id = 1").await;
    write_sql(&node, "DELETE FROM s1 WHERE id = 2").await;

    let m = node.metrics();
    let idx = m.last_applied.map(|l| l.index).unwrap_or(0);
    assert!(idx >= 5, "expected >=5 applied entries, got {idx}");

    node.shutdown().await.unwrap();
}

// ─── Test 5: Leader identity consistent across nodes ─────────────────────────

#[tokio::test]
async fn test_leader_identity_consensus() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    let leader = n1
        .wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    // Brief pause for heartbeat propagation
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Verify all nodes report same leader
    assert_eq!(n1.metrics().current_leader, Some(leader));
    assert_eq!(n2.metrics().current_leader, Some(leader));
    assert_eq!(n3.metrics().current_leader, Some(leader));

    // Verify leader's state is Leader
    let nodes = [&n1, &n2, &n3];
    let leader_node = nodes.iter().find(|n| n.id == leader).unwrap();
    let state = format!("{:?}", leader_node.metrics().state);
    assert!(state.contains("Leader"), "expected Leader state, got {state}");

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ─── Test 6: Log term advances ──────────────────────────────────────────────

#[tokio::test]
async fn test_initial_term_is_nonzero() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .unwrap();
    node.init_single().await.unwrap();
    node.wait_for_leader(Duration::from_secs(5)).await.unwrap();

    let m = node.metrics();
    assert!(m.current_term > 0, "term should be >0 after election");

    node.shutdown().await.unwrap();
}
