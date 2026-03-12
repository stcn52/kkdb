// R15 – Distributed cluster management: Raft log compaction,
//       membership change protocol, cross-datacenter replication lag monitoring,
//       cluster topology discovery.
//
// Provides:
//   - `LogCompactor`: Raft log compaction with snapshot trigger
//   - `MembershipChange`: joint consensus membership protocol
//   - `ReplicationLagMonitor`: cross-DC replication lag tracking with alerts
//   - `TopologyDiscovery`: cluster topology graph + partition mapping

use std::collections::{HashMap, HashSet, VecDeque};

// ── Raft Log Compaction ───────────────────────────────────────────────

/// A compacted snapshot reference.
#[derive(Debug, Clone)]
pub struct SnapshotRef {
    pub snapshot_id: u64,
    pub last_included_index: u64,
    pub last_included_term: u64,
    pub byte_size: usize,
    pub timestamp: u64,
}

/// Manages Raft log compaction decisions.
pub struct LogCompactor {
    /// Current log size (number of entries).
    log_size: u64,
    /// Last compacted index.
    last_compacted_index: u64,
    /// Compaction threshold (trigger when log exceeds this).
    threshold: u64,
    /// History of snapshots taken.
    snapshots: Vec<SnapshotRef>,
    next_snapshot_id: u64,
}

impl LogCompactor {
    pub fn new(threshold: u64) -> Self {
        Self {
            log_size: 0,
            last_compacted_index: 0,
            threshold,
            snapshots: Vec::new(),
            next_snapshot_id: 1,
        }
    }

    /// Append log entries.
    pub fn append_entries(&mut self, count: u64) {
        self.log_size += count;
    }

    /// Check if compaction is needed.
    pub fn needs_compaction(&self) -> bool {
        self.log_size - self.last_compacted_index > self.threshold
    }

    /// Perform compaction: create a snapshot up to the given index/term.
    pub fn compact(&mut self, up_to_index: u64, term: u64, snapshot_size: usize) -> SnapshotRef {
        let snap = SnapshotRef {
            snapshot_id: self.next_snapshot_id,
            last_included_index: up_to_index,
            last_included_term: term,
            byte_size: snapshot_size,
            timestamp: self.next_snapshot_id * 1000, // simulated
        };
        self.next_snapshot_id += 1;
        self.last_compacted_index = up_to_index;
        self.snapshots.push(snap.clone());
        snap
    }

    pub fn log_size(&self) -> u64 {
        self.log_size
    }

    pub fn last_compacted_index(&self) -> u64 {
        self.last_compacted_index
    }

    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    pub fn latest_snapshot(&self) -> Option<&SnapshotRef> {
        self.snapshots.last()
    }
}

// ── Membership Change Protocol ────────────────────────────────────────

/// Cluster membership state.
#[derive(Debug, Clone, PartialEq)]
pub enum MembershipState {
    /// Stable configuration.
    Stable,
    /// Joint consensus: old and new configs both active.
    Joint,
    /// Transitioning from joint to new stable.
    Transitioning,
}

/// A membership configuration.
#[derive(Debug, Clone)]
pub struct MembershipConfig {
    pub config_id: u64,
    pub members: HashSet<u64>,
    pub state: MembershipState,
}

/// Manages membership changes via joint consensus.
pub struct MembershipChange {
    current: MembershipConfig,
    pending_adds: Vec<u64>,
    pending_removes: Vec<u64>,
    history: Vec<MembershipConfig>,
}

impl MembershipChange {
    pub fn new(initial_members: HashSet<u64>) -> Self {
        let config = MembershipConfig {
            config_id: 1,
            members: initial_members,
            state: MembershipState::Stable,
        };
        Self {
            current: config,
            pending_adds: Vec::new(),
            pending_removes: Vec::new(),
            history: Vec::new(),
        }
    }

    /// Propose adding a node.
    pub fn propose_add(&mut self, node_id: u64) -> bool {
        if self.current.members.contains(&node_id) {
            return false;
        }
        self.pending_adds.push(node_id);
        true
    }

    /// Propose removing a node.
    pub fn propose_remove(&mut self, node_id: u64) -> bool {
        if !self.current.members.contains(&node_id) {
            return false;
        }
        self.pending_removes.push(node_id);
        true
    }

    /// Enter joint consensus phase.
    pub fn enter_joint(&mut self) -> MembershipState {
        if self.pending_adds.is_empty() && self.pending_removes.is_empty() {
            return self.current.state.clone();
        }
        self.history.push(self.current.clone());
        self.current.state = MembershipState::Joint;
        // Add new members to joint config
        for &node in &self.pending_adds {
            self.current.members.insert(node);
        }
        MembershipState::Joint
    }

    /// Commit the membership change (transition from joint to new stable).
    pub fn commit_change(&mut self) -> MembershipState {
        if self.current.state != MembershipState::Joint {
            return self.current.state.clone();
        }
        self.current.state = MembershipState::Transitioning;
        // Remove pending nodes
        for &node in &self.pending_removes {
            self.current.members.remove(&node);
        }
        self.pending_adds.clear();
        self.pending_removes.clear();
        self.current.config_id += 1;
        self.current.state = MembershipState::Stable;
        MembershipState::Stable
    }

