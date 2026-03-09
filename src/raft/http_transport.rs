//! HTTP JSON-RPC transport for KKDB Raft (Phase 2, cross-process).
//!
//! Replaces the in-memory channel transport with real HTTP calls so that
//! individual KKDB nodes can run in separate processes (or on separate hosts).
//!
//! ## Endpoints
//!
//! | Method | Path                          | Description                      |
//! |--------|-------------------------------|----------------------------------|
//! | POST   | `/raft/append-entries`        | AppendEntriesRequest (internal)  |
//! | POST   | `/raft/vote`                  | VoteRequest (internal)           |
//! | POST   | `/raft/install-snapshot`      | InstallSnapshotRequest (internal)|
//! | POST   | `/raft/init`                  | Cluster bootstrap                |
//! | POST   | `/raft/add-learner`           | Add a new learner node           |
//! | POST   | `/raft/change-membership`     | Promote / demote nodes           |
//! | POST   | `/raft/status`                | Basic status (legacy)            |
//! | GET    | `/raft/metrics`               | Rich JSON cluster metrics        |
//! | GET    | `/raft/metrics/prometheus`    | Prometheus text exposition       |

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use openraft::BasicNode;
use serde::{Deserialize, Serialize};

use crate::binlog::BinlogBroadcaster;
use crate::raft::log_store::KkdbLogStore;
use crate::raft::node::KkdbNode;
use crate::raft::types::{KkdbNodeId, KkdbTypeConfig};

// ─── Shared state for the Raft HTTP server ────────────────────────────────────

/// State injected into every Raft RPC handler.
#[derive(Clone)]
pub struct RaftApiState {
    pub node: Arc<KkdbNode>,
    /// WAL log store — for compaction stats in metrics endpoint.
    pub log_store: Option<KkdbLogStore>,
    /// Binlog broadcaster — for `/binlog/stream` replication endpoint.
    pub binlog: Option<BinlogBroadcaster>,
}

// ─── Request / response helpers ───────────────────────────────────────────────

/// Payload for /raft/init: map of node_id → addr
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitRequest {
    /// e.g. { "1": "127.0.0.1:7001", "2": "127.0.0.1:7002" }
    pub nodes: BTreeMap<KkdbNodeId, String>,
}

/// Payload for /raft/add-learner and /raft/change-membership
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipRequest {
    pub node_id: KkdbNodeId,
    pub addr: Option<String>,
    /// For change-membership: if true, retain existing voters; if false replace
    pub retain: Option<bool>,
}

// ─── Rich metrics types ───────────────────────────────────────────────────────

/// Full JSON metrics snapshot for GET /raft/metrics.
#[derive(Debug, Serialize)]
pub struct RaftMetricsJson {
    pub node_id: KkdbNodeId,
    pub role: String,
    pub current_leader: Option<KkdbNodeId>,
    pub current_term: u64,
    pub last_log_index: Option<u64>,
    pub last_applied_index: Option<u64>,
    pub snapshot_last_log_index: Option<u64>,
    pub membership_voter_ids: Vec<KkdbNodeId>,
    pub wal: WalMetrics,
}

/// WAL compaction diagnostics.
#[derive(Debug, Serialize)]
pub struct WalMetrics {
    pub live_records: u64,
    pub total_records: u64,
    pub dead_records: u64,
    pub compaction_ratio_pct: u64,
}

// ─── Internal helper: build metrics snapshot ─────────────────────────────────

fn build_metrics_json(s: &RaftApiState) -> RaftMetricsJson {
    let m = s.node.metrics();

    let role = if m.current_leader == Some(s.node.id) {
        "Leader"
    } else if m.current_leader.is_some() {
        "Follower"
    } else {
        "Candidate"
    }
    .to_string();

    let membership_voter_ids = m
        .membership_config
        .membership()
        .voter_ids()
        .collect::<Vec<_>>();

    let last_log_index = m.last_log_index;
    let last_applied_index = m.last_applied.map(|l| l.index);
    let snapshot_last_log_index = m.snapshot.as_ref().map(|s| s.index);

    // WAL compaction stats
    let wal = if let Some(ref store) = s.log_store {
        let (live, total, dead) = store.compaction_stats();
        let ratio = if total > 0 { dead * 100 / total } else { 0 };
        WalMetrics {
            live_records: live,
            total_records: total,
            dead_records: dead,
            compaction_ratio_pct: ratio,
        }
    } else {
        WalMetrics {
            live_records: 0,
            total_records: 0,
            dead_records: 0,
            compaction_ratio_pct: 0,
        }
    };

    RaftMetricsJson {
        node_id: s.node.id,
        role,
        current_leader: m.current_leader,
        current_term: m.current_term,
        last_log_index,
        last_applied_index,
        snapshot_last_log_index,
        membership_voter_ids,
        wal,
    }
}

