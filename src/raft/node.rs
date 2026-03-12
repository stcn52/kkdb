//! High-level KKDB Raft node.
//!
//! `KkdbNode` wraps `openraft::Raft` and provides clean methods for:
//! - Initializing a cluster
//! - Submitting SQL writes through Raft
//! - Querying cluster status

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use openraft::{BasicNode, Config, Raft};

use crate::binlog::BinlogBroadcaster;
use crate::raft::{
    log_store::KkdbLogStore,
    network::{KkdbNetworkFactory, NodeRegistry},
    state_machine::KkdbStateMachine,
    types::{KkdbNodeId, KkdbRequest, KkdbResponse, KkdbTypeConfig},
};
use crate::server::http_api::AppState;

/// The concrete Raft type for KKDB.
pub type KkdbRaft = Raft<KkdbTypeConfig>;

/// A KKDB Raft node.
#[derive(Clone)]
pub struct KkdbNode {
    pub id: KkdbNodeId,
    pub raft: KkdbRaft,
    pub registry: NodeRegistry,
    /// Binlog broadcaster — Some if this node is the binlog source.
    pub binlog: Option<BinlogBroadcaster>,
}

impl KkdbNode {
    /// Create and register a new in-process node.
    ///
    /// `wal_dir` — if `Some(path)`, the Raft log is persisted to `{path}/raft/`.
    ///             If `None`, an in-memory log store is used (tests).
    /// `binlog`  — if `Some(broadcaster)`, every committed write is also emitted
    ///             to the binlog for subscriber fan-out and pull replication.
    pub async fn new(
        id: KkdbNodeId,
        app_state: AppState,
        registry: NodeRegistry,
        wal_dir: Option<&std::path::Path>,
        binlog: Option<BinlogBroadcaster>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let config = Arc::new(
            Config {
                heartbeat_interval: 250,
                election_timeout_min: 299,
                election_timeout_max: 500,
                ..Default::default()
            }
            .validate()
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
        );

        let log_store = match wal_dir {
            Some(dir) => KkdbLogStore::open(dir)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
            None => KkdbLogStore::default(),
        };
        let mut state_machine = match wal_dir {
            Some(dir) => KkdbStateMachine::open(app_state, dir)
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
            None => KkdbStateMachine::new(app_state),
        };
        // Wire the binlog broadcaster into the state machine
        state_machine.binlog = binlog.clone();

        let network = KkdbNetworkFactory::new(Arc::clone(&registry));

        let raft = Raft::new(id, config, network, log_store, state_machine)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;

        // Register in shared peer map
        registry.lock().unwrap().insert(id, Arc::new(raft.clone()));

        Ok(Self {
            id,
            raft,
            registry,
            binlog,
        })
    }

