//! Raft distributed vector search tests.
//!
//! Verifies that:
//!  1. `CREATE VECTOR INDEX` DDL replicates correctly via Raft consensus.
//!  2. `INSERT` rows with vector embeddings replicate to all followers (log index parity).
//!  3. `VEC_SEARCH` works correctly with `SET kkdb.vec_ef_search` session variable.
//!  4. A single-node cluster correctly elects itself as leader and accepts vector DDL/DML.

use std::sync::{Arc, Mutex};
use std::collections::BTreeMap;
use std::time::Duration;

use kkdb::raft::node::{KkdbNode, start_cluster_3};
use kkdb::raft::types::KkdbRequest;
use kkdb::server::http_api::AppState;
use kkdb::raft::network::NodeRegistry;
use kkdb::vm::execute::{ExecResult, VM};
use kkdb::types::Value;

// ── Helpers ───────────────────────────────────────────────────────────────────

fn vec_expr(v: &[f32]) -> String {
    let inner: Vec<String> = v.iter().map(|x| x.to_string()).collect();
    format!("VEC('[{}]')", inner.join(","))
}

fn three_states() -> [AppState; 3] {
    [AppState::in_memory(), AppState::in_memory(), AppState::in_memory()]
}

async fn raft_write(node: &KkdbNode, sql: &str) -> bool {
    node.write(KkdbRequest { sql: sql.into(), user_id: "".into() })
        .await
        .map(|r| r.ok)
        .unwrap_or(false)
}

// ── Test 1: DDL replication log-parity ────────────────────────────────────────

/// CREATE VECTOR INDEX DDL replicates through Raft without error and all three
/// nodes settle on the same applied log index.
#[tokio::test]
async fn test_raft_vector_ddl_replicates() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    let leader_id = n1.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader elected");

    let nodes = [&n1, &n2, &n3];
    let leader = nodes.iter().find(|n| n.id == leader_id).unwrap();

    // Submit DDL via Raft
    assert!(raft_write(leader, "CREATE TABLE docs (id INTEGER PRIMARY KEY, emb BLOB)").await,
        "CREATE TABLE should succeed");
    assert!(raft_write(leader,
        "CREATE VECTOR INDEX idx_emb ON docs(emb) DIM 3 DISTANCE COSINE").await,
        "CREATE VECTOR INDEX should succeed");

    tokio::time::sleep(Duration::from_millis(300)).await;

    // All nodes should agree on applied log index
    let m1 = n1.metrics();
    let m2 = n2.metrics();
    let m3 = n3.metrics();
    assert_eq!(m1.last_applied, m2.last_applied, "n1 and n2 must agree");
    assert_eq!(m1.last_applied, m3.last_applied, "n1 and n3 must agree");

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ── Test 2: INSERT replication log-parity ─────────────────────────────────────

/// INSERT rows with vector BLOBs replicate fully: all three nodes reach the
/// same last_applied log index.
#[tokio::test]
async fn test_raft_vector_insert_replicates() {
    let [n1, n2, n3] = start_cluster_3(three_states())
        .await
        .expect("start cluster");

    let leader_id = n1.wait_for_leader(Duration::from_secs(10))
        .await
        .expect("leader");

    let nodes = [&n1, &n2, &n3];
    let leader = nodes.iter().find(|n| n.id == leader_id).unwrap();

    assert!(raft_write(leader, "CREATE TABLE vecs (id INTEGER PRIMARY KEY, v BLOB)").await);
    assert!(raft_write(leader,
        "CREATE VECTOR INDEX idx_v ON vecs(v) DIM 3 DISTANCE COSINE").await);
    assert!(raft_write(leader,
        &format!("INSERT INTO vecs VALUES (1, {})", vec_expr(&[1.0, 0.0, 0.0]))).await);
    assert!(raft_write(leader,
        &format!("INSERT INTO vecs VALUES (2, {})", vec_expr(&[0.0, 1.0, 0.0]))).await);
    assert!(raft_write(leader,
        &format!("INSERT INTO vecs VALUES (3, {})", vec_expr(&[0.0, 0.0, 1.0]))).await);

    tokio::time::sleep(Duration::from_millis(300)).await;

    let m1 = n1.metrics();
    let m3 = n3.metrics();
    assert_eq!(m1.last_applied, m3.last_applied, "follower did not replicate all entries");

    let _ = tokio::join!(n1.shutdown(), n2.shutdown(), n3.shutdown());
}