    pub fn members(&self) -> &HashSet<u64> {
        &self.current.members
    }

    pub fn state(&self) -> &MembershipState {
        &self.current.state
    }

    pub fn member_count(&self) -> usize {
        self.current.members.len()
    }

    /// Quorum size for current config.
    pub fn quorum_size(&self) -> usize {
        self.current.members.len() / 2 + 1
    }
}

// ── Replication Lag Monitor ───────────────────────────────────────────

/// Per-node replication lag information.
#[derive(Debug, Clone)]
pub struct ReplicaLag {
    pub node_id: u64,
    pub datacenter: String,
    pub lag_ms: u64,
    pub last_applied_index: u64,
    pub last_check: u64,
}

/// Alert thresholds.
#[derive(Debug, Clone)]
pub struct LagAlertConfig {
    pub warning_ms: u64,
    pub critical_ms: u64,
}

/// LAg alert levels.
#[derive(Debug, Clone, PartialEq)]
pub enum LagAlert {
    Normal,
    Warning(u64, u64), // node_id, lag_ms
    Critical(u64, u64),
}

/// Monitors cross-datacenter replication lag.
pub struct ReplicationLagMonitor {
    replicas: HashMap<u64, ReplicaLag>,
    alert_config: LagAlertConfig,
    alerts: Vec<LagAlert>,
    lag_history: VecDeque<(u64, u64)>, // (timestamp, max_lag)
    max_history: usize,
}

impl ReplicationLagMonitor {
    pub fn new(warning_ms: u64, critical_ms: u64) -> Self {
        Self {
            replicas: HashMap::new(),
            alert_config: LagAlertConfig { warning_ms, critical_ms },
            alerts: Vec::new(),
            lag_history: VecDeque::new(),
            max_history: 100,
        }
    }

    /// Update replication lag for a node.
    pub fn update_lag(
        &mut self,
        node_id: u64,
        datacenter: &str,
        lag_ms: u64,
        last_applied: u64,
        timestamp: u64,
    ) {
        let lag = ReplicaLag {
            node_id,
            datacenter: datacenter.to_string(),
            lag_ms,
            last_applied_index: last_applied,
            last_check: timestamp,
        };
        self.replicas.insert(node_id, lag);
    }

    /// Check all replicas and generate alerts.
    pub fn check_alerts(&mut self) -> Vec<LagAlert> {
        self.alerts.clear();
        for (_, lag) in &self.replicas {
            if lag.lag_ms >= self.alert_config.critical_ms {
                self.alerts.push(LagAlert::Critical(lag.node_id, lag.lag_ms));
            } else if lag.lag_ms >= self.alert_config.warning_ms {
                self.alerts.push(LagAlert::Warning(lag.node_id, lag.lag_ms));
            }
        }
        self.alerts.clone()
    }

    /// Record max lag in history.
    pub fn record_history(&mut self, timestamp: u64) {
        let max_lag = self.replicas.values().map(|l| l.lag_ms).max().unwrap_or(0);
        if self.lag_history.len() >= self.max_history {
            self.lag_history.pop_front();
        }
        self.lag_history.push_back((timestamp, max_lag));
    }

    /// Average lag across all replicas.
    pub fn avg_lag(&self) -> f64 {
        if self.replicas.is_empty() {
            return 0.0;
        }
        let total: u64 = self.replicas.values().map(|l| l.lag_ms).sum();
        total as f64 / self.replicas.len() as f64
    }

    pub fn max_lag(&self) -> u64 {
        self.replicas.values().map(|l| l.lag_ms).max().unwrap_or(0)
    }

    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }
}

// ── Cluster Topology Discovery ────────────────────────────────────────

/// Node role in the cluster.
#[derive(Debug, Clone, PartialEq)]
pub enum NodeRole {
    Leader,
    Follower,
    Learner,
    Witness,
}

/// A node in the topology.
#[derive(Debug, Clone)]
pub struct TopoNode {
    pub node_id: u64,
    pub address: String,
    pub datacenter: String,
    pub role: NodeRole,
    pub partitions: Vec<u32>,
}

/// Manages cluster topology and partition mapping.
pub struct TopologyDiscovery {
    nodes: HashMap<u64, TopoNode>,
    /// partition_id → node_ids that hold replicas.
    partition_map: HashMap<u32, Vec<u64>>,
    version: u64,
}

