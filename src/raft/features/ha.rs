// R13 – High Availability: Leader election, automatic failover, read-replica routing.
//
// Provides:
//   - `NodeState`: enumeration of node lifecycle states
//   - `LeaderElection`: pre-vote + election logic with term/timeout management
//   - `FailoverManager`: health monitoring + automatic leader failover
//   - `ReadReplicaRouter`: routes read queries to nearest/least-loaded replica

use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Node State ────────────────────────────────────────────────────────

/// Possible states of a Raft/cluster node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeState {
    Follower,
    Candidate,
    Leader,
    PreVote, // Pre-vote phase (avoids unnecessary term increment)
    Offline,
    Recovering,
}

impl std::fmt::Display for NodeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Follower => write!(f, "Follower"),
            Self::Candidate => write!(f, "Candidate"),
            Self::Leader => write!(f, "Leader"),
            Self::PreVote => write!(f, "PreVote"),
            Self::Offline => write!(f, "Offline"),
            Self::Recovering => write!(f, "Recovering"),
        }
    }
}

// ── Leader Election ───────────────────────────────────────────────────

/// Leader election manager with pre-vote support.
pub struct LeaderElection {
    node_id: u64,
    current_term: u64,
    voted_for: Option<u64>,
    state: NodeState,
    election_timeout: Duration,
    last_heartbeat: Instant,
    leader_id: Option<u64>,
    votes_received: Vec<u64>,
    cluster_size: usize,
}

impl LeaderElection {
    pub fn new(node_id: u64, cluster_size: usize, election_timeout: Duration) -> Self {
        Self {
            node_id,
            current_term: 0,
            voted_for: None,
            state: NodeState::Follower,
            election_timeout,
            last_heartbeat: Instant::now(),
            leader_id: None,
            votes_received: Vec::new(),
            cluster_size,
        }
    }

    pub fn node_id(&self) -> u64 {
        self.node_id
    }

    pub fn current_term(&self) -> u64 {
        self.current_term
    }

    pub fn state(&self) -> NodeState {
        self.state
    }

    pub fn leader_id(&self) -> Option<u64> {
        self.leader_id
    }

    pub fn is_leader(&self) -> bool {
        self.state == NodeState::Leader
    }

    /// Receive a heartbeat from the leader.
    pub fn receive_heartbeat(&mut self, leader_id: u64, term: u64) {
        if term >= self.current_term {
            self.current_term = term;
            self.state = NodeState::Follower;
            self.leader_id = Some(leader_id);
            self.last_heartbeat = Instant::now();
            self.voted_for = None;
        }
    }

    /// Check if election timeout has elapsed.
    pub fn election_timeout_elapsed(&self) -> bool {
        self.last_heartbeat.elapsed() >= self.election_timeout
    }

    /// Start a pre-vote phase.
    pub fn start_pre_vote(&mut self) {
        self.state = NodeState::PreVote;
        self.votes_received.clear();
        self.votes_received.push(self.node_id); // vote for self
    }

    /// Start a full election (increment term, become candidate).
    pub fn start_election(&mut self) {
        self.current_term += 1;
        self.state = NodeState::Candidate;
        self.voted_for = Some(self.node_id);
        self.votes_received.clear();
        self.votes_received.push(self.node_id);
        self.last_heartbeat = Instant::now(); // reset timeout
    }

    /// Record a vote received for this node.
    pub fn receive_vote(&mut self, voter_id: u64) -> bool {
        if !self.votes_received.contains(&voter_id) {
            self.votes_received.push(voter_id);
        }
        // Check if we have a majority
        if self.votes_received.len() > self.cluster_size / 2 {
            self.state = NodeState::Leader;
            self.leader_id = Some(self.node_id);
            return true; // became leader
        }
        false
    }

    /// Handle a vote request from another node.
    ///
    /// Returns `true` if we grant our vote.
    pub fn handle_vote_request(&mut self, candidate_id: u64, candidate_term: u64) -> bool {
        if candidate_term < self.current_term {
            return false;
        }
        if candidate_term > self.current_term {
            self.current_term = candidate_term;
            self.state = NodeState::Follower;
            self.voted_for = None;
        }
        if self.voted_for.is_none() || self.voted_for == Some(candidate_id) {
            self.voted_for = Some(candidate_id);
            self.last_heartbeat = Instant::now();
            return true;
        }
        false
    }

    /// Step down from leader/candidate to follower.
    pub fn step_down(&mut self, new_term: u64) {
        if new_term > self.current_term {
            self.current_term = new_term;
        }
        self.state = NodeState::Follower;
        self.voted_for = None;
        self.leader_id = None;
    }

    pub fn votes_received(&self) -> usize {
        self.votes_received.len()
    }
}

// ── Failover Manager ──────────────────────────────────────────────────

/// Health status of a cluster node.
#[derive(Debug, Clone)]
pub struct NodeHealth {
    pub node_id: u64,
    pub state: NodeState,
    pub last_seen: Instant,
    pub failed_health_checks: u32,
    pub max_failures: u32,
}