/// Render a metrics snapshot as Prometheus text exposition format.
fn render_prometheus(metrics: &RaftMetricsJson) -> String {
    let mut out = String::with_capacity(1024);

    let push = |out: &mut String, name: &str, help: &str, typ: &str, value: u64| {
        out.push_str(&format!("# HELP kkdb_{name} {help}\n"));
        out.push_str(&format!("# TYPE kkdb_{name} {typ}\n"));
        out.push_str(&format!(
            "kkdb_{name}{{node=\"{}\"}} {value}\n\n",
            metrics.node_id
        ));
    };

    push(
        &mut out,
        "raft_is_leader",
        "1 if this node is the current Raft leader, 0 otherwise",
        "gauge",
        if metrics.current_leader == Some(metrics.node_id) {
            1
        } else {
            0
        },
    );
    push(
        &mut out,
        "raft_current_term",
        "Current Raft term",
        "gauge",
        metrics.current_term,
    );
    push(
        &mut out,
        "raft_last_log_index",
        "Index of the last log entry",
        "gauge",
        metrics.last_log_index.unwrap_or(0),
    );
    push(
        &mut out,
        "raft_last_applied_index",
        "Index of the last log entry applied to the state machine",
        "gauge",
        metrics.last_applied_index.unwrap_or(0),
    );
    push(
        &mut out,
        "raft_snapshot_last_log_index",
        "Last log index covered by the most recent snapshot",
        "gauge",
        metrics.snapshot_last_log_index.unwrap_or(0),
    );
    push(
        &mut out,
        "wal_live_records",
        "Live log entries currently in the WAL",
        "gauge",
        metrics.wal.live_records,
    );
    push(
        &mut out,
        "wal_total_records",
        "Total records ever written to the WAL (live + dead)",
        "counter",
        metrics.wal.total_records,
    );
    push(
        &mut out,
        "wal_dead_records",
        "Dead records in the WAL eligible for compaction",
        "gauge",
        metrics.wal.dead_records,
    );
    push(
        &mut out,
        "wal_compaction_ratio_pct",
        "Percentage of WAL records that are dead (0-100)",
        "gauge",
        metrics.wal.compaction_ratio_pct,
    );
    push(
        &mut out,
        "membership_voter_count",
        "Number of voting members in the current Raft membership config",
        "gauge",
        metrics.membership_voter_ids.len() as u64,
    );

    out
}

// ─── Raft internal RPC handlers ───────────────────────────────────────────────