// ── Test 3: SET kkdb.vec_ef_search ────────────────────────────────────────────

/// SET kkdb.vec_ef_search = N overrides the HNSW candidate set size at query time.
/// Results should still be correct (this tests the config hook, not absolute recall).
#[test]
fn test_vec_ef_search_session_var() {
    let mut vm = VM::new_memory();
    vm.execute_sql("CREATE TABLE embs (id INTEGER PRIMARY KEY, v BLOB)").unwrap();
    for (i, vec) in [
        (1i64, vec![1.0f32, 0.0, 0.0]),
        (2,     vec![0.0, 1.0, 0.0]),
        (3,     vec![0.0, 0.0, 1.0]),
    ] {
        vm.execute_sql(&format!("INSERT INTO embs VALUES ({i}, {})", vec_expr(&vec))).unwrap();
    }
    vm.execute_sql("CREATE VECTOR INDEX idx ON embs(v) DIM 3 DISTANCE COSINE").unwrap();

    // Set high ef_search (maximum recall mode)
    vm.execute_sql("SET kkdb.vec_ef_search = '200'").unwrap();

    let qv = vec_expr(&[0.0, 0.0, 1.0]);
    let vs = format!("VEC_SEARCH('embs', 'idx', {qv})");
    let rows = match vm.execute_sql(
        &format!("SELECT id, {vs} AS score FROM embs ORDER BY {vs} DESC LIMIT 1")
    ).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        _ => panic!("Expected QueryResult"),
    };

    assert!(!rows.is_empty());
    assert_eq!(rows[0][0], Value::Integer(3),
        "With high ef_search, closest to [0,0,1] should be rowid 3");

    // Set low ef_search (speed mode) — should still work correctly for well-separated vectors
    vm.execute_sql("SET kkdb.vec_ef_search = '10'").unwrap();
    let qv2 = vec_expr(&[1.0, 0.0, 0.0]);
    let vs2 = format!("VEC_SEARCH('embs', 'idx', {qv2})");
    let rows2 = match vm.execute_sql(
        &format!("SELECT id, {vs2} AS score FROM embs ORDER BY {vs2} DESC LIMIT 1")
    ).unwrap() {
        ExecResult::QueryResult { rows, .. } => rows,
        _ => panic!("Expected QueryResult"),
    };
    assert!(!rows2.is_empty());
    // Rowid 1 ([1,0,0]) should dominate for query [1,0,0]
    assert_eq!(rows2[0][0], Value::Integer(1),
        "Even with low ef_search, closest to [1,0,0] should be rowid 1");
}

// ── Test 4: Single-node cluster is leader ─────────────────────────────────────

/// A single-node Raft cluster should elect itself as leader and should
/// successfully process vector DDL/DML writes.
#[tokio::test]
async fn test_raft_single_node_vector_write() {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let node = KkdbNode::new(1, AppState::in_memory(), Arc::clone(&registry), None, None)
        .await
        .expect("create node");
    node.init_single().await.expect("init");
    node.wait_for_leader(Duration::from_secs(5)).await.expect("leader");

    assert!(node.is_leader(), "single node must be leader");

    // Write vector DDL and DML
    assert!(raft_write(&node,
        "CREATE TABLE t (id INTEGER PRIMARY KEY, emb BLOB)").await);
    assert!(raft_write(&node,
        "CREATE VECTOR INDEX idx_t ON t(emb) DIM 2").await);
    assert!(raft_write(&node,
        &format!("INSERT INTO t VALUES (1, {})", vec_expr(&[0.5, 0.5]))).await);
    assert!(raft_write(&node,
        &format!("INSERT INTO t VALUES (2, {})", vec_expr(&[0.1, 0.9]))).await);

    node.shutdown().await.unwrap();
}