impl NodeHealth {
    pub fn new(node_id: u64, max_failures: u32) -> Self {
        Self {
            node_id,
            state: NodeState::Follower,
            last_seen: Instant::now(),
            failed_health_checks: 0,
            max_failures,
        }
    }

    /// Record a successful health check.
    pub fn mark_healthy(&mut self) {
        self.last_seen = Instant::now();
        self.failed_health_checks = 0;
        if self.state == NodeState::Offline {
            self.state = NodeState::Recovering;
        }
    }

    /// Record a failed health check.
    pub fn mark_unhealthy(&mut self) {
        self.failed_health_checks += 1;
        if self.failed_health_checks >= self.max_failures {
            self.state = NodeState::Offline;
        }
    }

    /// Check if the node is considered alive.
    pub fn is_alive(&self) -> bool {
        self.state != NodeState::Offline
    }
}

/// Manages automatic failover for a cluster.
pub struct FailoverManager {
    nodes: HashMap<u64, NodeHealth>,
    current_leader: Option<u64>,
    failover_count: u64,
}

impl Default for FailoverManager {
    fn default() -> Self {
        Self::new()
    }
}

impl FailoverManager {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            current_leader: None,
            failover_count: 0,
        }
    }

    /// Register a node in the cluster.
    pub fn add_node(&mut self, node_id: u64, max_failures: u32) {
        self.nodes
            .insert(node_id, NodeHealth::new(node_id, max_failures));
    }

    /// Remove a node from the cluster.
    pub fn remove_node(&mut self, node_id: u64) -> bool {
        if self.current_leader == Some(node_id) {
            self.current_leader = None;
        }
        self.nodes.remove(&node_id).is_some()
    }

    /// Set the current leader.
    pub fn set_leader(&mut self, node_id: u64) {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            node.state = NodeState::Leader;
        }
        self.current_leader = Some(node_id);
    }

    /// Report a health check result.
    pub fn health_check(&mut self, node_id: u64, healthy: bool) {
        if let Some(node) = self.nodes.get_mut(&node_id) {
            if healthy {
                node.mark_healthy();
            } else {
                node.mark_unhealthy();
            }
        }
    }

    /// Check if leader failover is needed and perform it.
    ///
    /// Returns the new leader's node_id if failover occurred.
    pub fn check_failover(&mut self) -> Option<u64> {
        let leader = self.current_leader?;
        let leader_health = self.nodes.get(&leader)?;
        if leader_health.is_alive() {
            return None; // leader is fine
        }
        // Leader is offline — pick a new one (lowest alive node_id)
        let new_leader = self
            .nodes
            .values()
            .filter(|n| n.is_alive() && n.node_id != leader)
            .min_by_key(|n| n.node_id)
            .map(|n| n.node_id)?;

        self.set_leader(new_leader);
        self.failover_count += 1;
        Some(new_leader)
    }

    pub fn current_leader(&self) -> Option<u64> {
        self.current_leader
    }

    pub fn alive_nodes(&self) -> Vec<u64> {
        self.nodes
            .values()
            .filter(|n| n.is_alive())
            .map(|n| n.node_id)
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn failover_count(&self) -> u64 {
        self.failover_count
    }
}

// ── Read Replica Router ───────────────────────────────────────────────

/// Routes read queries to the best available replica.
pub struct ReadReplicaRouter {
    /// node_id → current load (number of active queries).
    load: HashMap<u64, u32>,
    /// node_id → latency estimate in ms.
    latency: HashMap<u64, u32>,
    leader_id: Option<u64>,
}

impl Default for ReadReplicaRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl ReadReplicaRouter {
    pub fn new() -> Self {
        Self {
            load: HashMap::new(),
            latency: HashMap::new(),
            leader_id: None,
        }
    }

    pub fn set_leader(&mut self, node_id: u64) {
        self.leader_id = Some(node_id);
    }

    /// Register a replica with initial load and latency.
    pub fn add_replica(&mut self, node_id: u64, latency_ms: u32) {
        self.load.insert(node_id, 0);
        self.latency.insert(node_id, latency_ms);
    }

    /// Remove a replica.
    pub fn remove_replica(&mut self, node_id: u64) {
        self.load.remove(&node_id);
        self.latency.remove(&node_id);
    }

    /// Update load for a replica.
    pub fn update_load(&mut self, node_id: u64, active_queries: u32) {
        if let Some(l) = self.load.get_mut(&node_id) {
            *l = active_queries;
        }
    }