impl TopologyDiscovery {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            partition_map: HashMap::new(),
            version: 1,
        }
    }

    /// Register a node.
    pub fn add_node(&mut self, node: TopoNode) {
        let partitions = node.partitions.clone();
        let node_id = node.node_id;
        self.nodes.insert(node_id, node);
        for pid in partitions {
            self.partition_map.entry(pid).or_default().push(node_id);
        }
        self.version += 1;
    }

    /// Remove a node.
    pub fn remove_node(&mut self, node_id: u64) -> bool {
        if let Some(node) = self.nodes.remove(&node_id) {
            for pid in &node.partitions {
                if let Some(nodes) = self.partition_map.get_mut(pid) {
                    nodes.retain(|&id| id != node_id);
                }
            }
            self.version += 1;
            true
        } else {
            false
        }
    }

    /// Find which nodes hold a specific partition.
    pub fn nodes_for_partition(&self, partition_id: u32) -> Vec<u64> {
        self.partition_map.get(&partition_id).cloned().unwrap_or_default()
    }

    /// Find the leader node for a partition.
    pub fn leader_for_partition(&self, partition_id: u32) -> Option<u64> {
        let node_ids = self.nodes_for_partition(partition_id);
        for nid in node_ids {
            if let Some(node) = self.nodes.get(&nid) {
                if node.role == NodeRole::Leader {
                    return Some(nid);
                }
            }
        }
        None
    }

    /// All nodes in a datacenter.
    pub fn nodes_in_dc(&self, dc: &str) -> Vec<u64> {
        self.nodes.values()
            .filter(|n| n.datacenter == dc)
            .map(|n| n.node_id)
            .collect()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn partition_count(&self) -> usize {
        self.partition_map.len()
    }

    pub fn version(&self) -> u64 {
        self.version
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_compaction_basic() {
        let mut lc = LogCompactor::new(100);
        lc.append_entries(50);
        assert!(!lc.needs_compaction());
        lc.append_entries(60);
        assert!(lc.needs_compaction()); // 110 > 100
        let snap = lc.compact(100, 1, 5000);
        assert_eq!(snap.last_included_index, 100);
        assert!(!lc.needs_compaction()); // 110 - 100 = 10 < 100
    }

    #[test]
    fn log_compaction_multiple_snapshots() {
        let mut lc = LogCompactor::new(50);
        lc.append_entries(60);
        lc.compact(55, 1, 1000);
        lc.append_entries(60);
        lc.compact(110, 2, 2000);
        assert_eq!(lc.snapshot_count(), 2);
        assert_eq!(lc.latest_snapshot().unwrap().last_included_term, 2);
    }

    #[test]
    fn membership_add_remove() {
        let mut mc = MembershipChange::new(vec![1, 2, 3].into_iter().collect());
        assert_eq!(mc.member_count(), 3);
        assert_eq!(mc.quorum_size(), 2);

        mc.propose_add(4);
        mc.enter_joint();
        assert_eq!(mc.state(), &MembershipState::Joint);
        mc.commit_change();
        assert_eq!(mc.state(), &MembershipState::Stable);
        assert!(mc.members().contains(&4));
        assert_eq!(mc.member_count(), 4);
    }

    #[test]
    fn membership_remove() {
        let mut mc = MembershipChange::new(vec![1, 2, 3].into_iter().collect());
        mc.propose_remove(3);
        mc.enter_joint();
        mc.commit_change();
        assert!(!mc.members().contains(&3));
        assert_eq!(mc.member_count(), 2);
    }

    #[test]
    fn replication_lag_monitor() {
        let mut mon = ReplicationLagMonitor::new(100, 500);
        mon.update_lag(1, "us-east", 50, 1000, 1);
        mon.update_lag(2, "eu-west", 200, 990, 1);
        mon.update_lag(3, "ap-south", 600, 950, 1);

        let alerts = mon.check_alerts();
        assert!(alerts.iter().any(|a| matches!(a, LagAlert::Warning(2, _))));
        assert!(alerts.iter().any(|a| matches!(a, LagAlert::Critical(3, _))));
        assert_eq!(mon.max_lag(), 600);
    }

    #[test]
    fn replication_avg_lag() {
        let mut mon = ReplicationLagMonitor::new(100, 500);
        mon.update_lag(1, "dc1", 100, 100, 1);
        mon.update_lag(2, "dc2", 200, 100, 1);
        assert!((mon.avg_lag() - 150.0).abs() < 0.01);
    }

    #[test]
    fn topology_discovery_basic() {
        let mut topo = TopologyDiscovery::new();
        topo.add_node(TopoNode {
            node_id: 1,
            address: "10.0.0.1:8000".to_string(),
            datacenter: "us-east".to_string(),
            role: NodeRole::Leader,
            partitions: vec![0, 1],
        });
        topo.add_node(TopoNode {
            node_id: 2,
            address: "10.0.0.2:8000".to_string(),
            datacenter: "eu-west".to_string(),
            role: NodeRole::Follower,
            partitions: vec![0, 1],
        });
        assert_eq!(topo.node_count(), 2);
        assert_eq!(topo.leader_for_partition(0), Some(1));
        assert_eq!(topo.nodes_in_dc("eu-west"), vec![2]);
    }

    #[test]
    fn topology_remove_node() {
        let mut topo = TopologyDiscovery::new();
        topo.add_node(TopoNode {
            node_id: 1,
            address: "a".to_string(),
            datacenter: "dc1".to_string(),
            role: NodeRole::Leader,
            partitions: vec![0],
        });
        assert!(topo.remove_node(1));
        assert_eq!(topo.node_count(), 0);
        assert!(topo.nodes_for_partition(0).is_empty());
    }
}