async fn append_entries_handler(
    State(s): State<RaftApiState>,
    Json(rpc): Json<openraft::raft::AppendEntriesRequest<KkdbTypeConfig>>,
) -> impl IntoResponse {
    match s.node.raft.append_entries(rpc).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn vote_handler(
    State(s): State<RaftApiState>,
    Json(rpc): Json<openraft::raft::VoteRequest<KkdbNodeId>>,
) -> impl IntoResponse {
    match s.node.raft.vote(rpc).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn install_snapshot_handler(
    State(s): State<RaftApiState>,
    Json(rpc): Json<openraft::raft::InstallSnapshotRequest<KkdbTypeConfig>>,
) -> impl IntoResponse {
    match s.node.raft.install_snapshot(rpc).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

// ─── Cluster admin handlers ───────────────────────────────────────────────────

async fn init_handler(
    State(s): State<RaftApiState>,
    Json(req): Json<InitRequest>,
) -> impl IntoResponse {
    let members: BTreeMap<KkdbNodeId, BasicNode> = req
        .nodes
        .into_iter()
        .map(|(id, addr)| (id, BasicNode { addr }))
        .collect();
    match s.node.init_with_members(members).await {
        Ok(_) => (StatusCode::OK, Json(serde_json::json!({ "ok": true }))),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn add_learner_handler(
    State(s): State<RaftApiState>,
    Json(req): Json<MembershipRequest>,
) -> impl IntoResponse {
    let node_info = BasicNode {
        addr: req.addr.unwrap_or_default(),
    };
    match s.node.raft.add_learner(req.node_id, node_info, true).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

async fn change_membership_handler(
    State(s): State<RaftApiState>,
    Json(req): Json<MembershipRequest>,
) -> impl IntoResponse {
    let retain = req.retain.unwrap_or(true);
    let mut voters = std::collections::BTreeSet::new();
    voters.insert(req.node_id);
    match s.node.raft.change_membership(voters, retain).await {
        Ok(resp) => (StatusCode::OK, Json(serde_json::to_value(resp).unwrap())),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        ),
    }
}

/// Legacy POST /raft/status (kept for backward compat)
async fn status_handler(State(s): State<RaftApiState>) -> impl IntoResponse {
    let m = s.node.metrics();
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "node_id": s.node.id,
            "leader":  m.current_leader,
            "term":    m.current_term,
            "last_log_index": m.last_log_index,
            "last_applied":   m.last_applied,
            "is_leader": s.node.is_leader(),
        })),
    )
}

// ─── Monitoring endpoints ─────────────────────────────────────────────────────

/// GET /raft/metrics — rich JSON cluster + WAL metrics.
async fn metrics_json_handler(State(s): State<RaftApiState>) -> impl IntoResponse {
    let metrics = build_metrics_json(&s);
    (StatusCode::OK, Json(metrics))
}

/// GET /raft/metrics/prometheus — Prometheus text exposition format.
async fn metrics_prometheus_handler(State(s): State<RaftApiState>) -> impl IntoResponse {
    let metrics = build_metrics_json(&s);
    let body = render_prometheus(&metrics);
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

// ─── Binlog streaming handler ─────────────────────────────────────────────────

/// Query params for GET /binlog/stream
#[derive(Debug, Deserialize)]
struct BinlogStreamParams {
    /// Byte offset to start reading from (default = 0 = full history).
    #[serde(default)]
    from_pos: u64,
}

/// GET /binlog/stream?from_pos=N
///
/// Returns all binlog records from byte offset `from_pos` onwards as NDJSON.
/// Each line is a JSON object: `{"pos": <u64>, "data": "<base64>"}` where:
/// - `pos`  is the byte offset *after* this record (use as the next `from_pos`).
/// - `data` is base64-encoded framed bytes: `[len:u32 LE][crc32:u32 LE][payload]`.
///
/// Consumers should poll this endpoint (long-polling or periodic pull) and
/// advance `from_pos` each time. A `pos == 0` response means no new data.
async fn binlog_stream_handler(
    State(s): State<RaftApiState>,
    Query(params): Query<BinlogStreamParams>,
) -> impl IntoResponse {
    let Some(ref broadcaster) = s.binlog else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "application/x-ndjson")],
            "{\"error\":\"binlog not enabled on this node\"}".to_string(),
        );
    };

    let mgr = broadcaster.manager.lock().unwrap();
    let records = match mgr.read_from(params.from_pos) {
        Ok(r) => r,
        Err(e) => {
            drop(mgr);
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "application/x-ndjson")],
                format!("{{\"error\":\"{}\"}}", e),
            );
        }
    };
    drop(mgr);

    // Serialize each (next_pos, framed_bytes) as one NDJSON line
    let mut body = String::new();
    for (next_pos, framed) in records {
        let data_b64 = crate::binlog::base64_encode(&framed);
        body.push_str(&format!(
            "{{\"pos\":{next_pos},\"data\":\"{data_b64}\"}}
"
        ));
    }

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        body,
    )
}

// ─── Router builder ───────────────────────────────────────────────────────────

/// Build the Raft-internal HTTP router for a node.
/// Mount this on a separate port (e.g. 7001) from the REST API port.
///
/// Pass `log_store` to enable WAL compaction stats in the metrics endpoint.
pub fn build_raft_router(node: Arc<KkdbNode>) -> Router {
    build_raft_router_with_store(node, None, None)
}

pub fn build_raft_router_with_store(
    node: Arc<KkdbNode>,
    log_store: Option<KkdbLogStore>,
    binlog: Option<BinlogBroadcaster>,
) -> Router {
    let state = RaftApiState {
        node,
        log_store,
        binlog,
    };
    Router::new()
        // Internal Raft RPCs (called by other nodes)
        .route("/raft/append-entries", post(append_entries_handler))
        .route("/raft/vote", post(vote_handler))
        .route("/raft/install-snapshot", post(install_snapshot_handler))
        // Cluster admin
        .route("/raft/init", post(init_handler))
        .route("/raft/add-learner", post(add_learner_handler))
        .route("/raft/change-membership", post(change_membership_handler))
        .route("/raft/status", post(status_handler))
        // Monitoring
        .route("/raft/metrics", get(metrics_json_handler))
        .route("/raft/metrics/prometheus", get(metrics_prometheus_handler))
        // Binlog streaming (pull replication)
        .route("/binlog/stream", get(binlog_stream_handler))
        .with_state(state)
}