    /// Route a read query to the best replica (lowest latency × load score).
    ///
    /// Returns the node_id of the chosen replica. Falls back to leader if no replicas.
    pub fn route_read(&self) -> Option<u64> {
        // Exclude leader from read-replica routing (leader handles writes)
        let candidates: Vec<_> = self
            .load
            .iter()
            .filter(|(&id, _)| Some(id) != self.leader_id)
            .collect();

        if candidates.is_empty() {
            return self.leader_id; // fallback to leader
        }

        // Score = latency * (1 + load)
        candidates
            .iter()
            .min_by_key(|(&id, &load)| {
                let lat = self.latency.get(&id).copied().unwrap_or(100) as u64;
                lat * (1 + load as u64)
            })
            .map(|(&id, _)| id)
    }

    /// Route a write query (always to leader).
    pub fn route_write(&self) -> Option<u64> {
        self.leader_id
    }

    /// Number of registered replicas.
    pub fn replica_count(&self) -> usize {
        self.load.len()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_election_start_and_win() {
        let mut le = LeaderElection::new(1, 3, Duration::from_millis(150));
        assert_eq!(le.state(), NodeState::Follower);
        le.start_election();
        assert_eq!(le.state(), NodeState::Candidate);
        assert_eq!(le.current_term(), 1);
        // Already has self-vote; need 1 more for majority (3/2 + 1 = 2)
        let became_leader = le.receive_vote(2);
        assert!(became_leader);
        assert!(le.is_leader());
    }

    #[test]
    fn leader_election_pre_vote() {
        let mut le = LeaderElection::new(1, 5, Duration::from_millis(150));
        le.start_pre_vote();
        assert_eq!(le.state(), NodeState::PreVote);
        assert_eq!(le.votes_received(), 1); // self-vote
    }

    #[test]
    fn leader_election_vote_request() {
        let mut le = LeaderElection::new(2, 3, Duration::from_millis(150));
        // Higher term: grant vote
        assert!(le.handle_vote_request(1, 5));
        assert_eq!(le.current_term(), 5);
        // Already voted: deny
        assert!(!le.handle_vote_request(3, 5));
    }

    #[test]
    fn leader_election_step_down() {
        let mut le = LeaderElection::new(1, 3, Duration::from_millis(150));
        le.start_election();
        le.receive_vote(2);
        assert!(le.is_leader());
        le.step_down(10);
        assert_eq!(le.state(), NodeState::Follower);
        assert_eq!(le.current_term(), 10);
    }

    #[test]
    fn leader_election_heartbeat() {
        let mut le = LeaderElection::new(2, 3, Duration::from_millis(150));
        le.receive_heartbeat(1, 5);
        assert_eq!(le.state(), NodeState::Follower);
        assert_eq!(le.leader_id(), Some(1));
        assert_eq!(le.current_term(), 5);
    }

    #[test]
    fn failover_manager_basic() {
        let mut fm = FailoverManager::new();
        fm.add_node(1, 3);
        fm.add_node(2, 3);
        fm.add_node(3, 3);
        fm.set_leader(1);
        assert_eq!(fm.current_leader(), Some(1));

        // Leader fails
        for _ in 0..3 {
            fm.health_check(1, false);
        }
        let new_leader = fm.check_failover();
        assert!(new_leader.is_some());
        assert_ne!(new_leader.unwrap(), 1);
        assert_eq!(fm.failover_count(), 1);
    }

    #[test]
    fn failover_manager_no_failover_when_healthy() {
        let mut fm = FailoverManager::new();
        fm.add_node(1, 3);
        fm.set_leader(1);
        fm.health_check(1, true);
        assert_eq!(fm.check_failover(), None);
    }

    #[test]
    fn failover_manager_alive_nodes() {
        let mut fm = FailoverManager::new();
        fm.add_node(1, 2);
        fm.add_node(2, 2);
        fm.add_node(3, 2);
        fm.health_check(3, false);
        fm.health_check(3, false); // 2 failures = offline
        let alive = fm.alive_nodes();
        assert_eq!(alive.len(), 2);
        assert!(!alive.contains(&3));
    }

    #[test]
    fn read_replica_router() {
        let mut rr = ReadReplicaRouter::new();
        rr.set_leader(1);
        rr.add_replica(1, 5);
        rr.add_replica(2, 10);
        rr.add_replica(3, 15);

        // Read should go to replica, not leader
        let target = rr.route_read().unwrap();
        assert_ne!(target, 1); // should not route to leader

        // Write always goes to leader
        assert_eq!(rr.route_write(), Some(1));
    }

    #[test]
    fn read_replica_router_load_aware() {
        let mut rr = ReadReplicaRouter::new();
        rr.set_leader(1);
        rr.add_replica(1, 5);
        rr.add_replica(2, 10);
        rr.add_replica(3, 10);
        rr.update_load(2, 100); // heavily loaded
        rr.update_load(3, 1); // lightly loaded

        let target = rr.route_read().unwrap();
        assert_eq!(target, 3); // should pick less loaded
    }

    #[test]
    fn node_state_display() {
        assert_eq!(format!("{}", NodeState::Leader), "Leader");
        assert_eq!(format!("{}", NodeState::Follower), "Follower");
        assert_eq!(format!("{}", NodeState::PreVote), "PreVote");
    }
}