    /// Initialize a single-node cluster (node becomes Leader immediately).
    pub async fn init_single(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let mut members = BTreeMap::new();
        members.insert(
            self.id,
            BasicNode {
                addr: format!("node-{}", self.id),
            },
        );
        self.raft
            .initialize(members)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Initialize with a specific member set (for multi-node bootstrap).
    pub async fn init_with_members(
        &self,
        members: BTreeMap<KkdbNodeId, BasicNode>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.raft
            .initialize(members)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Submit a SQL write through Raft. Must be called on (or forwarded to) the Leader.
    pub async fn write(
        &self,
        req: KkdbRequest,
    ) -> Result<KkdbResponse, Box<dyn std::error::Error + Send + Sync>> {
        let resp = self
            .raft
            .client_write(req)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(resp.data)
    }

    /// Current Raft metrics snapshot.
    pub fn metrics(&self) -> openraft::RaftMetrics<KkdbNodeId, BasicNode> {
        self.raft.metrics().borrow().clone()
    }

    /// True if this node believes it is the current Leader.
    pub fn is_leader(&self) -> bool {
        let m = self.metrics();
        m.current_leader == Some(self.id)
    }

    /// Wait until a Leader is elected, up to `timeout`.
    pub async fn wait_for_leader(
        &self,
        timeout: std::time::Duration,
    ) -> Result<KkdbNodeId, Box<dyn std::error::Error + Send + Sync>> {
        let m = self
            .raft
            .wait(Some(timeout))
            .metrics(|m| m.current_leader.is_some(), "leader elected")
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(m.current_leader.unwrap())
    }

    // ── Distributed read / write helpers ─────────────────────────────────────

    /// Ensure this node has applied all log entries up to the current commit
    /// index before serving a read — i.e., ReadIndex linearizable read fence.
    ///
    /// Call this before executing SELECT on a follower so the local VM is
    /// up-to-date with the latest committed state.
    ///
    /// Returns `Ok(())` immediately on the leader (already up-to-date).
    /// Times out after 5 seconds if the cluster is unavailable.
    pub async fn ensure_linearizable(
        &self,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.raft
            .ensure_linearizable()
            .await
            .map(|_| ())
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    /// Get the REST base URL of the current leader, if known.
    ///
    /// `peer_rest_addrs` is passed in from `AppState`; it maps
    /// `node_id → "http://host:port"`.
    pub fn leader_rest_url(
        &self,
        peer_rest_addrs: &std::collections::BTreeMap<u64, String>,
    ) -> Option<String> {
        let leader_id = self.metrics().current_leader?;
        peer_rest_addrs.get(&leader_id).cloned()
    }

    /// Shutdown cleanly.
    pub async fn shutdown(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.raft
            .shutdown()
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
    }

    // ── Membership Change Helpers ────────────────────────────────────────────

    /// Add a learner (non-voting) node to the cluster.
    ///
    /// Must be called on the leader. The learner will start receiving log
    /// replication but will not participate in elections or quorum.
    pub async fn add_learner(
        &self,
        node_id: KkdbNodeId,
        addr: &str,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let node = BasicNode {
            addr: addr.to_string(),
        };
        self.raft
            .add_learner(node_id, node, true)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    /// Promote a learner to voter (full cluster member).
    ///
    /// Must be called on the leader. The node must already be a learner.
    /// This triggers a membership change through Raft consensus.
    pub async fn promote_to_voter(
        &self,
        node_id: KkdbNodeId,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let m = self.metrics();
        let membership = m.membership_config.membership();

        // Collect current voters + the new node
        let mut new_voters: BTreeMap<KkdbNodeId, BasicNode> = BTreeMap::new();
        for (&id, node) in membership.nodes() {
            new_voters.insert(id, node.clone());
        }
        new_voters.entry(node_id).or_insert_with(|| BasicNode {
            addr: format!("node-{}", node_id),
        });

        // Build the voter set (just the IDs)
        let voter_ids: std::collections::BTreeSet<KkdbNodeId> =
            new_voters.keys().copied().collect();

        self.raft
            .change_membership(voter_ids, false)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    /// Remove a node from the cluster.
    ///
    /// Must be called on the leader. Removes the node from both voter and
    /// learner sets.
    pub async fn remove_member(
        &self,
        node_id: KkdbNodeId,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let m = self.metrics();
        let membership = m.membership_config.membership();

        let mut voter_ids: std::collections::BTreeSet<KkdbNodeId> = std::collections::BTreeSet::new();
        for (&id, _) in membership.nodes() {
            if id != node_id {
                voter_ids.insert(id);
            }
        }

        self.raft
            .change_membership(voter_ids, false)
            .await
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
        Ok(())
    }

    /// Get the current cluster member list: (node_id, addr, is_voter).
    pub fn members(&self) -> Vec<(KkdbNodeId, String, bool)> {
        let m = self.metrics();
        let membership = m.membership_config.membership();
        let voter_ids = membership.voter_ids().collect::<std::collections::BTreeSet<_>>();

        membership
            .nodes()
            .map(|(&id, node)| (id, node.addr.clone(), voter_ids.contains(&id)))
            .collect()
    }
}

/// Start a fully initialized 3-node in-memory cluster.
/// Returns [leader, follower1, follower2].
pub async fn start_cluster_3(
    states: [AppState; 3],
) -> Result<[KkdbNode; 3], Box<dyn std::error::Error + Send + Sync>> {
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));

    let n1 = KkdbNode::new(1, states[0].clone(), Arc::clone(&registry), None, None).await?;
    let n2 = KkdbNode::new(2, states[1].clone(), Arc::clone(&registry), None, None).await?;
    let n3 = KkdbNode::new(3, states[2].clone(), Arc::clone(&registry), None, None).await?;

    let mut members = BTreeMap::new();
    members.insert(
        1u64,
        BasicNode {
            addr: "node-1".into(),
        },
    );
    members.insert(
        2u64,
        BasicNode {
            addr: "node-2".into(),
        },
    );
    members.insert(
        3u64,
        BasicNode {
            addr: "node-3".into(),
        },
    );

    // Only node 1 calls initialize; others receive membership via Raft replication
    n1.init_with_members(members).await?;

    Ok([n1, n2, n3])
}

// ─── Phase 2: HTTP-based cluster node ─────────────────────────────────────────

/// Create a Raft node that communicates with peers over HTTP (Phase 2).
///
/// `self_addr`   — this node's HTTP Raft RPC address (e.g. "http://127.0.0.1:7001")
/// `peer_addrs`  — map of other known nodes: { node_id → "http://host:port" }
/// `wal_dir`     — if `Some(dir)`, log is persisted to `{dir}/raft/`
pub async fn new_with_http_network(
    id: KkdbNodeId,
    app_state: AppState,
    self_addr: String,
    peer_addrs: BTreeMap<KkdbNodeId, String>,
    wal_dir: Option<std::path::PathBuf>,
    binlog: Option<crate::binlog::BinlogBroadcaster>,
) -> Result<KkdbNode, Box<dyn std::error::Error + Send + Sync>> {
    use crate::raft::http_network::{AddrRegistry, HttpNetworkFactory};

    // Build address registry including self
    let mut addrs = peer_addrs;
    addrs.insert(id, self_addr);
    let addr_registry: AddrRegistry = Arc::new(Mutex::new(addrs));

    let config = Arc::new(
        Config {
            heartbeat_interval: 250,
            election_timeout_min: 299,
            election_timeout_max: 500,
            ..Default::default()
        }
        .validate()
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
    );

    let log_store = match wal_dir.as_deref() {
        Some(dir) => crate::raft::log_store::KkdbLogStore::open(dir)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
        None => crate::raft::log_store::KkdbLogStore::default(),
    };
    let mut state_machine = match wal_dir.as_deref() {
        Some(dir) => crate::raft::state_machine::KkdbStateMachine::open(app_state, dir)
            .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?,
        None => crate::raft::state_machine::KkdbStateMachine::new(app_state),
    };
    state_machine.binlog = binlog.clone();

    let network = HttpNetworkFactory::new(Arc::clone(&addr_registry));

    // In-memory registry used only to store the self-handle for the HTTP server
    let registry: NodeRegistry = Arc::new(Mutex::new(BTreeMap::new()));
    let raft = Raft::new(id, config, network, log_store, state_machine)
        .await
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?;
    registry.lock().unwrap().insert(id, Arc::new(raft.clone()));

    Ok(KkdbNode {
        id,
        raft,
        registry,
        binlog,
    })
}

/// Bind the Raft HTTP RPC server on `raft_addr` (e.g. "0.0.0.0:7001").
/// This must be called after creating the node so handlers can access `node.raft`.
pub async fn start_raft_http_server(node: Arc<KkdbNode>, raft_addr: std::net::SocketAddr) {
    use crate::raft::http_transport::build_raft_router;
    let router = build_raft_router(node);
    let listener = tokio::net::TcpListener::bind(raft_addr)
        .await
        .expect("bind Raft HTTP port");
    axum::serve(listener, router)
        .await
        .expect("Raft HTTP server");
}
